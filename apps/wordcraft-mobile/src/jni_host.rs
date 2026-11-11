//! Calls into the Android framework through JNI: the things the desktop app gets from its
//! operating system and a phone only offers through Java.
//!
//! - the system clipboard: text, HTML, pictures and the names of the formats (`Services::clipboard_*`),
//!   and a formatted copy (`Services::set_clipboard_html`);
//! - Print: the PDF goes to the public Downloads folder through MediaStore and opens in the PDF
//!   viewer the user picks, whose own Print command reaches the system print service
//!   (`Services::print`). Writing a `PrintDocumentAdapter` would need Java code, which a
//!   `cargo apk` build has no place for;
//! - "make default app" opens Android's Default apps settings (`Services::make_default_app`);
//! - "Open with NovaWrite": the file another app handed over (`take_view_file`);
//! - a partial wake lock while the MCP bridge is online;
//! - the system's animation scale (`Services::animations_enabled`).
//!
//! Every function runs on the app's own thread, attached to the JVM, and returns an error (or
//! `None`) instead of panicking: a Java exception is cleared on the spot so one failed call never
//! takes the app down.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use android_activity::AndroidApp;
use jni::objects::{GlobalRef, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use wordcraft_engine::cmd::paste::MAX_PASTE_BYTES;
use wordcraft_ui_egui::DefaultApp;
use wordcraft_ui_egui::paste_picture::{self, ClipboardPicture};

type Res<T> = Result<T, String>;
type Jr<T> = jni::errors::Result<T>;

struct Ctx {
    vm: JavaVM,
    activity: GlobalRef,
    app: AndroidApp,
}

static CTX: OnceLock<Ctx> = OnceLock::new();

/// Called once by `android_main`, before the window opens.
pub fn init(vm: JavaVM, activity: GlobalRef, app: AndroidApp) {
    let _ = CTX.set(Ctx { vm, activity, app });
}

fn ctx() -> Res<&'static Ctx> {
    CTX.get().ok_or_else(|| "the Android host is not ready".to_string())
}

/// The app handle (soft keyboard, window flags).
pub fn app() -> Option<&'static AndroidApp> {
    CTX.get().map(|c| &c.app)
}

/// Run `f` with a JNI environment attached to this thread and the activity. Local references made
/// inside are freed when it returns; a pending Java exception is cleared.
fn with_env<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> Jr<T>) -> Res<T> {
    let c = ctx()?;
    let mut env = c.vm.attach_current_thread().map_err(|e| format!("JNI attach: {e}"))?;
    let activity = c.activity.as_obj();
    let out = env.with_local_frame(96, |env| f(env, activity));
    if out.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    out.map_err(|e| e.to_string())
}

/// A Java string as a Rust one (`None` for null).
fn jstr(env: &mut JNIEnv, s: &JObject) -> Jr<Option<String>> {
    if s.is_null() {
        return Ok(None);
    }
    let local = env.new_local_ref(s)?;
    let js = JString::from(local);
    let text: String = env.get_string(&js)?.into();
    Ok(Some(text))
}

/// `obj.toString()`.
fn to_string(env: &mut JNIEnv, obj: &JObject) -> Jr<Option<String>> {
    if obj.is_null() {
        return Ok(None);
    }
    let s = env.call_method(obj, "toString", "()Ljava/lang/String;", &[])?.l()?;
    jstr(env, &s)
}

fn service<'l>(env: &mut JNIEnv<'l>, activity: &JObject, name: &str) -> Jr<JObject<'l>> {
    let n = env.new_string(name)?;
    env.call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&n)])?.l()
}

fn content_resolver<'l>(env: &mut JNIEnv<'l>, activity: &JObject) -> Jr<JObject<'l>> {
    env.call_method(activity, "getContentResolver", "()Landroid/content/ContentResolver;", &[])?.l()
}

/// The Android API level.
fn sdk_int(env: &mut JNIEnv) -> Jr<i32> {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()
}

