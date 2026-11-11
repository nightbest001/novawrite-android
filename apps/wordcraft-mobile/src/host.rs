//! What the phone adds around the shared app: the services the desktop app gets from its operating
//! system (clipboard, printing, default app, animation setting), "Open with NovaWrite", the soft
//! keyboard, finger-sized controls and keeping the MCP bridge awake.
//!
//! The Android calls are in `jni_host.rs`; off Android (a PC `cargo check` or test run) every
//! method here does nothing.

use std::path::{Path, PathBuf};

use wordcraft_ui_egui::{Services, WordApp};

/// Host state kept between frames.
pub struct Host {
    /// Where files other apps hand over are copied (`<documents>/Incoming`).
    documents: PathBuf,
    /// The soft keyboard was asked to show and has not been asked to hide.
    keyboard_up: bool,
    /// The window had focus on the last frame (a return from another app checks for a new file).
    focused: bool,
    /// The wake lock and the screen-on flag are held for the MCP bridge.
    awake: bool,
}

impl Host {
    pub fn new(documents: &Path) -> Self {
        Self { documents: documents.to_path_buf(), keyboard_up: false, focused: true, awake: false }
    }

    /// Fill in the services a phone can offer.
    pub fn install(&self, services: &mut Services) {
        #[cfg(target_os = "android")]
        {
            use std::cell::Cell;
            use std::time::{Duration, Instant};

            use crate::jni_host as j;

            services.clipboard_text = Some(Box::new(j::clipboard_text));
            services.clipboard_html = Some(Box::new(j::clipboard_html));
            services.clipboard_formats = Some(Box::new(j::clipboard_formats));
            services.clipboard_picture = Some(Box::new(j::clipboard_picture));
            services.set_clipboard_html = Some(Box::new(j::set_clipboard_html));
            // Print: the PDF opens in a PDF viewer, whose Print command reaches the system print service.
            services.print = Some(Box::new(j::print_pdf));
            services.make_default_app = Some(Box::new(j::open_default_apps_settings));
            // Asked every frame: answer from memory and look again every few seconds.
            let cache: Cell<(Option<Instant>, Option<bool>)> = Cell::new((None, None));
            services.animations_enabled = Some(Box::new(move || {
                let (at, value) = cache.get();
                if at.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
                    return value;
                }
                let fresh = j::animations_enabled();
                cache.set((Some(Instant::now()), fresh));
                fresh
            }));
        }
        #[cfg(not(target_os = "android"))]
        let _ = services;
    }

    /// The app is closing: let go of the wake lock and the screen-on flag.
    pub fn release(&mut self) {
        self.keep_awake(false);
    }

    /// Finger-sized controls, once. The ribbon, menus and dialogs keep their layout; only the
    /// room around each control grows. Checked every frame because the theme setting rewrites the
    /// spacing when it changes.
    pub fn touch_style(&self, ctx: &egui::Context) {
        #[cfg(target_os = "android")]
        {
            if ctx.global_style().spacing.interact_size.y >= 30.0 {
                return;
            }
            ctx.all_styles_mut(|s| {
                s.spacing.interact_size = egui::vec2(30.0, 30.0);
                s.spacing.item_spacing = egui::vec2(8.0, 6.0);
                s.spacing.button_padding = egui::vec2(9.0, 5.0);
                s.spacing.scroll.bar_width = 12.0;
                // Nothing hovers on a touch screen: a tooltip would only open after a long press.
                s.interaction.tooltip_delay = 0.8;
            });
        }
        #[cfg(not(target_os = "android"))]
        let _ = ctx;
    }

    /// The file another app opened with NovaWrite, if the app was started that way.
    pub fn open_incoming(&self, app: &mut WordApp) {
        #[cfg(target_os = "android")]
        if let Some(path) = crate::jni_host::take_view_file(&self.documents) {
            match app.run("file.open", serde_json::json!({ "path": path.to_string_lossy() })) {
                Ok(_) => {}
                Err(e) => log::warn!("open {}: {e}", path.display()),
            }
        }
        #[cfg(not(target_os = "android"))]
        let _ = (app, &self.documents);
    }

    /// Once per frame, before the interface runs.
    pub fn before_ui(&mut self, ctx: &egui::Context, app: &mut WordApp, mcp_online: bool) {
        self.touch_style(ctx);
        let focused = ctx.input(|i| i.focused);
        if focused && !self.focused {
            // Back from another app: a "Open with" may have brought a file.
            self.open_incoming(app);
        }
        self.focused = focused;
        self.keep_awake(mcp_online);
    }

    /// While the MCP bridge is online: hold a wake lock and keep the screen on, so Android
    /// does not suspend the app mid-task. (A phone that is locked or switched to another app
    /// still pauses it: that needs a foreground service, which a `cargo apk` build cannot ship.)
    fn keep_awake(&mut self, on: bool) {
        if on == self.awake {
            return;
        }
        self.awake = on;
        #[cfg(target_os = "android")]
        {
            use android_activity::WindowManagerFlags;

            crate::jni_host::hold_wake_lock(on);
            if let Some(app) = crate::jni_host::app() {
                let flag = WindowManagerFlags::KEEP_SCREEN_ON;
                if on {
                    app.set_window_flags(flag, WindowManagerFlags::empty());
                } else {
                    app.set_window_flags(WindowManagerFlags::empty(), flag);
                }
            }
        }
    }

    /// Once per frame, after the interface ran: copies reach the system clipboard (egui has none on
    /// Android), and the soft keyboard follows the text caret.
    pub fn after_ui(&mut self, ctx: &egui::Context) {
        #[cfg(target_os = "android")]
        {
            for text in ctx.output(|o| {
                o.commands
                    .iter()
                    .filter_map(|c| match c {
                        egui::output::OutputCommand::CopyText(t) => Some(t.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            }) {
                if !text.is_empty() && !crate::jni_host::set_clipboard_text(&text) {
                    log::warn!("clipboard: the copy did not reach the system clipboard");
                }
            }

            let wants = ctx.egui_wants_keyboard_input() || ctx.output(|o| o.ime.is_some());
            let tapped = ctx.input(|i| i.pointer.any_click());
            if let Some(app) = crate::jni_host::app() {
                if wants && tapped {
                    // A tap on the page or a text field brings the keyboard up (again, if it was dismissed).
                    app.show_soft_input(true);
                    self.keyboard_up = true;
                } else if !wants && self.keyboard_up {
                    app.hide_soft_input(false);
                    self.keyboard_up = false;
                }
            }
        }
        #[cfg(not(target_os = "android"))]
        let _ = (ctx, &mut self.keyboard_up);
    }
}
