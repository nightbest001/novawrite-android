//! Ribbon › MCP: the bridge between this app and an MCP server, so an AI on the web can drive
//! WordCraft the way it drives Blender or Roblox Studio.
//!
//! ```text
//! AI --HTTPS--> <server>/mcp/<CODE> --> server <--long-poll-- this bridge --> the app's UI thread
//! ```
//!
//! The bridge opens no port. It pulls tool calls from `<server>/wordcraft/<CODE>/poll`, runs them
//! as control-channel requests (the same ones `--control` serves, answered by the UI thread) and
//! posts the results to `/wordcraft/<CODE>/result`. The ribbon tab ([`wordcraft_ui_egui::McpShared`])
//! holds the server URL, the secret code and the options; the settings are kept in
//! `<settings>/mcp-bridge.json`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use ureq::Agent;
use wordcraft_ui_egui::{ControlRequest, McpAction, McpHandle, McpShared, McpStatus};

const DEFAULT_URL: &str = "https://plugin-roblox.wisp.uno";
const USER_AGENT: &str = concat!("WordCraft/", env!("CARGO_PKG_VERSION"), " MCPBridge");
/// How long the UI thread gets to answer one request.
const CALL_TIMEOUT: Duration = Duration::from_secs(80);
/// The server holds a poll for 15 s; leave headroom.
const POLL_TIMEOUT: Duration = Duration::from_secs(40);
const POST_TIMEOUT: Duration = Duration::from_secs(60);
/// The server takes a result up to 28 MB; stay under.
const MAX_RESULT_BYTES: usize = 24_000_000;
const MAX_TEXT_CHARS: usize = 200_000;

/// Commands that write files, and commands that read them (what "Allow file access" covers).
const WRITE_CMDS: [&str; 10] = [
    "file.save",
    "file.saveAs",
    "file.exportPdf",
    "file.exportPng",
    "file.printPdf",
    "file.saveTemplate",
    "file.rename",
    "mailings.finish",
    "font.add",
    "font.remove",
];
const READ_CMDS: [&str; 11] = [
    "file.open",
    "file.newFromTemplate",
    "insert.textFromFile",
    "insert.object",
    "insert.picture",
    "picture.change",
    "mailings.recipients",
    "review.compare",
    "tools.updateStyles",
    "styles.copyFrom",
    "styles.organizer",
];

fn lock(h: &McpHandle) -> MutexGuard<'_, McpShared> {
    h.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Load the saved settings (when `settings` is a folder), start the supervisor thread and return
/// what the app needs: the state the ribbon tab shows and the receiver for the AI's requests.
pub fn start(ctx: egui::Context, settings: Option<PathBuf>) -> (McpHandle, Receiver<ControlRequest>) {
    let (tx, rx) = channel();
    let mut s = McpShared { url: DEFAULT_URL.into(), auto_connect: true, allow_files: true, ..McpShared::default() };
    if let Some(dir) = &settings {
        load_config(&mut s, dir);
    }
    let handle: McpHandle = Arc::new(Mutex::new(s));
    let h = handle.clone();
    std::thread::spawn(move || supervise(&h, &ctx, &tx, settings.as_deref()));
    (handle, rx)
}

// ---------------------------------------------------------------- settings

fn config_path(dir: &Path) -> PathBuf {
    dir.join("mcp-bridge.json")
}

fn load_config(s: &mut McpShared, dir: &Path) {
    let Ok(text) = std::fs::read_to_string(config_path(dir)) else { return };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
    if let Some(u) = v.get("url").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) {
        s.url = u.trim().to_string();
    }
    if let Some(c) = v.get("code").and_then(Value::as_str) {
        s.code = c.trim().to_string();
    }
    if let Some(b) = v.get("auto").and_then(Value::as_bool) {
        s.auto_connect = b;
    }
    if let Some(b) = v.get("allow_files").and_then(Value::as_bool) {
        s.allow_files = b;
    }
}

