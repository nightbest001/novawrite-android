//! Pemberitahuan update: sekali tiap app dibuka, app membaca release terbaru di GitHub
//! (`nightbest001/novawrite-android`). Kalau ada versi yang lebih baru, muncul jendela kecil berisi
//! catatan perubahan (isi release) dan tombol Unduh. "Nanti" menyembunyikan versi itu sampai ada
//! versi yang lebih baru lagi.
//!
//! Versi app diambil dari `NOVAWRITE_VERSION` (diisi workflow dari tag, mis. `v0.2.0`). Build
//! lokal tanpa variabel itu memakai versi paket.

use wordcraft_ui_egui::theme::{Tokens, medium, regular, semibold};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::Value;

const API: &str = "https://api.github.com/repos/nightbest001/novawrite-android/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/nightbest001/novawrite-android/releases/latest";
const URL_PREFIX: &str = "https://github.com/nightbest001/novawrite-android/";
const USER_AGENT: &str = concat!("NovaWrite/", env!("CARGO_PKG_VERSION"));
/// Catatan perubahan yang ditampilkan paling banyak segini (karakter).
const MAX_NOTES: usize = 2000;

/// Versi app yang sedang jalan.
pub fn current() -> &'static str {
    match option_env!("NOVAWRITE_VERSION") {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// Satu update yang tersedia.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub tag: String,
    /// Tautan unduh APK (kosong kalau release tidak punya APK: dipakai halaman release).
    pub apk_url: String,
    pub page_url: String,
    /// Isi release (catatan perubahan), sudah diratakan jadi teks biasa.
    pub notes: String,
}

fn parts(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
        .collect()
}

