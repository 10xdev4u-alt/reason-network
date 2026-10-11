//! `comms setup`: wires comms into every agent on this machine (MCP server, plus hooks where the agent has
//! them) and installs the daemon as a login service. `comms setup --remove` undoes it. Every file it edits is
//! backed up once, as `<file>.bak-comms`. Also `comms doctor`.

use crate::{agent, client::Link, paths, proto::*, render};
use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The service's name. A non-default `COMMS_HOME` gets its own, so a second instance (or a test) can never
/// replace the real one.
fn service_name() -> String {
    match std::env::var("COMMS_HOME") {
        Ok(h) => format!("dev.company-comms.daemon.{}", &blake3::hash(h.as_bytes()).to_hex()[..8]),
        Err(_) => "dev.company-comms.daemon".into(),
    }
}

/// What happened to one agent.
struct Report {
    agent: &'static str,
    lines: Vec<String>,
    notes: Vec<String>,
}

pub fn exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?.canonicalize()?)
}

/// The command line agents run through a shell: `'/path/to/comms' hook codex stop`.
fn shell_cmd(exe: &Path, args: &str) -> String {
    let p = exe.display().to_string();
    let quoted = if p.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) { p } else { format!("'{}'", p.replace('\'', "'\\''")) };
    format!("{quoted} {args}")
}

fn home() -> PathBuf {
    paths::user_home()
}

fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"))
}

fn gemini_home() -> PathBuf {
    std::env::var_os("GEMINI_CLI_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".gemini"))
}

fn opencode_home() -> PathBuf {
    home().join(".config/opencode")
}

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(json!({})),
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("{} is not plain JSON", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e.into()),
    }
}

/// Writes `content` if it differs, after backing up the original once. Returns whether anything changed.
fn write_file(path: &Path, content: &str, dry: bool) -> Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if dry {
        return Ok(true);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let backup = PathBuf::from(format!("{}.bak-comms", path.display()));
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup)?;
    }
    // Write then rename, so an agent never reads a half-written file.
    let tmp = PathBuf::from(format!("{}.tmp-comms", path.display()));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

fn write_json(path: &Path, v: &Value, dry: bool) -> Result<bool> {
    write_file(path, &(serde_json::to_string_pretty(v)? + "\n"), dry)
}

/// `v` as a JSON object (creating it if absent); anything else is left alone and reported.
fn obj(v: &mut Value) -> Result<&mut Map<String, Value>> {
    if v.is_null() {
        *v = json!({});
    }
    if !v.is_object() {
        return Err(anyhow!("expected a JSON object, found {v}; not touching it"));
    }
    Ok(v.as_object_mut().expect("checked above"))
}

/// Hook groups in the Claude Code style (`{matcher, hooks: [{type, command, ...}]}`), used by Codex and Gemini.
/// Replaces any earlier comms groups for `agent`, keeps everything else.
fn merge_hook_groups(hooks: &mut Map<String, Value>, agent: &str, groups: Vec<(&str, Value)>, remove: bool) {
    let marker = format!(" hook {agent} ");
    let mine = |v: &Value| v.to_string().contains(&marker);
    for (_, list) in hooks.iter_mut() {
        if let Some(arr) = list.as_array_mut() {
            // In a group, drop only our handlers (someone may have added theirs to the same group).
            for g in arr.iter_mut() {
                if let Some(inner) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    inner.retain(|h| !mine(h));
                }
            }
            arr.retain(|g| match g.get("hooks").and_then(Value::as_array) {
                Some(inner) => !inner.is_empty(),
                None => !mine(g),
            });
        }
    }
    hooks.retain(|_, list| list.as_array().is_none_or(|a| !a.is_empty()));
    if remove {
        return;
    }
    for (event, group) in groups {
        let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        if let Some(arr) = list.as_array_mut() {
            arr.push(group);
        }
    }
}

fn changed(path: &Path, did: bool) -> String {
    let p = crate::util::tilde(&path.display().to_string());
    if did { format!("updated {p}") } else { format!("{p} already set") }
}

// ---- Claude Code ----