/// All bytes of an `InputStream`, at most `cap`; `None` when it holds more. Closes the stream.
fn read_stream(env: &mut JNIEnv, stream: &JObject, cap: usize) -> Jr<Option<Vec<u8>>> {
    const CHUNK: usize = 64 * 1024;
    let buf = env.new_byte_array(CHUNK as i32)?;
    let mut out: Vec<u8> = Vec::new();
    let mut tmp = vec![0i8; CHUNK];
    let mut too_big = false;
    loop {
        let n = env.call_method(stream, "read", "([B)I", &[JValue::Object(&buf)])?.i()?;
        if n < 0 {
            break;
        }
        let n = (n as usize).min(CHUNK);
        env.get_byte_array_region(&buf, 0, &mut tmp[..n])?;
        if out.len().saturating_add(n) > cap {
            too_big = true;
            break;
        }
        out.extend(tmp[..n].iter().map(|b| *b as u8));
    }
    let _ = env.call_method(stream, "close", "()V", &[]);
    Ok((!too_big).then_some(out))
}

/// Write `bytes` to an `OutputStream` and close it.
fn write_stream(env: &mut JNIEnv, stream: &JObject, bytes: &[u8]) -> Jr<()> {
    const CHUNK: usize = 64 * 1024;
    for part in bytes.chunks(CHUNK) {
        let arr = env.new_byte_array(part.len() as i32)?;
        let signed: Vec<i8> = part.iter().map(|b| *b as i8).collect();
        env.set_byte_array_region(&arr, 0, &signed)?;
        env.call_method(stream, "write", "([B)V", &[JValue::Object(&arr)])?;
    }
    env.call_method(stream, "flush", "()V", &[])?;
    env.call_method(stream, "close", "()V", &[])?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------------------------

/// The first item of the primary clip, if there is one.
fn first_item<'l>(env: &mut JNIEnv<'l>, activity: &JObject) -> Jr<Option<(JObject<'l>, JObject<'l>)>> {
    let cm = service(env, activity, "clipboard")?;
    if cm.is_null() || !env.call_method(&cm, "hasPrimaryClip", "()Z", &[])?.z()? {
        return Ok(None);
    }
    let clip = env.call_method(&cm, "getPrimaryClip", "()Landroid/content/ClipData;", &[])?.l()?;
    if clip.is_null() || env.call_method(&clip, "getItemCount", "()I", &[])?.i()? < 1 {
        return Ok(None);
    }
    let item = env.call_method(&clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(0)])?.l()?;
    Ok((!item.is_null()).then_some((clip, item)))
}

/// The clipboard's text.
pub fn clipboard_text() -> Option<String> {
    with_env(|env, act| {
        let Some((_, item)) = first_item(env, act)? else { return Ok(None) };
        let text = env
            .call_method(&item, "coerceToText", "(Landroid/content/Context;)Ljava/lang/CharSequence;", &[JValue::Object(act)])?
            .l()?;
        to_string(env, &text)
    })
    .ok()
    .flatten()
    .filter(|t| !t.is_empty() && t.len() <= MAX_PASTE_BYTES)
}

/// The clipboard's HTML: another app's formatted copy.
pub fn clipboard_html() -> Option<String> {
    with_env(|env, act| {
        let Some((_, item)) = first_item(env, act)? else { return Ok(None) };
        let html = env.call_method(&item, "getHtmlText", "()Ljava/lang/String;", &[])?.l()?;
        jstr(env, &html)
    })
    .ok()
    .flatten()
    .filter(|h| !h.is_empty() && h.len() <= MAX_PASTE_BYTES)
}