/// Write the settings, keeping any other keys the file has (the Python bridge's `port`, `launch`).
fn save_config(s: &McpShared, dir: &Path) {
    let path = config_path(dir);
    let mut v = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
    let Some(o) = v.as_object_mut() else { return };
    o.insert("url".into(), json!(s.url.trim()));
    o.insert("code".into(), json!(s.code));
    o.insert("auto".into(), json!(s.auto_connect));
    o.insert("allow_files".into(), json!(s.allow_files));
    let write = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, v.to_string()));
    if let Err(e) = write {
        log::warn!("MCP settings not saved ({}): {e}", path.display());
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The code is a secret: owner only.
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

// ---------------------------------------------------------------- one connection

/// What one connection's threads share.
struct Link {
    base: String,
    code: String,
    shared: McpHandle,
    ctx: egui::Context,
    tx: Sender<ControlRequest>,
    stop: Arc<AtomicBool>,
}

struct Session {
    stop: Arc<AtomicBool>,
    base: String,
    code: String,
}

impl Session {
    fn open(shared: &McpHandle, ctx: &egui::Context, tx: &Sender<ControlRequest>) -> Self {
        let (base, code) = {
            let g = lock(shared);
            (g.url.trim().trim_end_matches('/').to_string(), g.code.clone())
        };
        let stop = Arc::new(AtomicBool::new(false));
        let link = Arc::new(Link { base: base.clone(), code: code.clone(), shared: shared.clone(), ctx: ctx.clone(), tx: tx.clone(), stop: stop.clone() });
        let (jobs_tx, jobs_rx) = channel::<Value>();
        let l = link.clone();
        std::thread::spawn(move || poll_loop(&l, &jobs_tx));
        std::thread::spawn(move || worker_loop(&link, &jobs_rx));
        Self { stop, base, code }
    }

    fn finished(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Stop the threads and tell the server this bridge is gone (it also frees a held poll).
    fn close(self) {
        self.stop.store(true, Ordering::Relaxed);
        let url = format!("{}/wordcraft/{}/bye", self.base, self.code);
        let _ = post(&agent(Duration::from_secs(3)), &url, &json!({}));
    }
}

/// Offline, Connecting or Online, with the problem if any; repaints only when something changed.
fn set_status(l: &Link, status: McpStatus, error: Option<String>) {
    // A connection that was closed (Disconnect, a new code) no longer reports.
    if l.stop.load(Ordering::Relaxed) {
        return;
    }
    let changed = {
        let mut g = lock(&l.shared);
        let mut changed = g.status != status;
        g.status = status;
        if let Some(e) = error
            && g.last_error != e
        {
            g.last_error = e;
            changed = true;
        }
        if status == McpStatus::Online && !g.last_error.is_empty() {
            g.last_error.clear();
            changed = true;
        }
        changed
    };
    if changed {
        l.ctx.request_repaint();
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) {
    let end = Instant::now() + total;
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Fetch tool calls from the server and hand them to the worker.
fn poll_loop(l: &Link, jobs: &Sender<Value>) {
    let agent = agent(POLL_TIMEOUT);
    let mut backoff = 1u64;
    let mut info = (String::from("WordCraft"), String::new(), Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(Instant::now));
    set_status(l, McpStatus::Connecting, None);
    while !l.stop.load(Ordering::Relaxed) {
        if info.2.elapsed() > Duration::from_secs(10) {
            info = (format!("WordCraft {}", env!("CARGO_PKG_VERSION")), active_title(l), Instant::now());
        }
        let url = format!("{}/wordcraft/{}/poll?v={}&f={}", l.base, l.code, encode(&info.0), encode(&info.1));
        match agent.get(&url).header("User-Agent", USER_AGENT).header("Accept", "application/json").call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                if status == 400 {
                    set_status(l, McpStatus::Offline, Some("The server refused the code (400). Click Generate MCP for a new one.".into()));
                    l.stop.store(true, Ordering::Relaxed);
                    return;
                }
                if status != 200 && status != 204 {
                    set_status(l, McpStatus::Offline, Some(format!("The server answered HTTP {status}.")));
                    sleep_unless_stopped(&l.stop, Duration::from_secs(backoff.min(15)));
                    backoff = (backoff * 2).min(15);
                    continue;
                }
                backoff = 1;
                set_status(l, McpStatus::Online, None);
                let name = resp.headers().get("X-Client-Name").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                {
                    let mut g = lock(&l.shared);
                    if g.ai_name != name {
                        g.ai_name = name;
                        l.ctx.request_repaint();
                    }
                }
                if status == 200
                    && let Ok(body) = resp.body_mut().read_to_string()
                    && let Ok(cmd) = serde_json::from_str::<Value>(&body)
                    && cmd.get("id").is_some()
                    && jobs.send(cmd).is_err()
                {
                    return;
                }
            }
            Err(e) => {
                set_status(l, McpStatus::Offline, Some(format!("No connection to the server: {e}")));
                sleep_unless_stopped(&l.stop, Duration::from_secs(backoff.min(15)));
                backoff = (backoff * 2).min(15);
            }
        }
    }
}

/// The title of the active document, for the server's status page ("" when there is none).
fn active_title(l: &Link) -> String {
    let Ok(v) = call(l, "engine.execute", json!({"command": "view.switchWindows", "params": {}}), Duration::from_secs(2)) else {
        return String::new();
    };
    let docs = v.get("documents").and_then(Value::as_array);
    docs.and_then(|d| d.iter().find(|x| x.get("active") == Some(&Value::Bool(true))))
        .and_then(|d| d.get("title").and_then(Value::as_str))
        .unwrap_or("")
        .chars()
        .take(80)
        .collect()
}

/// Run each tool call and post its result.
fn worker_loop(l: &Link, jobs: &Receiver<Value>) {
    let agent = agent(POST_TIMEOUT);
    let url = format!("{}/wordcraft/{}/result", l.base, l.code);
    while !l.stop.load(Ordering::Relaxed) {
        let cmd = match jobs.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let id = cmd.get("id").cloned().unwrap_or(Value::Null);
        let tool = cmd.get("tool").and_then(Value::as_str).unwrap_or("");
        let args = cmd.get("args").cloned().unwrap_or_else(|| json!({}));
        let started = Instant::now();
        let payload = match run_tool(l, tool, &args) {
            Ok(v) => fit(&id, v),
            Err(e) => json!({"id": id, "ok": false, "error": e}),
        };
        log::info!("MCP {tool}: {} in {:.2}s", payload["ok"], started.elapsed().as_secs_f32());
        for attempt in 0..3 {
            if post(&agent, &url, &payload).is_ok() {
                break;
            }
            if attempt < 2 {
                sleep_unless_stopped(&l.stop, Duration::from_millis(1500));
            }
        }
    }
}

/// The reply to post: the result, or an error when it is too big to be useful.
fn fit(id: &Value, result: Value) -> Value {
    let image = result.get("image_base64").is_some();
    let size = result.to_string().len();
    if size > MAX_RESULT_BYTES || (size > MAX_TEXT_CHARS && !image) {
        return json!({"id": id, "ok": false, "error": format!("The result is too big ({size} characters). Ask for less: a filter, `limit` or `from_block`.")});
    }
    json!({"id": id, "ok": true, "result": result})
}

// ---------------------------------------------------------------- supervisor

fn supervise(h: &McpHandle, ctx: &egui::Context, tx: &Sender<ControlRequest>, settings: Option<&Path>) {
    let mut session: Option<Session> = None;
    // Without a settings folder (`WORDCRAFT_NO_PREFS`) nothing connects by itself.
    let mut auto_pending = settings.is_some();
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let (actions, changed, snap) = {
            let mut g = lock(h);
            (std::mem::take(&mut g.actions), std::mem::take(&mut g.settings_changed), g.clone())
        };
        if changed && let Some(dir) = settings {
            save_config(&snap, dir);
        }
        if session.as_ref().is_some_and(Session::finished) {
            session = None;
        }
        let mut connect = false;
        for a in actions {
            match a {
                McpAction::Generate => {
                    generate_code(h, ctx, settings);
                    connect = true;
                }
                McpAction::Connect => connect = true,
                McpAction::Disconnect => {
                    if let Some(s) = session.take() {
                        s.close();
                    }
                    lock(h).status = McpStatus::Offline;
                    ctx.request_repaint();
                }
            }
        }
        if auto_pending {
            auto_pending = false;
            connect |= snap.auto_connect && snap.code_valid();
        }
        if connect {
            if let Some(s) = session.take() {
                s.close();
            }
            if lock(h).code_valid() {
                session = Some(Session::open(h, ctx, tx));
            } else {
                let mut g = lock(h);
                g.status = McpStatus::Offline;
                g.last_error = "The code must be 16 to 64 letters or digits. Click Generate MCP.".into();
                ctx.request_repaint();
            }
        }
    }
}

/// A new random code, saved, and the MCP URL queued for the clipboard.
fn generate_code(h: &McpHandle, ctx: &egui::Context, settings: Option<&Path>) {
    let mut b = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut b) {
        let mut g = lock(h);
        g.last_error = format!("No random numbers for a code: {e}");
        ctx.request_repaint();
        return;
    }
    let snap = {
        let mut g = lock(h);
        g.code = b.iter().map(|x| format!("{x:02x}")).collect();
        g.copy_url = true;
        g.clone()
    };
    if let Some(dir) = settings {
        save_config(&snap, dir);
    }
    ctx.request_repaint();
}

