//! Android entry point. The platform's `NativeActivity` loads `libwordcraft_mobile.so` and calls
//! `android_main`.

use android_activity::AndroidApp;
use jni::JavaVM;
use jni::objects::JObject;

#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("NovaWrite"));
    init_jni(&app);
    // Private data (settings, the MCP code, recovery); needs no permission.
    let internal = app.internal_data_path();
    let settings = internal.clone().unwrap_or_else(std::env::temp_dir).join("settings");
    // The app's folder on shared storage (Android/data/<package>/files): the user can reach it
    // from a file manager or USB, and it needs no permission either.
    let documents = app.external_data_path().or(internal).unwrap_or_else(std::env::temp_dir).join("Documents");
    let options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    if let Err(e) = crate::run(options, crate::Paths { settings, documents }) {
        log::error!("NovaWrite stopped: {e}");
    }
}

/// Hand the JVM and the activity to `jni_host`. Without them the Java-only services (system
/// clipboard, print, Open with, wake lock) report an error and the rest of the app still runs.
fn init_jni(app: &AndroidApp) {
    // SAFETY: android-activity passes the process's `JavaVM*` and the activity's `jobject`. Both
    // stay valid while the activity lives, and this thread ends with it.
    let vm = match unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) } {
        Ok(vm) => vm,
        Err(e) => {
            log::error!("JNI: no JavaVM: {e}");
            return;
        }
    };
    let global = {
        let env = match vm.attach_current_thread() {
            Ok(env) => env,
            Err(e) => {
                log::error!("JNI: cannot attach: {e}");
                return;
            }
        };
        // SAFETY: as above; `JObject` does not free the reference when dropped.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        match env.new_global_ref(&activity) {
            Ok(g) => g,
            Err(e) => {
                log::error!("JNI: cannot keep the activity: {e}");
                return;
            }
        }
    };
    crate::jni_host::init(vm, global, app.clone());
}
