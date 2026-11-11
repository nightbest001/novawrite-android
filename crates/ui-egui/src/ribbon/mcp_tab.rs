//! Ribbon › MCP: connect this WordCraft to an AI over an MCP server, as Blender's "MCP Bridge"
//! panel does. The tab only edits [`crate::mcp::McpShared`] and shows the status; the desktop
//! host's bridge thread does the connecting (see `apps/wordcraft/src/mcp_bridge.rs`).

use egui::{Color32, Rect, RichText, Sense, TextEdit, pos2, vec2};

use super::common::stack;
use super::{Ui, WordApp};
use crate::mcp::{McpAction, McpHandle, McpStatus, lock};
use crate::theme::{Tokens, regular};
use crate::widgets::{BIG_ICON_Y, BIG_LABEL_Y, CONTENT_H, LABEL_PITCH, bg, group, label_line};

const ONLINE: Color32 = Color32::from_rgb(0x2E, 0x8B, 0x57);
const CONNECTING: Color32 = Color32::from_rgb(0xE0, 0xA3, 0x00);
const OFFLINE: Color32 = Color32::from_rgb(0xD1, 0x3B, 0x3B);

/// Which text field of the Server group.
#[derive(Clone, Copy)]
enum Field {
    Url,
    Code,
}

pub(super) fn mcp_tab(app: &mut WordApp, ui: &mut Ui) {
    let Some(h) = app.mcp.clone() else {
        group(ui, "MCP", None, app, |ui, _app| {
            ui.label(tl!("The MCP bridge is only in the desktop app."));
        });
        return;
    };
    let snap = lock(&h).clone();
    // Generate asks for the URL on the clipboard once the new code exists.
    if snap.copy_url && snap.code_valid() {
        ui.ctx().copy_text(snap.mcp_url());
        lock(&h).copy_url = false;
    }
    group(ui, "Server", None, app, |ui, _app| {
        stack(ui, |ui| {
            field(ui, &h, "Server:", Field::Url);
            field(ui, &h, "Code:", Field::Code);
        });
    });
    group(ui, "Connection", None, app, |ui, _app| {
        if big_action(ui, "update", "Generate\nMCP", true, "Make a new code (the old one stops working) and connect") {
            lock(&h).actions.push(McpAction::Generate);
        }
        if big_action(ui, "copy", "Copy\nURL", snap.code_valid(), "Copy the MCP URL for the AI's connector settings") {
            lock(&h).copy_url = true;
        }
        if snap.status == McpStatus::Offline {
            if big_action(ui, "update", "Connect", snap.code_valid(), "Connect to the MCP server with this code") {
                lock(&h).actions.push(McpAction::Connect);
            }
        } else if big_action(ui, "close", "Disconnect", true, "Disconnect from the MCP server") {
            lock(&h).actions.push(McpAction::Disconnect);
        }
    });
    group(ui, "Options", None, app, |ui, _app| {
        stack(ui, |ui| {
            let mut auto = snap.auto_connect;
            if ui.checkbox(&mut auto, tl!("Connect automatically")).changed() {
                let mut g = lock(&h);
                g.auto_connect = auto;
                g.settings_changed = true;
            }
            let mut files = snap.allow_files;
            let hint = tl!("Lets the AI open, save and export files on this computer");
            if ui.checkbox(&mut files, tl!("Allow file access")).on_hover_text(hint).changed() {
                let mut g = lock(&h);
                g.allow_files = files;
                g.settings_changed = true;
            }
        });
    });
    group(ui, "Status", None, app, |ui, _app| {
        stack(ui, |ui| {
            let (text, color) = match snap.status {
                McpStatus::Online => (tl!("Online"), ONLINE),
                McpStatus::Connecting => (tl!("Connecting…"), CONNECTING),
                McpStatus::Offline => (tl!("Offline"), OFFLINE),
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(color));
                ui.label(text);
            });
            if snap.status == McpStatus::Online && !snap.ai_name.is_empty() {
                ui.label(format!("AI: {}", snap.ai_name));
            }
            if snap.status != McpStatus::Online && !snap.last_error.is_empty() {
                ui.label(RichText::new(shorten(&snap.last_error, 56)).small()).on_hover_text(&snap.last_error);
            }
        });
    });
}

/// One labelled text field of the Server group; a change is kept in the shared state and saved by
/// the host.
fn field(ui: &mut Ui, h: &McpHandle, label: &str, which: Field) {
    ui.horizontal(|ui| {
        ui.add_sized([46.0, 20.0], egui::Label::new(tl!(label)));
        // 230 px on a desktop; on a narrow screen (a phone) the field takes what is left.
        let room = (ui.available_width() - 8.0).clamp(120.0, 230.0);
        let mut text = {
            let g = lock(h);
            match which {
                Field::Url => g.url.clone(),
                Field::Code => g.code.clone(),
            }
        };
        if ui.add(TextEdit::singleline(&mut text).desired_width(room)).changed() {
            let mut g = lock(h);
            match which {
                Field::Url => g.url = text,
                Field::Code => g.code = text.trim().to_string(),
            }
            g.settings_changed = true;
        }
    });
}

/// A large ribbon button that is not a command: a 32 px icon over a (possibly two-line) label.
/// `true` on the frame it is clicked while `enabled`.
fn big_action(ui: &mut Ui, icon: &str, label: &str, enabled: bool, tip: &str) -> bool {
    let label = tl!(label);
    let t = Tokens::get(ui.ctx());
    let text_w =
        label.split('\n').map(|l| ui.ctx().fonts_mut(|f| f.layout_no_wrap(l.to_string(), regular(11.5), t.text).size().x)).fold(0.0, f32::max);
    let (r, resp) = ui.allocate_exact_size(vec2((text_w + 12.0).max(44.0), CONTENT_H), Sense::click());
    if enabled {
        bg(ui, r, &resp, false, &t);
    }
    let ic = Rect::from_center_size(pos2(r.center().x, r.min.y + BIG_ICON_Y), vec2(32.0, 32.0));
    let (c, a) = if enabled { (t.icon, t.accent) } else { (t.text_disabled, t.text_disabled) };
    crate::icons::paint(ui.painter(), ic, icon, c, a);
    let mut y = r.min.y + BIG_LABEL_Y;
    for l in label.split('\n') {
        label_line(ui, r.center().x, y, l, regular(11.5), if enabled { t.text } else { t.text_disabled }, false);
        y += LABEL_PITCH;
    }
    let resp = resp.on_hover_text(tl!(tip));
    resp.clicked() && enabled
}

/// `s` cut to `max` characters, with … when something was dropped.
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::shorten;

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        assert_eq!(shorten("abc", 5), "abc");
        assert_eq!(shorten("ééééé", 3), "ééé…");
    }
}
