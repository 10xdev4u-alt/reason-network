//! Which agent are we running under, what is its session, and how do we reach it.

use crate::{git::GitInfo, paths, util};
use serde_json::{Value, json};
use std::{io::Write, os::unix::net::UnixStream, path::PathBuf, time::Duration};

pub const CLAUDE_CODE: &str = "claude-code";

/// Best guess from the environment the agent gives its children. `COMMS_AGENT` (set by `comms setup` in the
/// MCP config) wins.
pub fn detect_env() -> Option<&'static str> {
    let has = |k: &str| std::env::var_os(k).is_some();
    if let Ok(a) = std::env::var("COMMS_AGENT") {
        return from_client_name(&a);
    }
    if has("CLAUDECODE") || has("CLAUDE_CODE_SESSION_ID") {
        Some(CLAUDE_CODE)
    } else if has("CODEX_THREAD_ID") || has("CODEX_SESSION_ID") {
        Some("codex")
    } else if has("GEMINI_CLI") {
        Some("gemini")
    } else if has("CURSOR_AGENT") {
        Some("cursor")
    } else if has("OPENCODE") {
        Some("opencode")
    } else if has("COPILOT_CLI") || has("COPILOT_AGENT_SESSION_ID") {
        Some("copilot")
    } else {
        None
    }
}

/// From the MCP client's `clientInfo.name`, which beats the environment (agents get launched from each
/// other's terminals and inherit their variables).
pub fn from_client_name(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let has = |s: &str| n.contains(s);
    Some(if has("claude-code") || has("claude code") {
        CLAUDE_CODE
    } else if has("codex") {
        "codex"
    } else if has("cursor") {
        "cursor"
    } else if has("gemini") {
        "gemini"
    } else if has("opencode") {
        "opencode"
    } else if has("copilot") {
        "copilot"
    } else if n == "amp" || n.starts_with("amp-") || n.starts_with("amp ") {
        "amp"
    } else if has("zed") {
        "zed"
    } else if has("devin") || has("windsurf") {
        "devin"
    } else if has("claude") {
        "claude-desktop"
    } else {
        return None;
    })
}

/// Agents that run one MCP server process for many sessions, so the session must come from each call.
pub fn multiplexed(agent: &str) -> bool {
    matches!(agent, "codex" | "opencode" | "cursor")
}

/// The session id an agent exports to its MCP servers, if any.
pub fn env_session_id(agent: &str) -> Option<String> {
    let var = match agent {
        CLAUDE_CODE => "CLAUDE_CODE_SESSION_ID",
        "copilot" => "COPILOT_AGENT_SESSION_ID",
        "gemini" => "GEMINI_SESSION_ID",
        "codex" => "CODEX_THREAD_ID",
        "opencode" => "OPENCODE_SESSION_ID",
        _ => return None,
    };
    std::env::var(var).ok().filter(|s| !s.is_empty())
}

/// The session id carried in a tool call's `_meta`. Codex sends `sessionId` (the root thread, the same id its
/// hooks see) and `threadId` (which differs inside subagents); opencode V2 sends `ai.opencode/sessionID`.
pub fn meta_session_id(meta: &serde_json::Map<String, Value>) -> Option<String> {
    ["sessionId", "ai.opencode/sessionID", "threadId"]
        .iter()
        .find_map(|k| meta.get(*k).and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Finds an executable on `PATH` or in the usual install places (a launchd/systemd daemon has a bare `PATH`).
pub fn find_bin(name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(format!("COMMS_{}_BIN", name.to_uppercase())) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let home = paths::user_home();
    let extra = [
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".cargo/bin"),
        home.join(".opencode/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).chain(extra).map(|d| d.join(name)).find(|p| {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// The agent process behind a hook: our parent, skipping the shell some agents run hooks through.
pub fn hook_agent_pid() -> Option<u32> {
    let mut pid = util::parent_pid();
    for _ in 0..3 {
        let out = std::process::Command::new("ps").args(["-o", "ppid=,comm=", "-p", &pid.to_string()]).output().ok()?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let (ppid, comm) = line.split_once(' ')?;
        let comm = comm.trim().rsplit('/').next().unwrap_or_default().trim_start_matches('-');
        if !matches!(comm, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh") {
            return Some(pid);
        }
        pid = ppid.trim().parse().ok()?;
    }
    Some(pid)
}

pub fn short(agent: &str) -> &str {
    match agent {
        CLAUDE_CODE | "claude-desktop" => "claude",
        "" => "agent",
        a => a,
    }
}

/// `<repo-or-dir>-<agent>`, e.g. `api-claude`.
pub fn default_name(agent: &str, cwd: &str, git: &GitInfo) -> String {
    let dir = git.root.as_deref().unwrap_or(cwd);
    let base = util::slug(dir.rsplit('/').find(|s| !s.is_empty()).unwrap_or("home"));
    let base = if base.is_empty() { "home".into() } else { base };
    format!("{base}-{}", short(agent))
}

pub fn cwd() -> String {
    std::env::var("CLAUDE_PROJECT_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| "/".into())
}

/// The agent process: `CLAUDE_PID` when Claude Code sets it, else our parent.
pub fn agent_pid() -> u32 {
    std::env::var("CLAUDE_PID").ok().and_then(|s| s.parse().ok()).unwrap_or_else(util::parent_pid)
}

/// The inbox socket of the Claude Code session that spawned us. Posting a `user` frame there puts the text in
/// the conversation between tool calls, or starts a turn if the session is idle.
#[derive(Debug, Clone)]
pub struct ClaudeSocket {
    pub path: PathBuf,
    pub token: Option<String>,
}

pub fn claude_socket() -> Option<ClaudeSocket> {
    let token = std::env::var("CLAUDE_CODE_MESSAGING_TOKEN").ok().filter(|s| !s.is_empty());
    if let Some(path) = std::env::var_os("CLAUDE_CODE_MESSAGING_SOCKET") {
        return Some(ClaudeSocket { path: path.into(), token });
    }
    // Not exported to this process: fall back to the session's registry entry.
    let file = paths::claude_sessions().join(format!("{}.json", agent_pid()));
    let s: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    let path = s["messagingSocketPath"].as_str()?;
    Some(ClaudeSocket { path: path.into(), token })
}

pub fn post_to_claude(sock: &ClaudeSocket, text: &str) -> std::io::Result<()> {
    let mut c = UnixStream::connect(&sock.path)?;
    c.set_write_timeout(Some(Duration::from_secs(5)))?;
    if let Some(t) = &sock.token {
        writeln!(c, "{}", json!({ "type": "auth", "token": t }))?;
    }
    writeln!(c, "{}", json!({ "type": "user", "message": { "role": "user", "content": text } }))?;
    c.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names() {
        assert_eq!(from_client_name("claude-code"), Some(CLAUDE_CODE));
        assert_eq!(from_client_name("codex-mcp-client"), Some("codex"));
        assert_eq!(from_client_name("cursor-vscode"), Some("cursor"));
        assert_eq!(from_client_name("gemini-cli-mcp-client"), Some("gemini"));
        assert_eq!(from_client_name("claude-ai"), Some("claude-desktop"));
        assert_eq!(from_client_name("example-client"), None);
    }

    #[test]
    fn names() {
        let git = GitInfo { root: Some("/Users/alex/src/API".into()), ..Default::default() };
        assert_eq!(default_name(CLAUDE_CODE, "/Users/alex/src/API/web", &git), "api-claude");
        assert_eq!(default_name("codex", "/tmp/My Thing", &GitInfo::default()), "my-thing-codex");
    }
}
