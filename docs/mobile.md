# NovaWrite (WordCraft on Android)

`apps/wordcraft-mobile` is the only app in this repo. It runs the same `WordApp` as WordCraft's
desktop app, with the MCP tab. The desktop app, the CLI, the web build and the PC installers were
removed: this repo builds one APK.

The MCP bridge runs **on the phone**: it long-polls the MCP server and runs the AI's tool calls
itself (`apps/wordcraft-mobile/src/mcp_bridge.rs`).

Status: **not yet built or run on a device.** The JNI code was type-checked against `jni 0.21`
with stubs, and the Android API names were checked against the `android-activity 0.6.1` and
`egui 0.37` sources, but nothing was compiled with the full workspace. Expect to fix compile errors
on the first build.

## What a phone does differently

| | Desktop WordCraft | NovaWrite |
|---|---|---|
| File Open / Save As | system dialog | built-in file browser, starts in the app's `Documents` folder |
| Settings, MCP code | `~/.config/wordcraft` (etc.) | app-private data folder |
| Ribbon on first run | open | collapsed |
| Clipboard (text, HTML, pictures) | arboard | `ClipboardManager` through JNI (`jni_host.rs`) |
| Print | spooler (CUPS, Windows) | PDF to `Download/NovaWrite` (MediaStore), opened in a PDF viewer that prints |
| Default app button | `xdg-mime` / Settings | opens Settings › Default apps; `VIEW`/`EDIT` intent filters in `Cargo.toml` |
| Open a file from another app | command line, Apple events | intent data copied to `Documents/Incoming` at start and on return to the app |
| Scrolling | wheel, touchpad | finger drag (`canvas/input.rs::touch_scroll`) |
| Keyboard | hardware | soft keyboard shown on a tap on the page or a text field |
| MCP bridge | always | online while the app is on screen; wake lock and screen-on flag while online |
| Animation setting | OS setting | `animator_duration_scale` |

## Where the code is

- `src/lib.rs`: the shell (preferences, recovery, MCP), shared with a PC `cargo check`.
- `src/android.rs`: the entry point and the one place that uses `unsafe` (hands the JVM and the
  activity to `jni_host`).
- `src/host.rs`: what the phone adds around the app; no-ops off Android.
- `src/jni_host.rs`: every call into the Android framework.
- `src/mcp_bridge.rs`: the MCP bridge.
- `crates/ui-egui/src/canvas/input.rs`, `canvas.rs`: touch scrolling.

## What still needs Java

- **A foreground service** to keep the MCP bridge alive while the app is in the background or the
  screen is locked. `cargo apk` cannot compile Java. Moving to Gradle (or `xbuild` with a Java
  module) would allow a `Service` class and a `PrintDocumentAdapter` for real in-app printing.
- **`onNewIntent`.** `NativeActivity` does not call `setIntent`, so a file opened with NovaWrite
  while it is already running is not seen. The manifest uses `singleTask` so Android reuses the
  running window instead of starting a second copy.
- **IME composition.** NativeActivity delivers key events, not composed text. `GameActivity` fixes
  this but needs AndroidX and Gradle.

## Build

    rustup target add aarch64-linux-android
    cargo install cargo-apk
    cd apps/wordcraft-mobile
    cargo apk build --release      # or: cargo apk run --release (device attached)

- `[package.metadata.android]` in `Cargo.toml` asks for `INTERNET` (the MCP bridge) and `WAKE_LOCK`.
- The entry point is `android_main` in `src/android.rs`. `android-activity` must be the version
  eframe uses: check with `cargo tree -i android-activity` and change `Cargo.toml` if it differs.
- Documents are in `Android/data/com.nightbest001.novawrite/files/Documents`, reachable from a
  file manager or USB with no permission.

## Connect the AI

Ribbon › MCP › Generate MCP, paste the copied URL into the AI's connector settings. The code is
kept in the app's private `settings/mcp-bridge.json`.

## Cargo.lock

The lock file still lists packages of the removed apps and lacks `jni`. Run `cargo apk build`
once without `--locked` so Cargo rewrites it.