/// `remote` lebih baru dari `local`? (`v0.2.0` dan `0.2.0` dianggap sama.)
pub fn is_newer(remote: &str, local: &str) -> bool {
    let (a, b) = (parts(remote), parts(local));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Markdown release jadi teks biasa: judul tanpa `#`, butir dengan titik, tanpa `**`.
fn plain(md: &str) -> String {
    let mut out = String::new();
    for line in md.lines() {
        let t = line.trim_end();
        let t = if t.trim_start().starts_with('#') {
            t.trim_start().trim_start_matches('#').trim()
        } else if let Some(rest) = t.trim_start().strip_prefix("- ").or_else(|| t.trim_start().strip_prefix("* ")) {
            out.push_str("• ");
            rest
        } else {
            t
        };
        out.push_str(&t.replace("**", "").replace('`', ""));
        out.push('\n');
    }
    let text = out.trim().to_string();
    if text.chars().count() > MAX_NOTES {
        let cut: String = text.chars().take(MAX_NOTES).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// Isi jawaban `releases/latest` jadi [`Info`], atau `None` kalau tidak lebih baru dari `local`.
pub fn parse(body: &str, local: &str) -> Option<Info> {
    let v: Value = serde_json::from_str(body).ok()?;
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = v["tag_name"].as_str()?.trim().to_string();
    if tag.is_empty() || !is_newer(&tag, local) {
        return None;
    }
    // Hanya tautan di repo ini yang boleh dibuka dari jendela update.
    let safe = |u: &str| u.starts_with(URL_PREFIX).then(|| u.to_string());
    let page_url = v["html_url"].as_str().and_then(safe).unwrap_or_else(|| RELEASES_PAGE.to_string());
    let apk_url = v["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.to_ascii_lowercase().ends_with(".apk")))
        .and_then(|a| a["browser_download_url"].as_str())
        .and_then(safe)
        .unwrap_or_default();
    let notes = plain(v["body"].as_str().unwrap_or(""));
    Some(Info { tag, apk_url, page_url, notes })
}

/// Satu permintaan ke GitHub. `None` kalau offline, dibatasi, atau tidak ada update.
fn fetch(local: &str) -> Option<Info> {
    let config = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(false).build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(API).header("User-Agent", USER_AGENT).header("Accept", "application/vnd.github+json").call().ok()?;
    if resp.status().as_u16() != 200 {
        return None;
    }
    let text = resp.body_mut().read_to_string().ok()?;
    parse(&text, local)
}

/// Pengecek update dan jendelanya.
pub struct Updater {
    rx: Receiver<Info>,
    info: Option<Info>,
    /// Versi yang sudah dijawab "Nanti". Disimpan supaya tidak muncul lagi tiap app dibuka.
    dismissed: Option<String>,
    dismissed_path: PathBuf,
}

impl Updater {
    /// Mulai pengecekan di thread sendiri; hasilnya diambil [`Updater::show`].
    pub fn start(ctx: egui::Context, settings: PathBuf) -> Self {
        let (tx, rx) = channel();
        let dismissed_path = settings.join("update-dismissed.txt");
        let dismissed = std::fs::read_to_string(&dismissed_path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        std::thread::Builder::new()
            .name("novawrite-update".into())
            .spawn(move || {
                if let Some(info) = fetch(current()) {
                    let _ = tx.send(info);
                    ctx.request_repaint();
                }
            })
            .ok();
        Self { rx, info: None, dismissed, dismissed_path }
    }

    /// Gambar jendela update kalau ada. Mengembalikan tautan yang harus dibuka (tombol Unduh).
    pub fn show(&mut self, ctx: &egui::Context) -> Option<String> {
        if let Ok(info) = self.rx.try_recv()
            && self.dismissed.as_deref() != Some(info.tag.as_str())
        {
            self.info = Some(info);
        }
        let info = self.info.clone()?;
        let mut open_url = None;
        let mut later = false;
        let t = Tokens::get(ctx);
        let frame = egui::Frame::NONE
            .fill(t.panel)
            .stroke(egui::Stroke::new(1.0, t.border))
            .corner_radius(10)
            .inner_margin(egui::Margin::same(16))
            .shadow(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: egui::Color32::from_black_alpha(if t.dark { 120 } else { 40 }) });
        egui::Window::new("Update tersedia")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let width = (ctx.content_rect().width() - 64.0).clamp(220.0, 320.0);
                ui.set_max_width(width);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                ui.label(egui::RichText::new("Update tersedia").font(semibold(16.0)).color(t.text));
                ui.label(egui::RichText::new(format!("NovaWrite {} sudah tersedia", info.tag)).font(medium(13.0)).color(t.text));
                ui.label(egui::RichText::new(format!("Versi kamu: {}", current())).font(regular(12.0)).color(t.text_dim));
                if !info.notes.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Yang baru").font(semibold(13.0)).color(t.text));
                    egui::ScrollArea::vertical().max_height(200.0).auto_shrink([false, true]).show(ui, |ui| {
                        ui.label(egui::RichText::new(&info.notes).font(regular(12.5)).color(t.text));
                    });
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let primary = egui::Button::new(egui::RichText::new("Unduh").font(medium(13.0)).color(t.on_accent)).fill(t.accent).corner_radius(6);
                    if ui.add(primary).clicked() {
                        open_url = Some(if info.apk_url.is_empty() { info.page_url.clone() } else { info.apk_url.clone() });
                    }
                    let plain = |text: &str| {
                        egui::Button::new(egui::RichText::new(text).font(medium(13.0)).color(t.text)).fill(t.input).stroke(egui::Stroke::new(1.0, t.border)).corner_radius(6)
                    };
                    if ui.add(plain("Halaman release")).clicked() {
                        open_url = Some(info.page_url.clone());
                    }
                    if ui.add(plain("Nanti")).clicked() {
                        later = true;
                    }
                });
            });
        if later {
            if let Err(e) = std::fs::write(&self.dismissed_path, &info.tag) {
                log::warn!("update: cannot remember the dismissed version: {e}");
            }
            self.dismissed = Some(info.tag);
            self.info = None;
        }
        open_url
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.5"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
    }

    #[test]
    fn newer_release_becomes_info() {
        let json = r###"{
            "tag_name": "v0.2.0", "draft": false, "prerelease": false,
            "html_url": "https://github.com/nightbest001/novawrite-android/releases/tag/v0.2.0",
            "body": "## Ditambahkan\n- **Ikon** baru\n- Cek `update`\n\n## Diperbaiki\n- Galat build",
            "assets": [
                {"name": "notes.txt", "browser_download_url": "https://github.com/nightbest001/novawrite-android/releases/download/v0.2.0/notes.txt"},
                {"name": "NovaWrite-v0.2.0-android.apk", "browser_download_url": "https://github.com/nightbest001/novawrite-android/releases/download/v0.2.0/NovaWrite-v0.2.0-android.apk"}
            ]
        }"###;
        let info = parse(json, "0.1.0").unwrap();
        assert_eq!(info.tag, "v0.2.0");
        assert!(info.apk_url.ends_with("NovaWrite-v0.2.0-android.apk"));
        assert_eq!(info.notes, "Ditambahkan\n• Ikon baru\n• Cek update\n\nDiperbaiki\n• Galat build");
        assert!(parse(json, "0.2.0").is_none());
    }

    #[test]
    fn links_outside_the_repo_are_dropped() {
        let json = r#"{"tag_name": "v9.0.0", "html_url": "https://evil.example/x", "assets": [{"name": "a.apk", "browser_download_url": "https://evil.example/a.apk"}]}"#;
        let info = parse(json, "0.1.0").unwrap();
        assert_eq!(info.page_url, RELEASES_PAGE);
        assert!(info.apk_url.is_empty());
    }

    #[test]
    fn junk_and_drafts_are_ignored() {
        assert!(parse("not json", "0.1.0").is_none());
        assert!(parse(r#"{"tag_name": "v9.0.0", "draft": true}"#, "0.1.0").is_none());
        assert!(parse(r#"{"name": "no tag"}"#, "0.1.0").is_none());
    }
}