// ---------------------------------------------------------------- http

fn agent(timeout: Duration) -> Agent {
    // A 4xx or 5xx answer is an answer, not an error: the poll loop reads the status itself.
    let config = Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build();
    Agent::new_with_config(config)
}

fn post(agent: &Agent, url: &str, payload: &Value) -> Result<(), String> {
    let body = payload.to_string();
    agent.post(url).header("Content-Type", "application/json").header("User-Agent", USER_AGENT).send(&body).map(|_| ()).map_err(|e| e.to_string())
}

/// Percent-encode a query value.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------- calls into the app

/// One control-channel method, answered by the UI thread.
fn call(l: &Link, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
    let (req, rx) = ControlRequest::new(method, params);
    l.tx.send(req).map_err(|_| "The WordCraft window is closed.".to_string())?;
    l.ctx.request_repaint();
    let reply = rx.recv_timeout(wait).map_err(|_| "WordCraft did not answer in time.".to_string())?;
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(reply.get("error").and_then(Value::as_str).unwrap_or("unknown error").to_string())
    }
}

fn app(l: &Link, method: &str, params: Value) -> Result<Value, String> {
    call(l, method, params, CALL_TIMEOUT)
}

/// `engine.execute`: a command with params, on document `doc` or the active one.
fn exe(l: &Link, command: &str, params: Value, doc: Option<u64>) -> Result<Value, String> {
    let mut p = json!({"command": command, "params": params});
    if let Some(d) = doc {
        p["doc"] = json!(d);
    }
    app(l, "engine.execute", p)
}