fn claude(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let Some(bin) = agent::find_bin("claude") else { return Ok(None) };
    let mut r = Report { agent: "Claude Code", lines: Vec::new(), notes: Vec::new() };
    let get = Command::new(&bin).args(["mcp", "get", "comms"]).output()?;
    let existing = get.status.success();
    let current = existing && String::from_utf8_lossy(&get.stdout).contains(&exe.display().to_string());
    if remove || (existing && !current) {
        if existing && !dry {
            Command::new(&bin).args(["mcp", "remove", "comms", "--scope", "user"]).output()?;
        }
        if remove {
            r.lines.push(if existing { "removed the comms MCP server" } else { "nothing to remove" }.into());
            return Ok(Some(r));
        }
    }
    if current {
        r.lines.push("comms MCP server already set".into());
    } else {
        if !dry {
            let out = Command::new(&bin)
                .args(["mcp", "add", "--scope", "user", "comms", "--"])
                .arg(exe)
                .arg("mcp")
                .output()?;
            if !out.status.success() {
                return Err(anyhow!("claude mcp add failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        r.lines.push("added the comms MCP server (user scope)".into());
        r.notes.push("Claude Code: new sessions pick it up; restart running ones to join comms.".into());
    }
    Ok(Some(r))
}

// ---- Codex ----

fn codex(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let dir = codex_home();
    if agent::find_bin("codex").is_none() && !dir.exists() {
        return Ok(None);
    }
    let mut r = Report { agent: "Codex", lines: Vec::new(), notes: Vec::new() };

    let cfg_path = dir.join("config.toml");
    let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().with_context(|| format!("parsing {}", cfg_path.display()))?;
    if remove {
        if let Some(servers) = doc.get_mut("mcp_servers").and_then(|t| t.as_table_like_mut()) {
            servers.remove("comms");
        }
    } else {
        let servers = doc.entry("mcp_servers").or_insert(toml_edit::table());
        if let Some(t) = servers.as_table_mut() {
            t.set_implicit(true);
        }
        let mut t = toml_edit::Table::new();
        t["command"] = toml_edit::value(exe.display().to_string());
        t["args"] = toml_edit::value(toml_edit::Array::from_iter(["mcp"]));
        let mut env = toml_edit::InlineTable::new();
        env.insert("COMMS_AGENT", "codex".into());
        if let Ok(h) = std::env::var("COMMS_HOME") {
            env.insert("COMMS_HOME", h.into());
        }
        t["env"] = toml_edit::value(env);
        // Room for check_inbox(wait_secs) long-polls.
        t["tool_timeout_sec"] = toml_edit::value(660);
        t["default_tools_approval_mode"] = toml_edit::value("approve");
        servers["comms"] = toml_edit::Item::Table(t);
    }
    r.lines.push(changed(&cfg_path, write_file(&cfg_path, &doc.to_string(), dry)?));

    let hooks_path = dir.join("hooks.json");
    let mut v = read_json(&hooks_path)?;
    let group = |event: &str, matcher: bool, timeout: u64| {
        let mut g = json!({ "hooks": [{ "type": "command", "command": shell_cmd(exe, &format!("hook codex {event}")), "timeout": timeout }] });
        if matcher {
            g["matcher"] = json!("*");
        }
        g
    };
    let groups = vec![
        ("SessionStart", group("session-start", true, 10)),
        ("UserPromptSubmit", group("user-prompt-submit", false, 10)),
        ("PostToolUse", group("post-tool-use", true, 10)),
        ("Stop", group("stop", false, 10)),
        ("SessionEnd", group("session-end", false, 3)),
    ];
    merge_hook_groups(obj(obj(&mut v)?.entry("hooks").or_insert_with(|| json!({})))?, "codex", groups, remove);
    let did = write_json(&hooks_path, &v, dry)?;
    r.lines.push(changed(&hooks_path, did));
    if did && !remove {
        r.notes.push(
            "Codex: next time you start codex it asks about new hooks; choose \"Trust all and continue\" (or review them in /hooks). Until then Codex sessions only see messages when they check their inbox.".into(),
        );
    }
    Ok(Some(r))
}

// ---- Cursor ----

fn cursor(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let dir = home().join(".cursor");
    if !dir.exists() && agent::find_bin("cursor-agent").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "Cursor", lines: Vec::new(), notes: Vec::new() };
    let mcp_path = dir.join("mcp.json");
    let mut v = read_json(&mcp_path)?;
    let servers = obj(obj(&mut v)?.entry("mcpServers").or_insert_with(|| json!({})))?;
    if remove {
        servers.remove("comms");
    } else {
        servers.insert(
            "comms".into(),
            json!({ "type": "stdio", "command": exe.display().to_string(), "args": ["mcp"], "env": { "COMMS_AGENT": "cursor" } }),
        );
    }
    r.lines.push(changed(&mcp_path, write_json(&mcp_path, &v, dry)?));

    let hooks_path = dir.join("hooks.json");
    let mut v = read_json(&hooks_path)?;
    obj(&mut v)?.entry("version").or_insert(json!(1));
    let hooks = obj(obj(&mut v)?.entry("hooks").or_insert_with(|| json!({})))?;
    let entry = |event: &str| json!({ "command": shell_cmd(exe, &format!("hook cursor {event}")), "timeout": 10 });
    let mut ours = vec![
        ("sessionStart", entry("session-start")),
        ("beforeSubmitPrompt", entry("user-prompt-submit")),
        ("postToolUse", entry("post-tool-use")),
        ("stop", entry("stop")),
        ("sessionEnd", entry("session-end")),
    ];
    ours[2].1["matcher"] = json!("*");
    merge_hook_groups(hooks, "cursor", ours, remove);
    r.lines.push(changed(&hooks_path, write_json(&hooks_path, &v, dry)?));
    Ok(Some(r))
}

// ---- Gemini CLI ----

fn gemini(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let dir = gemini_home();
    if !dir.exists() && agent::find_bin("gemini").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "Gemini CLI", lines: Vec::new(), notes: Vec::new() };
    let path = dir.join("settings.json");
    let mut v = read_json(&path)?;
    let servers = obj(obj(&mut v)?.entry("mcpServers").or_insert_with(|| json!({})))?;
    if remove {
        servers.remove("comms");
    } else {
        servers.insert(
            "comms".into(),
            json!({ "command": exe.display().to_string(), "args": ["mcp"], "env": { "COMMS_AGENT": "gemini" }, "timeout": 660000, "trust": true }),
        );
    }
    let group = |event: &str, timeout: u64| {
        json!({ "matcher": "*", "hooks": [{ "type": "command", "name": format!("comms-{event}"), "command": shell_cmd(exe, &format!("hook gemini {event}")), "timeout": timeout }] })
    };
    let groups = vec![
        ("SessionStart", group("session-start", 10000)),
        ("BeforeAgent", group("user-prompt-submit", 10000)),
        ("AfterTool", group("post-tool-use", 10000)),
        ("AfterAgent", group("stop", 10000)),
        ("SessionEnd", group("session-end", 5000)),
    ];
    merge_hook_groups(obj(obj(&mut v)?.entry("hooks").or_insert_with(|| json!({})))?, "gemini", groups, remove);
    let did = write_json(&path, &v, dry)?;
    r.lines.push(changed(&path, did));
    if did && !remove {
        r.notes.push("Gemini CLI: hooks and MCP servers only run in trusted folders (`/permissions trust` in a project).".into());
    }
    Ok(Some(r))
}

// ---- GitHub Copilot CLI ----

fn copilot(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let dir = home().join(".copilot");
    if !dir.exists() && agent::find_bin("copilot").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "Copilot CLI", lines: Vec::new(), notes: Vec::new() };
    let mcp_path = dir.join("mcp-config.json");
    let mut v = read_json(&mcp_path)?;
    let servers = obj(obj(&mut v)?.entry("mcpServers").or_insert_with(|| json!({})))?;
    if remove {
        servers.remove("comms");
    } else {
        servers.insert(
            "comms".into(),
            json!({ "type": "local", "command": exe.display().to_string(), "args": ["mcp"], "env": { "COMMS_AGENT": "copilot" }, "tools": ["*"], "timeout": 660000 }),
        );
    }
    r.lines.push(changed(&mcp_path, write_json(&mcp_path, &v, dry)?));

    let hooks_path = dir.join("hooks/comms.json");
    if remove {
        if hooks_path.exists() && !dry {
            std::fs::remove_file(&hooks_path)?;
        }
        r.lines.push(format!("removed {}", crate::util::tilde(&hooks_path.display().to_string())));
    } else {
        let entry = |event: &str| json!([{ "type": "command", "bash": shell_cmd(exe, &format!("hook copilot {event}")), "timeoutSec": 10 }]);
        let v = json!({ "version": 1, "hooks": {
            "sessionStart": entry("session-start"),
            "userPromptSubmitted": entry("user-prompt-submit"),
            "postToolUse": entry("post-tool-use"),
            "agentStop": entry("stop"),
            "sessionEnd": entry("session-end"),
        }});
        r.lines.push(changed(&hooks_path, write_json(&hooks_path, &v, dry)?));
    }
    Ok(Some(r))
}

// ---- opencode ----

fn opencode(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let dir = opencode_home();
    if !dir.exists() && agent::find_bin("opencode").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "opencode", lines: Vec::new(), notes: Vec::new() };
    let path = dir.join("opencode.json");
    if dir.join("opencode.jsonc").exists() && !path.exists() {
        r.notes.push(format!(
            "opencode: you use opencode.jsonc, which comms doesn't edit. Add under \"mcp\": \"comms\": {{\"type\": \"local\", \"command\": [\"{}\", \"mcp\"], \"environment\": {{\"COMMS_AGENT\": \"opencode\"}}}}",
            exe.display()
        ));
    } else {
        let mut v = read_json(&path)?;
        let mcp = obj(obj(&mut v)?.entry("mcp").or_insert_with(|| json!({})))?;
        if remove {
            mcp.remove("comms");
        } else {
            mcp.insert(
                "comms".into(),
                json!({ "type": "local", "command": [exe.display().to_string(), "mcp"], "environment": { "COMMS_AGENT": "opencode" }, "enabled": true }),
            );
        }
        r.lines.push(changed(&path, write_json(&path, &v, dry)?));
    }
    let plugin = dir.join("plugins/comms.ts");
    if remove {
        if plugin.exists() && !dry {
            std::fs::remove_file(&plugin)?;
        }
        r.lines.push(format!("removed {}", crate::util::tilde(&plugin.display().to_string())));
    } else {
        let code = include_str!("opencode-plugin.ts").replace("__COMMS_EXE__", &exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
        r.lines.push(changed(&plugin, write_file(&plugin, &code, dry)?));
    }
    Ok(Some(r))
}

// ---- background service ----

pub fn service_installed() -> bool {
    service_path().exists()
}

fn service_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join(format!("Library/LaunchAgents/{}.plist", service_name()))
    } else {
        home().join(format!(".config/systemd/user/{}.service", service_name()))
    }
}