/// The MIME types on the clipboard, for a paste that finds nothing it can use.
pub fn clipboard_formats() -> Vec<String> {
    with_env(|env, act| {
        let Some((clip, _)) = first_item(env, act)? else { return Ok(Vec::new()) };
        let desc = env.call_method(&clip, "getDescription", "()Landroid/content/ClipDescription;", &[])?.l()?;
        let count = env.call_method(&desc, "getMimeTypeCount", "()I", &[])?.i()?;
        let mut out = Vec::new();
        for i in 0..count.min(32) {
            let m = env.call_method(&desc, "getMimeType", "(I)Ljava/lang/String;", &[JValue::Int(i)])?.l()?;
            if let Some(s) = jstr(env, &m)? {
                out.push(s);
            }
        }
        Ok(out)
    })
    .unwrap_or_default()
}

/// The picture on the clipboard: a screenshot or "Copy image", which Android hands over as a
/// content URI.
pub fn clipboard_picture() -> Option<ClipboardPicture> {
    let read = with_env(|env, act| {
        let Some((clip, item)) = first_item(env, act)? else { return Ok(None) };
        let desc = env.call_method(&clip, "getDescription", "()Landroid/content/ClipDescription;", &[])?.l()?;
        let mime = env.new_string("image/*")?;
        if !env.call_method(&desc, "hasMimeType", "(Ljava/lang/String;)Z", &[JValue::Object(&mime)])?.z()? {
            return Ok(None);
        }
        let uri = env.call_method(&item, "getUri", "()Landroid/net/Uri;", &[])?.l()?;
        if uri.is_null() {
            return Ok(None);
        }
        let cr = content_resolver(env, act)?;
        let stream = env.call_method(&cr, "openInputStream", "(Landroid/net/Uri;)Ljava/io/InputStream;", &[JValue::Object(&uri)])?.l()?;
        if stream.is_null() {
            return Ok(Some(Err("the clipboard picture couldn't be opened".to_string())));
        }
        Ok(Some(match read_stream(env, &stream, paste_picture::MAX_FILE_BYTES as usize)? {
            Some(bytes) => Ok(bytes),
            None => Err("the clipboard picture is too large to paste".to_string()),
        }))
    });
    match read {
        Ok(Some(Ok(bytes))) if !bytes.is_empty() => Some(paste_picture::encoded_picture(bytes)),
        Ok(Some(Err(why))) => Some(ClipboardPicture::Unreadable(why)),
        Ok(_) => None,
        Err(e) => {
            log::warn!("clipboard picture: {e}");
            Some(ClipboardPicture::Unreadable(format!("the system clipboard couldn't be read ({e})")))
        }
    }
}

/// Put `html` on the clipboard with `text` as its plain alternative. `true` when it is there.
/// `mathml` has no Android format: the HTML carries the equation.
pub fn set_clipboard_html(html: &str, text: &str, _mathml: &str) -> bool {
    set_clip(Some(html), text)
}

/// Put plain `text` on the clipboard.
pub fn set_clipboard_text(text: &str) -> bool {
    set_clip(None, text)
}