fn files_allowed(l: &Link) -> bool {
    lock(&l.shared).allow_files
}

const FILES_OFF: &str = "File access is off: tick Allow file access in WordCraft's MCP tab.";

/// Refuse a command that reads or writes files while file access is off.
fn guard_command(l: &Link, command: &str) -> Result<(), String> {
    if (WRITE_CMDS.contains(&command) || READ_CMDS.contains(&command)) && !files_allowed(l) {
        return Err(FILES_OFF.into());
    }
    Ok(())
}

fn doc_of(a: &Value) -> Option<u64> {
    a.get("doc").and_then(Value::as_u64)
}

fn text_of<'a>(a: &'a Value, key: &str) -> &'a str {
    a.get(key).and_then(Value::as_str).unwrap_or("")
}

/// A reply that opened or made a document, with that document's id added when it lacks one.
fn with_active_id(l: &Link, mut r: Value) -> Value {
    if r.get("id").is_none()
        && let Ok(list) = exe(l, "view.switchWindows", json!({}), None)
        && let Some(id) = list
            .get("documents")
            .and_then(Value::as_array)
            .and_then(|d| d.iter().find(|x| x.get("active") == Some(&Value::Bool(true))))
            .and_then(|d| d.get("id").cloned())
        && let Some(o) = r.as_object_mut()
    {
        o.insert("id".into(), id);
    }
    r
}

