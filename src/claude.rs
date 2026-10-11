//! Claude Code keeps one `~/.claude/sessions/<pid>.json` per live session (name, cwd, busy/idle). Reading them
//! lists every Claude session on this machine, including ones that don't have tincan loaded.

use crate::{paths, util};
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