fn set_clip(html: Option<&str>, text: &str) -> bool {
    let done = with_env(|env, act| {
        let cm = service(env, act, "clipboard")?;
        if cm.is_null() {
            return Ok(false);
        }
        let label = env.new_string("NovaWrite")?;
        let body = env.new_string(text)?;
        let clip = match html {
            Some(h) => {
                let html = env.new_string(h)?;
                env.call_static_method(
                    "android/content/ClipData",
                    "newHtmlText",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;Ljava/lang/String;)Landroid/content/ClipData;",
                    &[JValue::Object(&label), JValue::Object(&body), JValue::Object(&html)],
                )?
                .l()?
            }
            None => env
                .call_static_method(
                    "android/content/ClipData",
                    "newPlainText",
                    "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                    &[JValue::Object(&label), JValue::Object(&body)],
                )?
                .l()?,
        };
        env.call_method(&cm, "setPrimaryClip", "(Landroid/content/ClipData;)V", &[JValue::Object(&clip)])?;
        Ok(true)
    });
    match done {
        Ok(ok) => ok,
        Err(e) => {
            log::warn!("clipboard: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Print
// ---------------------------------------------------------------------------------------------

/// `Intent.FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK`.
const VIEW_FLAGS: i32 = 0x0000_0001 | 0x1000_0000;

/// Hand a PDF to the system: save it in Downloads/NovaWrite (MediaStore, Android 10 and newer: no
/// permission needed) and open it in the viewer the user picks, whose Print command prints it.
pub fn print_pdf(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("there is nothing to print".into());
    }
    with_env(|env, act| {
        if sdk_int(env)? < 29 {
            return Ok(Err("printing needs Android 10 or newer; use File › Export › PDF and print from a PDF app".to_string()));
        }
        let cr = content_resolver(env, act)?;
        let values = env.new_object("android/content/ContentValues", "()V", &[])?;
        let name = env.new_string(format!("NovaWrite-print-{}.pdf", unix_seconds()))?;
        put_str(env, &values, "_display_name", &name)?;
        let mime = env.new_string("application/pdf")?;
        put_str(env, &values, "mime_type", &mime)?;
        let rel = env.new_string("Download/NovaWrite")?;
        put_str(env, &values, "relative_path", &rel)?;
        put_int(env, &values, "is_pending", 1)?;

        let collection = env
            .get_static_field("android/provider/MediaStore$Downloads", "EXTERNAL_CONTENT_URI", "Landroid/net/Uri;")?
            .l()?;
        let uri = env
            .call_method(
                &cr,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection), JValue::Object(&values)],
            )?
            .l()?;
        if uri.is_null() {
            return Ok(Err("Android would not create the PDF in Downloads".to_string()));
        }
        let mode = env.new_string("w")?;
        let out = env
            .call_method(
                &cr,
                "openOutputStream",
                "(Landroid/net/Uri;Ljava/lang/String;)Ljava/io/OutputStream;",
                &[JValue::Object(&uri), JValue::Object(&mode)],
            )?
            .l()?;
        if out.is_null() {
            return Ok(Err("Android would not open the PDF for writing".to_string()));
        }
        write_stream(env, &out, bytes)?;

        // Done writing: make it visible to other apps.
        let done = env.new_object("android/content/ContentValues", "()V", &[])?;
        put_int(env, &done, "is_pending", 0)?;
        let null = JObject::null();
        env.call_method(
            &cr,
            "update",
            "(Landroid/net/Uri;Landroid/content/ContentValues;Ljava/lang/String;[Ljava/lang/String;)I",
            &[JValue::Object(&uri), JValue::Object(&done), JValue::Object(&null), JValue::Object(&null)],
        )?;

        let view = new_intent(env, "android.intent.action.VIEW")?;
        env.call_method(
            &view,
            "setDataAndType",
            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&uri), JValue::Object(&mime)],
        )?;
        env.call_method(&view, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(VIEW_FLAGS)])?;
        let title = env.new_string("Print or open the PDF")?;
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[JValue::Object(&view), JValue::Object(&title)],
            )?
            .l()?;
        env.call_method(&chooser, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(VIEW_FLAGS)])?;
        env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&chooser)])?;
        Ok(Ok(()))
    })?
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn put_str(env: &mut JNIEnv, values: &JObject, key: &str, value: &JString) -> Jr<()> {
    let k = env.new_string(key)?;
    env.call_method(values, "put", "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&k), JValue::Object(value)])?;
    Ok(())
}

fn put_int(env: &mut JNIEnv, values: &JObject, key: &str, value: i32) -> Jr<()> {
    let k = env.new_string(key)?;
    let boxed = env.call_static_method("java/lang/Integer", "valueOf", "(I)Ljava/lang/Integer;", &[JValue::Int(value)])?.l()?;
    env.call_method(values, "put", "(Ljava/lang/String;Ljava/lang/Integer;)V", &[JValue::Object(&k), JValue::Object(&boxed)])?;
    Ok(())
}

fn new_intent<'l>(env: &mut JNIEnv<'l>, action: &str) -> Jr<JObject<'l>> {
    let a = env.new_string(action)?;
    env.new_object("android/content/Intent", "(Ljava/lang/String;)V", &[JValue::Object(&a)])
}