/// A picture reply (`png` in base64) as the server's image result.
fn image_of(v: &Value, keys: &[&str]) -> Result<Value, String> {
    let png = v.get("png").and_then(Value::as_str).filter(|p| !p.is_empty()).ok_or("The reply carried no picture.")?;
    let mut out = json!({"image_base64": png, "mime_type": "image/png"});
    for k in keys {
        if let Some(x) = v.get(*k) {
            out[*k] = x.clone();
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- tools

fn run_tool(l: &Link, tool: &str, a: &Value) -> Result<Value, String> {
    match tool {
        "list_documents" => exe(l, "view.switchWindows", json!({}), None),
        "new_document" => {
            let template = a.get("template").and_then(Value::as_str).unwrap_or("blank");
            exe(l, "file.new", json!({"template": template}), None).map(|r| with_active_id(l, r))
        }
        "open_document" => open_document(l, a),
        "import_markdown" => import_markdown(l, a),
        "get_text" => exe(l, "document.text", json!({}), doc_of(a)),
        "inspect_document" => inspect_document(l, a),
        "select_text" => {
            let occurrence = a.get("occurrence").cloned().unwrap_or_else(|| json!(1));
            exe(l, "select.text", json!({"text": text_of(a, "text"), "occurrence": occurrence}), doc_of(a))
        }
        "type_text" => {
            let mut p = json!({"text": text_of(a, "text"), "raw": true});
            if a.get("paragraphs").and_then(Value::as_bool) == Some(true) {
                p["paragraphs"] = json!(true);
            }
            exe(l, "text.insert", p, doc_of(a))
        }
        "list_commands" => list_commands(l, a),
        "execute" => execute(l, a),
        "batch" => batch(l, a),
        "save_document" => save_document(l, a),
        "render_page" => {
            let mut p = json!({"page": a.get("page").cloned().unwrap_or_else(|| json!(1)), "scale": a.get("scale").cloned().unwrap_or_else(|| json!(1.0))});
            if let Some(d) = doc_of(a) {
                p["doc"] = json!(d);
            }
            image_of(&app(l, "ui.render", p)?, &["width", "height", "pages"])
        }
        "screenshot" => image_of(&app(l, "ui.screenshot", json!({}))?, &["width", "height"]),
        "ui_inspect" => app(l, "ui.inspect", json!({})),
        "click" => app(l, "ui.click", pick(a, &["x", "y", "count", "shift", "cmd"])),
        "key" => app(l, "ui.key", pick(a, &["key", "shift", "alt", "cmd"])),
        other => Err(format!("Unknown tool: {other}")),
    }
}

/// The listed keys of `a`, as new params.
fn pick(a: &Value, keys: &[&str]) -> Value {
    let mut o = serde_json::Map::new();
    for k in keys {
        if let Some(v) = a.get(*k) {
            o.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(o)
}

fn open_document(l: &Link, a: &Value) -> Result<Value, String> {
    let path = text_of(a, "path");
    if path.is_empty() {
        return Err("`path` is required.".into());
    }
    if !files_allowed(l) {
        return Err(FILES_OFF.into());
    }
    exe(l, "file.open", json!({"path": path, "password": a.get("password").cloned().unwrap_or(Value::Null)}), None)
}

fn execute(l: &Link, a: &Value) -> Result<Value, String> {
    let command = text_of(a, "command");
    if command.is_empty() {
        return Err("`command` is required.".into());
    }
    guard_command(l, command)?;
    exe(l, command, a.get("params").cloned().unwrap_or_else(|| json!({})), doc_of(a))
}

fn batch(l: &Link, a: &Value) -> Result<Value, String> {
    let commands = a.get("commands").and_then(Value::as_array).filter(|c| !c.is_empty()).ok_or("`commands` must list the commands to run.")?;
    let keep_going = a.get("keepGoing").and_then(Value::as_bool) == Some(true);
    // Check them all first: when one is refused, none runs.
    for c in commands {
        guard_command(l, text_of(c, "command"))?;
    }
    let mut results = Vec::new();
    for c in commands {
        let id = text_of(c, "command");
        match exe(l, id, c.get("params").cloned().unwrap_or_else(|| json!({})), doc_of(a)) {
            Ok(r) => results.push(json!({"command": id, "ok": true, "result": r})),
            Err(e) => {
                results.push(json!({"command": id, "ok": false, "error": e}));
                if !keep_going {
                    let stopped_at = results.len() - 1;
                    return Ok(json!({"ok": false, "results": results, "stopped_at": stopped_at}));
                }
            }
        }
    }
    let all_ok = results.iter().all(|r| r["ok"] == json!(true));
    Ok(json!({"ok": all_ok, "results": results}))
}

fn save_document(l: &Link, a: &Value) -> Result<Value, String> {
    if !files_allowed(l) {
        return Err(FILES_OFF.into());
    }
    let path = a.get("path").and_then(Value::as_str).filter(|p| !p.is_empty());
    match path {
        Some(p) if p.to_ascii_lowercase().ends_with(".png") => exe(l, "file.exportPng", json!({"path": p}), doc_of(a)),
        Some(p) => exe(l, "file.save", json!({"path": p}), doc_of(a)),
        None => exe(l, "file.save", json!({}), doc_of(a)),
    }
}

fn inspect_document(l: &Link, a: &Value) -> Result<Value, String> {
    let mut p = json!({});
    if let Some(d) = doc_of(a) {
        p["doc"] = json!(d);
    }
    let mut res = app(l, "document.inspect", p)?;
    let Some(blocks) = res.get("blocks").and_then(Value::as_array).cloned() else { return Ok(res) };
    let from = a.get("from_block").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = a.get("limit").and_then(Value::as_u64).map(|n| n as usize);
    let mut slice: Vec<Value> = blocks.iter().skip(from).take(limit.unwrap_or(usize::MAX)).cloned().collect();
    // No limit asked for: cut a big document down to what fits, and say how to read on.
    while limit.is_none() && slice.len() > 1 && Value::Array(slice.clone()).to_string().len() > MAX_TEXT_CHARS / 2 {
        slice.truncate((slice.len() / 2).max(1));
    }
    if slice.len() != blocks.len() {
        let returned = slice.len();
        if let Some(o) = res.as_object_mut() {
            o.insert("blocks".into(), Value::Array(slice));
            o.insert("blocks_total".into(), json!(blocks.len()));
            o.insert("blocks_from".into(), json!(from));
            o.insert("blocks_returned".into(), json!(returned));
            if from + returned < blocks.len() {
                o.insert("note".into(), json!(format!("Cut short. Continue with from_block={}.", from + returned)));
            }
        }
    }
    Ok(res)
}

fn list_commands(l: &Link, a: &Value) -> Result<Value, String> {
    let query = text_of(a, "query").to_lowercase();
    let limit = a.get("limit").and_then(Value::as_u64).unwrap_or(60).clamp(1, 500) as usize;
    let compact = a.get("compact").and_then(Value::as_bool).unwrap_or(true);
    let all = app(l, "engine.commands", json!({}))?;
    let Some(list) = all.as_array() else { return Ok(all) };
    let hits: Vec<&Value> = list
        .iter()
        .filter(|c| query.is_empty() || ["id", "label", "location"].iter().any(|k| c.get(*k).and_then(Value::as_str).is_some_and(|x| x.to_lowercase().contains(&query))))
        .collect();
    let shown: Vec<Value> = hits
        .iter()
        .take(limit)
        .map(|c| if compact { Value::String(command_row(c)) } else { (*c).clone() })
        .collect();
    Ok(json!({"total_match": hits.len(), "shown": shown.len(), "commands": shown}))
}

/// `id | label | location | params` for one command.
fn command_row(c: &Value) -> String {
    let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
    let mut row = format!("{} | {} | {}", field("id"), field("label"), field("location"));
    if !field("params").is_empty() {
        row.push_str(" | params: ");
        row.push_str(field("params"));
    }
    row
}

/// Write the whole document as Markdown (or HTML), open it and, with `save_as`, save it in the
/// format the name asks for.
fn import_markdown(l: &Link, a: &Value) -> Result<Value, String> {
    let text = text_of(a, "markdown");
    if text.trim().is_empty() {
        return Err("`markdown` is required.".into());
    }
    let ext = match text_of(a, "format").to_lowercase().as_str() {
        "" | "md" => "md",
        "html" => "html",
        _ => return Err("`format` must be md or html.".into()),
    };
    let save_as = a.get("save_as").and_then(Value::as_str).filter(|p| !p.is_empty());
    if save_as.is_some() && !files_allowed(l) {
        return Err(FILES_OFF.into());
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("wordcraft-mcp-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("No temporary folder: {e}"))?;
    let file = dir.join(format!("document.{ext}"));
    let opened = std::fs::write(&file, text).map_err(|e| format!("Could not write the temporary file: {e}")).and_then(|()| {
        exe(l, "file.open", json!({"path": file.to_string_lossy()}), None)
    });
    let _ = std::fs::remove_dir_all(&dir);
    let mut res = with_active_id(l, opened?);
    let doc = res.get("id").and_then(Value::as_u64);
    match save_as {
        Some(p) => res["saved"] = exe(l, "file.saveAs", json!({"path": p}), doc)?,
        None => res["note"] = json!("The document is open but not saved to a file. Save it with save_document and a `path`."),
    }
    Ok(res)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("WordCraft 0.15.1"), "WordCraft%200.15.1");
        assert_eq!(encode("Laporan é"), "Laporan%20%C3%A9");
    }

    #[test]
    fn settings_survive_a_save_and_a_load_and_keep_other_keys() {
        let dir = std::env::temp_dir().join(format!("wc-mcp-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(config_path(&dir), r#"{"port": 7981, "launch": "wordcraft"}"#).unwrap();
        let s = McpShared { url: "https://x.test".into(), code: "abcdef0123456789abcdef0123456789".into(), auto_connect: false, allow_files: false, ..McpShared::default() };
        save_config(&s, &dir);
        let mut back = McpShared::default();
        load_config(&mut back, &dir);
        assert_eq!((back.url.as_str(), back.code.as_str(), back.auto_connect, back.allow_files), ("https://x.test", "abcdef0123456789abcdef0123456789", false, false));
        let kept: Value = serde_json::from_str(&std::fs::read_to_string(config_path(&dir)).unwrap()).unwrap();
        assert_eq!(kept["port"], json!(7981));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_big_result_is_an_error_but_a_picture_is_not() {
        let big = json!({"text": "x".repeat(MAX_TEXT_CHARS + 1)});
        assert_eq!(fit(&json!("1"), big)["ok"], json!(false));
        let pic = json!({"image_base64": "x".repeat(MAX_TEXT_CHARS + 1)});
        assert_eq!(fit(&json!("1"), pic)["ok"], json!(true));
    }

    #[test]
    fn a_command_row_lists_id_label_location_and_params() {
        let c = json!({"id": "insert.table", "label": "Table", "location": "Insert", "params": "{rows, cols}"});
        assert_eq!(command_row(&c), "insert.table | Table | Insert | params: {rows, cols}");
    }
}
