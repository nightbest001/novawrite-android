//! State of the MCP tab, shared with the desktop host's bridge thread.
//!
//! Ribbon › MCP (`ribbon/mcp_tab.rs`) edits the server URL, code and options here and reads the
//! status; the host (`apps/wordcraft/src/mcp_bridge.rs`) owns the network side: it long-polls the
//! MCP server and answers the AI's tool calls through the same request channel the control port
//! uses. The browser app has no bridge, so there [`crate::WordApp`] holds no handle and the tab
//! says so.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Where the bridge to the MCP server stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum McpStatus {
    #[default]
    Offline,
    Connecting,
    Online,
}

/// Something the tab asks the bridge to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpAction {
    /// A new code (the old one stops working), copied as a URL, then connect.
    Generate,
    Connect,
    Disconnect,
}

/// What the tab and the bridge share.
#[derive(Clone, Debug, Default)]
pub struct McpShared {
    /// The MCP server, e.g. `https://plugin-roblox.wisp.uno`.
    pub url: String,
    /// The secret code: 16 to 64 letters or digits. Whoever knows it can drive this app.
    pub code: String,
    /// Connect when the app starts, if there is a code.
    pub auto_connect: bool,
    /// Let the AI open, save and export files on this computer.
    pub allow_files: bool,
    pub status: McpStatus,
    /// The AI client that last used the code, when the server knows it.
    pub ai_name: String,
    /// Why the bridge is not online, or the last problem.
    pub last_error: String,
    /// The tab puts the MCP URL on the clipboard on its next frame.
    pub copy_url: bool,
    /// Requests from the tab, taken by the bridge.
    pub actions: Vec<McpAction>,
    /// The url, code or options changed: the host saves them.
    pub settings_changed: bool,
}

impl McpShared {
    /// The URL to paste into the AI's connector settings.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp/{}", self.url.trim().trim_end_matches('/'), self.code)
    }

    /// Whether the code has the shape the server accepts.
    pub fn code_valid(&self) -> bool {
        (16..=64).contains(&self.code.len()) && self.code.bytes().all(|b| b.is_ascii_alphanumeric())
    }
}

/// The shared state, held by the app and by the bridge thread.
pub type McpHandle = Arc<Mutex<McpShared>>;

/// Lock the state; a panic on another thread never locks the tab out.
pub fn lock(h: &McpHandle) -> MutexGuard<'_, McpShared> {
    h.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_code_has_the_servers_shape() {
        let mut s = McpShared { code: "abcdef0123456789abcdef0123456789".into(), ..Default::default() };
        assert!(s.code_valid());
        s.code = "short".into();
        assert!(!s.code_valid());
        s.code = "abcdef0123456789abcdef012345678!".into();
        assert!(!s.code_valid());
    }

    #[test]
    fn the_url_joins_server_and_code() {
        let s = McpShared { url: " https://x.test/ ".into(), code: "c0de".into(), ..Default::default() };
        assert_eq!(s.mcp_url(), "https://x.test/mcp/c0de");
    }
}