// ---------------------------------------------------------------------------------------------
// Open a link
// ---------------------------------------------------------------------------------------------

/// Open `url` in the user's browser (an `ACTION_VIEW` intent).
pub fn open_url(url: &str) -> Result<(), String> {
    with_env(|env, act| {
        let s = env.new_string(url)?;
        let uri = env
            .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&s)])?
            .l()?;
        let intent = new_intent(env, "android.intent.action.VIEW")?;
        env.call_method(&intent, "setData", "(Landroid/net/Uri;)Landroid/content/Intent;", &[JValue::Object(&uri)])?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(0x1000_0000)])?;
        env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    })
    .map_err(|e| format!("couldn't open the link: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Default app
// ---------------------------------------------------------------------------------------------

/// Android has no call that makes an app the default for a file type: the user picks it in
/// Settings › Default apps, or taps "Always" the next time a file is opened with NovaWrite.
pub fn open_default_apps_settings() -> Result<DefaultApp, String> {
    with_env(|env, act| {
        let intent = new_intent(env, "android.settings.MANAGE_DEFAULT_APPS_SETTINGS")?;
        env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(0x1000_0000)])?;
        env.call_method(act, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(DefaultApp::SettingsOpened)
    })
    .map_err(|e| format!("couldn't open Settings: {e}"))
}

// ---------------------------------------------------------------------------------------------
// "Open with NovaWrite"
// ---------------------------------------------------------------------------------------------

/// The last file URI taken from an intent, so one intent opens once.
fn last_uri() -> MutexGuard<'static, Option<String>> {
    static LAST: Mutex<Option<String>> = Mutex::new(None);
    LAST.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Largest file taken from another app.
const MAX_OPEN_BYTES: usize = 200 << 20;

/// The document another app opened with NovaWrite (a `VIEW` or `EDIT` intent), copied into `dir`
/// under its own name so the format is known from the extension. `None` when the launch had no
/// file, or this intent was already taken.
pub fn take_view_file(dir: &Path) -> Option<PathBuf> {
    let taken = with_env(|env, act| {
        let intent = env.call_method(act, "getIntent", "()Landroid/content/Intent;", &[])?.l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let action = env.call_method(&intent, "getAction", "()Ljava/lang/String;", &[])?.l()?;
        let action = jstr(env, &action)?.unwrap_or_default();
        if action != "android.intent.action.VIEW" && action != "android.intent.action.EDIT" {
            return Ok(None);
        }
        let uri = env.call_method(&intent, "getData", "()Landroid/net/Uri;", &[])?.l()?;
        let Some(uri_text) = to_string(env, &uri)? else { return Ok(None) };
        if last_uri().as_deref() == Some(uri_text.as_str()) {
            return Ok(None);
        }
        *last_uri() = Some(uri_text.clone());

        let cr = content_resolver(env, act)?;
        let name = display_name(env, &cr, &uri)?;
        let stream = env.call_method(&cr, "openInputStream", "(Landroid/net/Uri;)Ljava/io/InputStream;", &[JValue::Object(&uri)])?.l()?;
        if stream.is_null() {
            return Ok(Some(Err("Android would not give NovaWrite that file".to_string())));
        }
        Ok(Some(match read_stream(env, &stream, MAX_OPEN_BYTES)? {
            Some(bytes) => Ok((name, bytes)),
            None => Err("that file is too large to open".to_string()),
        }))
    });
    match taken {
        Ok(Some(Ok((name, bytes)))) => save_incoming(dir, &name, &bytes)
            .map_err(|e| log::warn!("open with: {e}"))
            .ok(),
        Ok(Some(Err(why))) => {
            log::warn!("open with: {why}");
            None
        }
        Ok(None) => None,
        Err(e) => {
            log::warn!("open with: {e}");
            None
        }
    }
}

/// The file's display name from its URI (`OpenableColumns.DISPLAY_NAME`), else the last path part.
fn display_name(env: &mut JNIEnv, cr: &JObject, uri: &JObject) -> Jr<String> {
    let null = JObject::null();
    let cursor = env
        .call_method(
            cr,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[JValue::Object(uri), JValue::Object(&null), JValue::Object(&null), JValue::Object(&null), JValue::Object(&null)],
        )?
        .l()?;
    let mut name = None;
    if !cursor.is_null() {
        if env.call_method(&cursor, "moveToFirst", "()Z", &[])?.z()? {
            let col = env.new_string("_display_name")?;
            let idx = env.call_method(&cursor, "getColumnIndex", "(Ljava/lang/String;)I", &[JValue::Object(&col)])?.i()?;
            if idx >= 0 {
                let s = env.call_method(&cursor, "getString", "(I)Ljava/lang/String;", &[JValue::Int(idx)])?.l()?;
                name = jstr(env, &s)?;
            }
        }
        let _ = env.call_method(&cursor, "close", "()V", &[]);
    }
    if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
        return Ok(n);
    }
    let seg = env.call_method(uri, "getLastPathSegment", "()Ljava/lang/String;", &[])?.l()?;
    Ok(jstr(env, &seg)?.unwrap_or_else(|| "document".to_string()))
}

