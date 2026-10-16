//! Claude Code keeps one `~/.claude/sessions/<pid>.json` per live session (name, cwd, busy/idle). Reading them
//! lists every Claude session on this machine, including ones that don't have tincan loaded.

use crate::{agent::ClaudeSocket, paths, util};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSession {
    pub pid: u32,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub name: Option<String>,
    /// `user` after a `/rename` (`derived` for the default name).
    pub name_source: Option<String>,
    pub status: Option<String>,
    pub status_updated_at: Option<i64>,
    pub started_at: Option<i64>,
    /// Where the session takes messages from peers (what SendMessage uses).
    pub messaging_socket_path: Option<String>,
}

pub fn scan() -> Vec<ClaudeSession> {
    let Ok(dir) = std::fs::read_dir(paths::claude_sessions()) else {
        return Vec::new();
    };
    dir.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| serde_json::from_str::<ClaudeSession>(&std::fs::read_to_string(p).ok()?).ok())
        .filter(|s| util::pid_alive(s.pid))
        .collect()
}

impl ClaudeSession {
    /// A short name for the directory: Claude Code's own, or, when it named the session after its title (`auto`),
    /// the first words of it (the whole title shows as what it's doing).
    pub fn label(&self) -> Option<String> {
        let name = self.name.as_deref()?;
        let name = if self.name_source.as_deref() == Some("auto") {
            name.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
        } else {
            name.to_string()
        };
        Some(util::slug(&name)).filter(|n| !n.is_empty())
    }

    /// The name someone chose for the session (`/rename`), as opposed to the one Claude Code made up.
    pub fn chosen_name(&self) -> Option<&str> {
        let chosen = matches!(self.name_source.as_deref(), Some("user" | "peer" | "hook"));
        self.name.as_deref().filter(|_| chosen)
    }
}

/// Whether tincan is one of Claude Code's MCP servers here (`reason ask setup` chose Claude Code). Read once: the
/// daemon restarts after setup.
pub fn wired() -> bool {
    static WIRED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *WIRED.get_or_init(|| {
        let file = match std::env::var_os("CLAUDE_CONFIG_DIR") {
            Some(dir) => std::path::PathBuf::from(dir).join(".claude.json"),
            None => paths::user_home().join(".claude.json"),
        };
        std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| v["mcpServers"][crate::AGENT_NAME].is_object())
    })
}

/// The messaging socket of a live session, by its id or process.
pub fn socket_for(session_id: Option<&str>, pid: Option<u32>) -> Option<ClaudeSocket> {
    let c = scan().into_iter().find(|c| {
        (session_id.is_some() && c.session_id.as_deref() == session_id) || (pid.is_some() && Some(c.pid) == pid)
    })?;
    Some(ClaudeSocket { path: c.messaging_socket_path?.into(), token: peer_token(c.pid) })
}

/// The token a session's messaging socket wants from its peers, which Claude Code asks for on Windows only. It
/// leaves it for them in `~/.claude/sessions/<pid>.<hash>.key`.
pub fn peer_token(pid: u32) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let prefix = format!("{pid}.");
    std::fs::read_dir(paths::claude_sessions()).ok()?.flatten().find_map(|e| {
        let name = e.file_name().into_string().ok()?;
        if !name.starts_with(&prefix) || !name.ends_with(".key") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
        v["peerToken"].as_str().map(str::to_string)
    })
}