/// A PATH for the daemon that includes where agents usually live (it runs `codex queue`).
fn service_path_env() -> String {
    let h = home().display().to_string();
    format!("{h}/.local/bin:{h}/.bun/bin:{h}/.npm-global/bin:{h}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
}

async fn service(exe: &Path, remove: bool, dry: bool) -> Result<String> {
    let path = service_path();
    let uid = unsafe { libc::getuid() };
    let comms_home = std::env::var("COMMS_HOME").ok();
    if cfg!(target_os = "macos") {
        let target = format!("gui/{uid}/{}", service_name());
        if remove {
            if !dry {
                let _ = Command::new("launchctl").args(["bootout", &target]).output();
                let _ = std::fs::remove_file(&path);
            }
            return Ok("removed the login service".into());
        }
        let home_env = comms_home.map(|h| format!("\n    <key>COMMS_HOME</key><string>{}</string>", xml(&h))).unwrap_or_default();
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>daemon</string><string>--service</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>{}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>{}</string>{home_env}
  </dict>
</dict>
</plist>
"#,
            xml(&service_name()),
            xml(&exe.display().to_string()),
            xml(&paths::log().display().to_string()),
            xml(&service_path_env()),
        );
        let did = write_file(&path, &plist, dry)?;
        let loaded = Command::new("launchctl").args(["print", &target]).output().is_ok_and(|o| o.status.success());
        if !dry && (did || !loaded) {
            let _ = Command::new("launchctl").args(["bootout", &target]).output();
            crate::stop_daemon().await;
            let out = Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}")]).arg(&path).output()?;
            if !out.status.success() {
                return Err(anyhow!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        Ok(if did || !loaded { "installed the login service (launchd)".into() } else { "login service already running".into() })
    } else {
        if remove {
            if !dry {
                let _ = Command::new("systemctl").args(["--user", "disable", "--now", &format!("{}.service", service_name())]).output();
                let _ = std::fs::remove_file(&path);
            }
            return Ok("removed the login service".into());
        }
        let home_env = comms_home.map(|h| format!("Environment=\"COMMS_HOME={h}\"\n")).unwrap_or_default();
        let unit = format!(
            "[Unit]\nDescription=company-comms daemon\n\n[Service]\nExecStart={} daemon --service\nRestart=always\nRestartSec=3\nEnvironment=\"PATH={}\"\n{home_env}\n[Install]\nWantedBy=default.target\n",
            shell_cmd(exe, "").trim_end(),
            service_path_env()
        );
        let did = write_file(&path, &unit, dry)?;
        if !dry {
            crate::stop_daemon().await;
            Command::new("systemctl").args(["--user", "daemon-reload"]).output()?;
            let out = Command::new("systemctl").args(["--user", "enable", "--now", &format!("{}.service", service_name())]).output()?;
            if !out.status.success() {
                return Err(anyhow!("systemctl: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            // Keep running after logout (servers, SSH boxes). Best effort: some systems want a password.
            if let Ok(user) = std::env::var("USER") {
                let _ = Command::new("loginctl").args(["enable-linger", &user]).stdin(std::process::Stdio::null()).output();
            }
        }
        Ok(if did { "installed the login service (systemd --user)".into() } else { "login service already set".into() })
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

type AgentFn = fn(&Path, bool, bool) -> Result<Option<Report>>;

struct Agent {
    id: &'static str,
    name: &'static str,
    /// How quickly it sees messages, and anything to know.
    hint: &'static str,
    installed: fn() -> bool,
    wired: fn(&Path) -> bool,
    apply: AgentFn,
}

fn has(path: PathBuf, needle: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(needle))
}

fn me(exe: &Path) -> String {
    exe.display().to_string()
}

const AGENTS: [Agent; 6] = [
    Agent {
        id: "claude",
        name: "Claude Code",
        hint: "messages arrive instantly",
        installed: || agent::find_bin("claude").is_some(),
        wired: |_| {
            agent::find_bin("claude")
                .is_some_and(|b| Command::new(b).args(["mcp", "get", "comms"]).output().is_ok_and(|o| o.status.success()))
        },
        apply: claude,
    },
    Agent {
        id: "codex",
        name: "Codex",
        hint: "instant; asks once to trust the hooks",
        installed: || agent::find_bin("codex").is_some() || codex_home().exists(),
        wired: |exe| has(codex_home().join("config.toml"), &me(exe)) && has(codex_home().join("hooks.json"), "hook codex"),
        apply: codex,
    },
    Agent {
        id: "cursor",
        name: "Cursor",
        hint: "sees messages at its next step",
        installed: || home().join(".cursor").exists() || agent::find_bin("cursor-agent").is_some(),
        wired: |exe| has(home().join(".cursor/mcp.json"), &me(exe)),
        apply: cursor,
    },
    Agent {
        id: "gemini",
        name: "Gemini CLI",
        hint: "at its next step; trusted folders only",
        installed: || gemini_home().exists() || agent::find_bin("gemini").is_some(),
        wired: |exe| has(gemini_home().join("settings.json"), &me(exe)),
        apply: gemini,
    },
    Agent {
        id: "copilot",
        name: "Copilot CLI",
        hint: "sees messages at its next step",
        installed: || home().join(".copilot").exists() || agent::find_bin("copilot").is_some(),
        wired: |exe| has(home().join(".copilot/mcp-config.json"), &me(exe)),
        apply: copilot,
    },
    Agent {
        id: "opencode",
        name: "opencode",
        hint: "messages arrive within seconds",
        installed: || opencode_home().exists() || agent::find_bin("opencode").is_some(),
        wired: |_| opencode_home().join("plugins/comms.ts").exists(),
        apply: opencode,
    },
];

/// An agent found on this machine.
pub struct Found {
    pub id: &'static str,
    pub name: &'static str,
    pub hint: &'static str,
    pub wired: bool,
}

pub fn detect(exe: &Path) -> Vec<Found> {
    AGENTS
        .iter()
        .filter(|a| (a.installed)())
        .map(|a| Found { id: a.id, name: a.name, hint: a.hint, wired: (a.wired)(exe) })
        .collect()
}

/// What `apply` did: a line per agent (and the service), and things the user should know.
#[derive(Default)]
pub struct Applied {
    pub lines: Vec<String>,
    pub notes: Vec<String>,
}

/// Wires comms into the `install` agents and out of the `remove` ones; installs (`Some(true)`) or removes
/// (`Some(false)`) the login service.
pub async fn apply(exe: &Path, install: &[&str], remove: &[&str], service: Option<bool>, dry: bool) -> Applied {
    let mut out = Applied::default();
    for a in &AGENTS {
        let removing = remove.contains(&a.id);
        if !removing && !install.contains(&a.id) {
            continue;
        }
        match (a.apply)(exe, removing, dry) {
            Ok(Some(r)) => {
                out.lines.push(format!("{}: {}", r.agent, r.lines.join("; ")));
                out.notes.extend(r.notes);
            }
            Ok(None) => {}
            Err(e) => out.lines.push(format!("{}: skipped ({e:#})", a.name)),
        }
    }
    if let Some(on) = service {
        if std::env::var_os("COMMS_NO_SERVICE").is_some() {
            out.lines.push("service: skipped (COMMS_NO_SERVICE is set)".into());
            return out;
        }
        match self::service(exe, !on, dry).await {
            Ok(s) => out.lines.push(format!("service: {s}")),
            Err(e) => out.lines.push(format!("service: not installed ({e:#}); the daemon still starts on demand")),
        }
    }
    out
}

/// Non-interactive setup: every agent found (or, with `remove`, every agent wired).
pub async fn run(dry: bool, remove: bool, no_service: bool) -> Result<()> {
    let exe = exe()?;
    let found = detect(&exe);
    let verb = if dry { "Would change" } else if remove { "Removing comms" } else { "Setting up comms" };
    println!("{verb} ({}):", crate::util::tilde(&exe.display().to_string()));
    if found.is_empty() && !remove {
        println!("  no supported agents found (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI, opencode)");
    }
    let ids: Vec<&str> = found.iter().map(|f| f.id).collect();
    let (install, removing): (&[&str], &[&str]) = if remove { (&[], &ids) } else { (&ids, &[]) };
    let out = apply(&exe, install, removing, (!no_service).then_some(!remove), dry).await;
    for l in &out.lines {
        println!("  {l}");
    }
    if !out.notes.is_empty() {
        println!();
        for n in &out.notes {
            println!("- {n}");
        }
    }
    Ok(())
}

/// Read-only checks, one line each.
pub async fn doctor() -> Result<()> {
    let exe = exe()?;
    println!("comms {} at {}", env!("CARGO_PKG_VERSION"), crate::util::tilde(&exe.display().to_string()));
    let link = Link::new(None);
    match link.call::<TeamInfo>(Request::Team).await {
        Ok(t) => {
            let online = t.machines.iter().filter(|m| !m.is_me && m.online).count();
            let others = t.machines.len().saturating_sub(1);
            println!("✓ daemon running; team {} with {others} other machine(s), {online} connected", t.name);
            print!("{}", render::team(&t).lines().skip(1).map(|l| format!("{l}\n")).collect::<String>());
            if others > 0 && online == 0 {
                println!("  ! no teammate connected: check your internet, or that their machines are on (`comms doctor` there)");
            }
        }
        Err(e) if e.to_string().contains("not in a team") => {
            println!("✓ daemon running");
            println!("- not in a team yet: run `comms`");
        }
        Err(e) => println!("✗ daemon: {e:#}"),
    }
    let svc = service_path();
    println!(
        "{} login service: {}",
        if svc.exists() { "✓" } else { "-" },
        if svc.exists() { "installed" } else { "not installed (`comms setup`)" }
    );
    for f in detect(&exe) {
        if f.wired {
            println!("✓ {}: wired up", f.name);
        } else {
            println!("- {}: installed, not on comms (`comms setup`)", f.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_groups_merge_and_remove() {
        let mut hooks = serde_json::from_value::<Map<String, Value>>(json!({
            "Stop": [{ "hooks": [{ "type": "command", "command": "other-tool stop" }] }],
            "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "/x/comms hook codex post-tool-use" }] }],
        }))
        .unwrap();
        let ours = |e: &str| json!({ "hooks": [{ "type": "command", "command": format!("/y/comms hook codex {e} ") }] });
        merge_hook_groups(&mut hooks, "codex", vec![("Stop", ours("stop")), ("PostToolUse", ours("post-tool-use"))], false);
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 2, "keeps other tools' hooks");
        assert_eq!(hooks["PostToolUse"].as_array().unwrap().len(), 1, "replaces the old comms hook");
        merge_hook_groups(&mut hooks, "codex", vec![], true);
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 1);
        assert!(!hooks.contains_key("PostToolUse"));
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_cmd(Path::new("/usr/local/bin/comms"), "mcp"), "/usr/local/bin/comms mcp");
        assert_eq!(shell_cmd(Path::new("/Users/a b/comms"), "mcp"), "'/Users/a b/comms' mcp");
    }
}