/// Write an incoming file into `dir/Incoming` under a safe version of its name.
fn save_incoming(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let folder = dir.join("Incoming");
    std::fs::create_dir_all(&folder)?;
    let safe: String = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("document")
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
        .collect();
    let safe = if safe.trim_matches('.').is_empty() { "document".to_string() } else { safe };
    let path = folder.join(safe);
    let mut file = std::fs::File::create(&path)?;
    file.write_all(bytes)?;
    file.flush()?;
    Ok(path)
}

// ---------------------------------------------------------------------------------------------
// Keeping the MCP bridge awake
// ---------------------------------------------------------------------------------------------

static WAKE_LOCK: Mutex<Option<GlobalRef>> = Mutex::new(None);

/// Take (`on`) or release the partial wake lock. With the screen kept on as well (see
/// `host::frame`), the CPU and the network stay up while the AI works.
pub fn hold_wake_lock(on: bool) {
    let mut held = WAKE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    if on == held.is_some() {
        return;
    }
    let result = with_env(|env, act| {
        if on {
            let pm = service(env, act, "power")?;
            let tag = env.new_string("NovaWrite:mcp")?;
            // PowerManager.PARTIAL_WAKE_LOCK
            let lock = env
                .call_method(&pm, "newWakeLock", "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;", &[JValue::Int(1), JValue::Object(&tag)])?
                .l()?;
            env.call_method(&lock, "setReferenceCounted", "(Z)V", &[JValue::Bool(0)])?;
            env.call_method(&lock, "acquire", "()V", &[])?;
            Ok(Some(env.new_global_ref(&lock)?))
        } else {
            if let Some(lock) = held.as_ref() {
                env.call_method(lock.as_obj(), "release", "()V", &[])?;
            }
            Ok(None)
        }
    });
    match result {
        Ok(lock) => *held = lock,
        Err(e) => {
            log::warn!("wake lock: {e}");
            // A failed release still forgets the lock: the platform frees it with the process.
            if !on {
                *held = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// System animation setting
// ---------------------------------------------------------------------------------------------

/// Whether the system has animations on: Settings › Developer options › Animator duration scale
/// (and Accessibility › Remove animations) set this to 0.
pub fn animations_enabled() -> Option<bool> {
    with_env(|env, act| {
        let cr = content_resolver(env, act)?;
        let key = env.new_string("animator_duration_scale")?;
        let scale = env
            .call_static_method(
                "android/provider/Settings$Global",
                "getFloat",
                "(Landroid/content/ContentResolver;Ljava/lang/String;F)F",
                &[JValue::Object(&cr), JValue::Object(&key), JValue::Float(1.0)],
            )?
            .f()?;
        Ok(scale > 0.0)
    })
    .ok()
}
