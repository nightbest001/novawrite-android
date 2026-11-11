//! NovaWrite: WordCraft on Android.
//!
//! The [`wordcraft_ui_egui::WordApp`] run by eframe's native runner on a phone. This repo has no PC
//! app: what the desktop app gets from its operating system, the phone gets here.
//!
//! - No system file dialog. The host answers every dialog with an error, so the app shows its own
//!   [file browser](wordcraft_ui_egui), which starts in the app's `Documents` folder (the current
//!   folder: `HOME` is not set on a phone).
//! - Settings (`ui.json`, `mcp-bridge.json`, recovery files, templates) live in the app's private
//!   data folder, handed over by the platform entry point (`android.rs`).
//! - The ribbon starts collapsed on a first run, to leave room for the page.
//! - Ribbon › MCP is there: the app long-polls the MCP server and runs the AI's tool calls itself
//!   (`mcp_bridge.rs`).
//! - `host.rs` and `jni_host.rs` add what a PC gets from its OS: the system clipboard (text, HTML,
//!   pictures), Print (through a PDF viewer), the default-app button, "Open with NovaWrite",
//!   the soft keyboard, finger-sized controls and a wake lock while the MCP bridge is online.
//! - The page scrolls under a finger (`crates/ui-egui/src/canvas/input.rs`).
//!
//! Not here: a foreground service that keeps the MCP bridge alive while the app is not on screen
//! (it needs Java code, which a `cargo apk` build cannot ship), and input-method composition
//! (NativeActivity delivers key events only).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod android;

mod host;
#[cfg(target_os = "android")]
mod jni_host;
mod mcp_bridge;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use wordcraft_engine::Session;
use wordcraft_ui_egui::{FileDialogAnswer, FileDialogRequest, McpHandle, McpStatus, Services, UiState, WordApp};

/// Where the app keeps things, chosen by the platform entry point.
pub struct Paths {
    /// Preferences, MCP settings (the secret code is in `mcp-bridge.json`), recovery, templates.
    /// Private to the app.
    pub settings: PathBuf,
    /// Where the built-in file browser starts, and where the AI's `file.save` lands by default.
    pub documents: PathBuf,
}

/// Run the app until it is closed. `options` carries the platform's handle (`android_app` on
/// Android).
pub fn run(options: eframe::NativeOptions, paths: Paths) -> eframe::Result {
    for dir in [&paths.settings, &paths.documents] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            log::warn!("cannot create {}: {e}", dir.display());
        }
    }
    // The file browser starts in the home folder, else in the current folder.
    if let Err(e) = std::env::set_current_dir(&paths.documents) {
        log::warn!("cannot enter {}: {e}", paths.documents.display());
    }
    eframe::run_native("NovaWrite", options, Box::new(move |cc| Ok(Box::new(Shell::new(cc, paths)))))
}

/// What the app asks the host for. There is no system file dialog on a phone, so each request is
/// answered with an error at once, which makes the app show its built-in browser instead (#576).
fn services(settings: &Path, host: &host::Host) -> Services {
    let mut services = Services {
        file_dialog: Some(Box::new(|_req: FileDialogRequest| {
            let (tx, rx) = channel::<FileDialogAnswer>();
            let _ = tx.send(Err("no system file dialog on this device".into()));
            rx
        })),
        templates_dir: Some(settings.join("Templates")),
        ..Default::default()
    };
    // The system clipboard, Print, the default-app button and the animation setting.
    host.install(&mut services);
    services
}

/// How often changed preferences are written.
const PREFS_EVERY: Duration = Duration::from_secs(2);

struct Shell {
    app: WordApp,
    host: host::Host,
    mcp: McpHandle,
    prefs_path: PathBuf,
    /// The preferences as last written (or read).
    prefs_json: Vec<u8>,
    prefs_checked: Instant,
}

impl Shell {
    fn new(cc: &eframe::CreationContext<'_>, paths: Paths) -> Self {
        let region = wordcraft_ui_egui::i18n::system_region();
        let mut session = Session::new(wordcraft_engine::blank_document(&region));
        session.system_region = region;
        let host = host::Host::new(&paths.documents);
        let mut app = WordApp::new(session, services(&paths.settings, &host));

        let prefs_path = paths.settings.join("ui.json");
        let saved = std::fs::read(&prefs_path).ok();
        let restored = saved.as_deref().and_then(UiState::from_json);
        let first_run = restored.is_none();
        if let Some(ui) = restored {
            app.apply_prefs(ui);
        }
        // A phone's screen is small: the first run opens with the ribbon folded away (a tap on a
        // tab brings it back).
        if first_run {
            app.ui.ribbon_collapsed = true;
        }
        // A region chosen in File › Options replaces the system's for the first blank document too.
        if app.session.prefs.region.is_some() {
            let doc = app.session.blank_document();
            app.replace_startup_document(doc);
        }
        let ctx = cc.egui_ctx.clone();
        if let Err(e) = app.session.enable_recovery(paths.settings.join("recovery"), Arc::new(move || ctx.request_repaint())) {
            log::warn!("local recovery unavailable: {e}");
        }
        // Ribbon › MCP: settings in `mcp-bridge.json`.
        let (mcp, mcp_rx) = mcp_bridge::start(cc.egui_ctx.clone(), Some(paths.settings.clone()));
        let mcp_status = mcp.clone();
        app = app.with_mcp(mcp, mcp_rx);
        app.start_screen();
        // Started with "Open with NovaWrite": open that file instead of the start screen's blank page.
        host.open_incoming(&mut app);
        host.touch_style(&cc.egui_ctx);
        Self { app, host, mcp: mcp_status, prefs_path, prefs_json: saved.unwrap_or_default(), prefs_checked: Instant::now() }
    }

    /// Write the preferences when they changed: a phone may kill the app without running
    /// `on_exit`, so this is not left to the exit path. `force` skips the 2 s wait.
    fn save_prefs(&mut self, force: bool) {
        if !force && self.prefs_checked.elapsed() < PREFS_EVERY {
            return;
        }
        self.prefs_checked = Instant::now();
        let bytes = match serde_json::to_vec_pretty(&self.app.prefs()) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("preferences: cannot serialize: {e}");
                return;
            }
        };
        if bytes == self.prefs_json {
            return;
        }
        // A temporary file renamed over the old one: an interrupted write keeps the old file.
        let tmp = self.prefs_path.with_extension("json.tmp");
        let written = std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &self.prefs_path));
        match written {
            Ok(()) => self.prefs_json = bytes,
            Err(e) => {
                log::warn!("preferences: cannot save {}: {e}", self.prefs_path.display());
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        let online = wordcraft_ui_egui::mcp::lock(&self.mcp).status == McpStatus::Online;
        self.host.before_ui(ctx, &mut self.app, online);
        self.save_prefs(false);
        if self.app.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        self.host.after_ui(ui.ctx());
    }

    fn on_exit(&mut self) {
        self.host.release();
        self.app.session.finish_recovery();
        self.save_prefs(true);
    }
}
