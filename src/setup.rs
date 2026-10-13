//! `tincan setup`: wires tincan into every agent on this machine (MCP server, plus hooks where the agent has
//! them) and installs the daemon as a login service. `tincan setup --remove` undoes it. Every file it edits is
//! backed up once, as `<file>.bak-tincan`. Also `tincan doctor`.

use crate::{agent, client::Link, config::Config, paths, proto::*, render};
use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The service's name. A non-default `TINCAN_HOME` gets its own, so a second instance (or a test) can never
/// replace the real one.
fn service_name() -> String {
    match std::env::var("TINCAN_HOME") {
        Ok(h) => format!("dev.tincan.daemon.{}", &blake3::hash(h.as_bytes()).to_hex()[..8]),
        Err(_) => "dev.tincan.daemon".into(),
    }
}

/// What happened to one agent.
struct Report {
    agent: &'static str,
    lines: Vec<String>,
    notes: Vec<String>,
}

pub fn exe() -> Result<PathBuf> {
    let exe = crate::util::current_exe()?;
    // Windows reports it in full already; its canonical form (`\\?\C:\…`) is not for other programs' configs.
    if cfg!(windows) {
        return Ok(exe);
    }
    Ok(exe.canonicalize()?)
}

/// The command line agents run through a shell: `'/path/to/tincan' hook codex stop`. On Windows that shell is
/// PowerShell (`& 'C:\Users\Alex Smith\…\tincan.exe' hook codex stop`), and a path that needs no quotes also runs in
/// cmd.exe.
fn shell_cmd(exe: &Path, args: &str) -> String {
    let p = exe.display().to_string();
    let plain = |extra: &str| p.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    let quoted = if cfg!(windows) {
        if plain(r"\:._-") { p.clone() } else { format!("& '{}'", p.replace('\'', "''")) }
    } else if plain("/._-") {
        p.clone()
    } else {
        format!("'{}'", p.replace('\'', "'\\''"))
    };
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
    home().join(".config").join("opencode")
}

fn pi_home() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).unwrap_or_else(|| home().join(".pi").join("agent"))
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
    let backup = PathBuf::from(format!("{}.bak-tincan", path.display()));
    if path.exists() && !backup.exists() {
        std::fs::copy(path, &backup)?;
    }
    // Write then rename, so an agent never reads a half-written file.
    let tmp = PathBuf::from(format!("{}.tmp-tincan", path.display()));
    std::fs::write(&tmp, content)?;
    // Keep its permissions (a config holding tokens may be 0600).
    if let Ok(meta) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, meta.permissions())?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

fn write_json(path: &Path, v: &Value, dry: bool) -> Result<bool> {
    // Removing tincan from a config that never existed leaves an empty shell: don't create a file for that.
    if !path.exists() && hollow(v) {
        return Ok(false);
    }
    write_file(path, &(serde_json::to_string_pretty(v)? + "\n"), dry)
}

/// Only empty objects and arrays.
fn hollow(v: &Value) -> bool {
    match v {
        Value::Object(o) => o.values().all(hollow),
        Value::Array(a) => a.is_empty(),
        Value::Null => true,
        _ => false,
    }
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
/// Replaces any earlier tincan groups for `agent`, keeps everything else.
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
    if did { format!("updated {p}") } else { format!("{p} unchanged") }
}

// ---- Claude Code ----

fn claude(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    let Some(bin) = agent::find_bin("claude") else { return Ok(None) };
    let mut r = Report { agent: "Claude Code", lines: Vec::new(), notes: Vec::new() };
    let get = Command::new(&bin).args(["mcp", "get", "tincan"]).stdin(std::process::Stdio::null()).output()?;
    let existing = get.status.success();
    let current = existing && String::from_utf8_lossy(&get.stdout).contains(&exe.display().to_string());
    if remove || (existing && !current) {
        if existing && !dry {
            Command::new(&bin).args(["mcp", "remove", "tincan", "--scope", "user"]).stdin(std::process::Stdio::null()).output()?;
        }
        if remove {
            r.lines.push(if existing { "removed the tincan MCP server" } else { "nothing to remove" }.into());
            return Ok(Some(r));
        }
    }
    if current {
        r.lines.push("tincan MCP server already set".into());
    } else {
        if !dry {
            let out = Command::new(&bin)
                .args(["mcp", "add", "--scope", "user", "tincan", "--"])
                .arg(exe)
                .arg("mcp")
                .stdin(std::process::Stdio::null()).output()?;
            if !out.status.success() {
                return Err(anyhow!("claude mcp add failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        r.lines.push("added the tincan MCP server (user scope)".into());
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
            servers.remove("tincan");
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
        env.insert("TINCAN_AGENT", "codex".into());
        if let Ok(h) = std::env::var("TINCAN_HOME") {
            env.insert("TINCAN_HOME", h.into());
        }
        t["env"] = toml_edit::value(env);
        // Room for check_inbox(wait_secs) long-polls.
        t["tool_timeout_sec"] = toml_edit::value(660);
        t["default_tools_approval_mode"] = toml_edit::value("approve");
        servers["tincan"] = toml_edit::Item::Table(t);
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
            "Codex asks once to trust tincan's hooks: choose \"Trust all and continue\".".into(),
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
        servers.remove("tincan");
    } else {
        servers.insert(
            "tincan".into(),
            json!({ "type": "stdio", "command": exe.display().to_string(), "args": ["mcp"], "env": { "TINCAN_AGENT": "cursor" } }),
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
        servers.remove("tincan");
    } else {
        servers.insert(
            "tincan".into(),
            json!({ "command": exe.display().to_string(), "args": ["mcp"], "env": { "TINCAN_AGENT": "gemini" }, "timeout": 660000, "trust": true }),
        );
    }
    let group = |event: &str, timeout: u64| {
        json!({ "matcher": "*", "hooks": [{ "type": "command", "name": format!("tincan-{event}"), "command": shell_cmd(exe, &format!("hook gemini {event}")), "timeout": timeout }] })
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
        r.notes.push("Gemini CLI only runs tincan in trusted folders (`/permissions trust`).".into());
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
        servers.remove("tincan");
    } else {
        servers.insert(
            "tincan".into(),
            json!({ "type": "local", "command": exe.display().to_string(), "args": ["mcp"], "env": { "TINCAN_AGENT": "copilot" }, "tools": ["*"], "timeout": 660000 }),
        );
    }
    r.lines.push(changed(&mcp_path, write_json(&mcp_path, &v, dry)?));

    let hooks_path = dir.join("hooks").join("tincan.json");
    if remove {
        if hooks_path.exists() {
            if !dry {
                std::fs::remove_file(&hooks_path)?;
            }
            r.lines.push(format!("removed {}", crate::util::tilde(&hooks_path.display().to_string())));
        }
    } else {
        let shell = if cfg!(windows) { "powershell" } else { "bash" };
        let entry = |event: &str| json!([{ "type": "command", shell: shell_cmd(exe, &format!("hook copilot {event}")), "timeoutSec": 10 }]);
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
            "opencode: you use opencode.jsonc, which tincan doesn't edit. Add under \"mcp\": \"tincan\": {{\"type\": \"local\", \"command\": [\"{}\", \"mcp\"], \"environment\": {{\"TINCAN_AGENT\": \"opencode\"}}}}",
            exe.display()
        ));
    } else {
        let mut v = read_json(&path)?;
        let mcp = obj(obj(&mut v)?.entry("mcp").or_insert_with(|| json!({})))?;
        if remove {
            mcp.remove("tincan");
        } else {
            mcp.insert(
                "tincan".into(),
                json!({ "type": "local", "command": [exe.display().to_string(), "mcp"], "environment": { "TINCAN_AGENT": "opencode" }, "enabled": true }),
            );
        }
        r.lines.push(changed(&path, write_json(&path, &v, dry)?));
    }
    let plugin = dir.join("plugins").join("tincan.ts");
    if remove {
        if plugin.exists() {
            if !dry {
                std::fs::remove_file(&plugin)?;
            }
            r.lines.push(format!("removed {}", crate::util::tilde(&plugin.display().to_string())));
        }
    } else {
        let code = include_str!("opencode-plugin.ts").replace("__TINCAN_EXE__", &exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
        r.lines.push(changed(&plugin, write_file(&plugin, &code, dry)?));
    }
    Ok(Some(r))
}

// ---- pi ----

/// One extension does it all: it adds tincan's MCP server to each session (deferred, so the tools stay out of
/// the model's context until it looks for them) and delivers messages.
fn pi(exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    if !pi_home().exists() && agent::find_bin("pi").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "pi", lines: Vec::new(), notes: Vec::new() };
    let ext = pi_home().join("extensions").join("tincan.ts");
    if remove {
        if ext.exists() {
            if !dry {
                std::fs::remove_file(&ext)?;
            }
            r.lines.push(format!("removed {}", crate::util::tilde(&ext.display().to_string())));
        }
    } else {
        let code = include_str!("pi-extension.ts").replace("__TINCAN_EXE__", &exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
        r.lines.push(changed(&ext, write_file(&ext, &code, dry)?));
    }
    Ok(Some(r))
}

// ---- Reason ----

/// Reason's agent runs in its cloud: nothing to install there. The daemon lists your live Reason sessions and
/// delivers with `reason session send`; they reply with the tincan CLI.
fn reason(_exe: &Path, remove: bool, dry: bool) -> Result<Option<Report>> {
    if agent::find_bin("reason").is_none() {
        return Ok(None);
    }
    let mut r = Report { agent: "Reason", lines: Vec::new(), notes: Vec::new() };
    let mut cfg = Config::load_strict()?;
    if cfg.reason != remove {
        r.lines.push("already set".into());
    } else {
        if !dry {
            cfg.reason = !remove;
            cfg.save()?;
        }
        r.lines.push(if remove { "stopped listing Reason sessions" } else { "lists your live Reason sessions" }.into());
    }
    Ok(Some(r))
}

// ---- background service ----

#[cfg(unix)]
pub fn service_installed() -> bool {
    service_path().exists()
}

/// Asks the service manager to start the daemon now (rather than a stray one of our own, which would hold the
/// lock and leave the service's daemon waiting, without its menu bar icon). True if it accepted.
#[cfg(unix)]
pub fn start_service() -> bool {
    let out = if cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        Command::new("launchctl").args(["kickstart", &format!("gui/{uid}/{}", service_name())]).stdin(std::process::Stdio::null()).output()
    } else {
        Command::new("systemctl").args(["--user", "start", &format!("{}.service", service_name())]).stdin(std::process::Stdio::null()).output()
    };
    out.is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
fn service_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join(format!("Library/LaunchAgents/{}.plist", service_name()))
    } else {
        home().join(format!(".config/systemd/user/{}.service", service_name()))
    }
}

/// A PATH for the daemon that includes where agents usually live (it runs `codex queue`).
#[cfg(unix)]
fn service_path_env() -> String {
    let h = home().display().to_string();
    format!("{h}/.local/bin:{h}/.bun/bin:{h}/.npm-global/bin:{h}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
}

#[cfg(unix)]
async fn service(exe: &Path, remove: bool, dry: bool) -> Result<String> {
    let path = service_path();
    let uid = unsafe { libc::getuid() };
    let tincan_home = std::env::var("TINCAN_HOME").ok();
    if cfg!(target_os = "macos") {
        let target = format!("gui/{uid}/{}", service_name());
        if remove {
            if !dry {
                let _ = Command::new("launchctl").args(["bootout", &target]).stdin(std::process::Stdio::null()).output();
                let _ = std::fs::remove_file(&path);
            }
            return Ok("removed the login service".into());
        }
        let home_env = tincan_home.map(|h| format!("\n    <key>TINCAN_HOME</key><string>{}</string>", xml(&h))).unwrap_or_default();
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
        let loaded = Command::new("launchctl").args(["print", &target]).stdin(std::process::Stdio::null()).output().is_ok_and(|o| o.status.success());
        if !dry && (did || !loaded) {
            let _ = Command::new("launchctl").args(["bootout", &target]).stdin(std::process::Stdio::null()).output();
            crate::stop_daemon().await;
            let out = Command::new("launchctl").args(["bootstrap", &format!("gui/{uid}")]).arg(&path).stdin(std::process::Stdio::null()).output()?;
            if !out.status.success() {
                return Err(anyhow!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        Ok(if did || !loaded { "installed the login service (launchd)".into() } else { "login service already running".into() })
    } else {
        if remove {
            if !dry {
                let _ = Command::new("systemctl").args(["--user", "disable", "--now", &format!("{}.service", service_name())]).stdin(std::process::Stdio::null()).output();
                let _ = std::fs::remove_file(&path);
            }
            return Ok("removed the login service".into());
        }
        // Containers and WSL often have no user systemd: say so plainly, and leave no unit behind.
        let user_systemd = Command::new("systemctl")
            .args(["--user", "show-environment"])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        if !user_systemd {
            return Err(anyhow!("systemctl --user isn't available here"));
        }
        let home_env = tincan_home.map(|h| format!("Environment=\"TINCAN_HOME={h}\"\n")).unwrap_or_default();
        let unit = format!(
            "[Unit]\nDescription=tincan daemon\n\n[Service]\nExecStart={} daemon --service\nRestart=always\nRestartSec=3\nEnvironment=\"PATH={}\"\n{home_env}StandardError=append:{}\n\n[Install]\nWantedBy=default.target\n",
            shell_cmd(exe, "").trim_end(),
            service_path_env(),
            paths::log().display()
        );
        let did = write_file(&path, &unit, dry)?;
        if !dry {
            paths::ensure_home()?;
            crate::stop_daemon().await;
            Command::new("systemctl").args(["--user", "daemon-reload"]).stdin(std::process::Stdio::null()).output()?;
            let out = Command::new("systemctl").args(["--user", "enable", "--now", &format!("{}.service", service_name())]).stdin(std::process::Stdio::null()).output()?;
            if !out.status.success() {
                let _ = std::fs::remove_file(&path);
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

#[cfg(unix)]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// Without admin, Windows has no service for us: the daemon starts at logon from the user's Run key, and when it
// restarts it starts the next one itself (see `Daemon::restart_soon`).

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

#[cfg(windows)]
pub fn service_installed() -> bool {
    run_entry().is_some()
}

/// There is no service manager to ask: the caller starts the daemon.
#[cfg(windows)]
pub fn start_service() -> bool {
    false
}

/// Our Run entry, as `reg query` shows it.
#[cfg(windows)]
fn run_entry() -> Option<String> {
    let out = Command::new("reg").args(["query", RUN_KEY, "/v", &service_name()]).stdin(std::process::Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(windows)]
async fn service(exe: &Path, remove: bool, dry: bool) -> Result<String> {
    let reg = |args: &[&str]| Command::new("reg").args(args).stdin(std::process::Stdio::null()).output();
    if remove {
        if !dry {
            let _ = reg(&["delete", RUN_KEY, "/v", &service_name(), "/f"]);
            crate::stop_daemon().await;
        }
        return Ok("removed the login service".into());
    }
    // Windows starts it with its command line and nothing else.
    if std::env::var_os("TINCAN_HOME").is_some() {
        return Err(anyhow!("a logon entry can't set TINCAN_HOME"));
    }
    let command = format!("\"{}\" daemon --service", exe.display());
    let did = !run_entry().is_some_and(|v| v.contains(&command));
    if !dry {
        if did {
            let out = reg(&["add", RUN_KEY, "/v", &service_name(), "/d", &command, "/f"])?;
            if !out.status.success() {
                return Err(anyhow!("reg add: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        // Running now, as from logon: a daemon that is running starts it as it stops.
        if !crate::stop_daemon().await {
            crate::client::start_daemon(true)?;
        }
    }
    Ok(if did { "installed the login service (Run key)".into() } else { "login service already set".into() })
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

/// Our path as a JSON config has it (with Windows' backslashes escaped).
fn me(exe: &Path) -> String {
    let s = json!(exe.display().to_string()).to_string();
    s[1..s.len() - 1].to_string()
}

const AGENTS: [Agent; 8] = [
    Agent {
        id: "claude",
        name: "Claude Code",
        hint: "messages arrive instantly",
        installed: || agent::find_bin("claude").is_some(),
        wired: |_| {
            agent::find_bin("claude")
                .is_some_and(|b| Command::new(b).args(["mcp", "get", "tincan"]).stdin(std::process::Stdio::null()).output().is_ok_and(|o| o.status.success()))
        },
        apply: claude,
    },
    Agent {
        id: "codex",
        name: "Codex",
        hint: "instant; asks once to trust the hooks",
        installed: || agent::find_bin("codex").is_some() || codex_home().exists(),
        // TOML keeps a Windows path as it is ('C:\…').
        wired: |exe| has(codex_home().join("config.toml"), &exe.display().to_string()) && has(codex_home().join("hooks.json"), "hook codex"),
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
        wired: |_| opencode_home().join("plugins/tincan.ts").exists(),
        apply: opencode,
    },
    Agent {
        id: "pi",
        name: "pi",
        hint: "messages arrive within seconds",
        installed: || pi_home().exists() || agent::find_bin("pi").is_some(),
        wired: |_| pi_home().join("extensions/tincan.ts").exists(),
        apply: pi,
    },
    Agent {
        id: "reason",
        name: "Reason",
        hint: "messages arrive after its current turn",
        installed: || agent::find_bin("reason").is_some(),
        wired: |_| Config::load().reason,
        apply: reason,
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
    /// The agents set up (or removed) without trouble, and whether the service is installed now.
    pub agents: Vec<&'static str>,
    pub service: Option<bool>,
}

/// Wires tincan into the `install` agents and out of the `remove` ones; installs (`Some(true)`) or removes
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
                out.agents.push(a.name);
            }
            Ok(None) => {}
            Err(e) => {
                out.lines.push(format!("{}: skipped ({e:#})", a.name));
                out.notes.push(format!("{} not set up: {e:#}", a.name));
            }
        }
    }
    if let Some(on) = service {
        if std::env::var_os("TINCAN_NO_SERVICE").is_some() {
            out.lines.push("service: skipped (TINCAN_NO_SERVICE is set)".into());
            return out;
        }
        match self::service(exe, !on, dry).await {
            Ok(s) => {
                out.lines.push(format!("service: {s}"));
                out.service = Some(on);
            }
            Err(e) => {
                out.notes.push(format!("Not running in the background: {e:#}; tincan starts when you use it."));
            }
        }
    }
    out
}

/// Non-interactive setup: every agent found (or, with `remove`, every agent wired).
pub async fn run(dry: bool, remove: bool, no_service: bool) -> Result<()> {
    let exe = exe()?;
    let found = detect(&exe);
    let verb = if dry { "Would change" } else if remove { "Removing tincan" } else { "Setting up tincan" };
    println!("{verb} ({}):", crate::util::tilde(&exe.display().to_string()));
    if found.is_empty() && !remove {
        println!("  no supported agents found (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI, opencode, pi, Reason)");
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
    println!("tincan {} at {}", env!("CARGO_PKG_VERSION"), crate::util::tilde(&exe.display().to_string()));
    let link = Link::new(None);
    match link.call::<TeamInfo>(Request::Team).await {
        Ok(t) => {
            let online = t.machines.iter().filter(|m| !m.is_me && m.online).count();
            let others = t.machines.len().saturating_sub(1);
            println!("✓ daemon running; team {} with {others} other machine(s), {online} connected", t.name);
            print!("{}", render::team(&t).lines().skip(1).map(|l| format!("{l}\n")).collect::<String>());
            if others > 0 && online == 0 {
                println!("  ! no teammate connected: check your internet, or that their machines are on (`tincan doctor` there)");
            }
        }
        Err(e) if e.to_string().contains("not in a team") => {
            println!("✓ daemon running");
            println!("- not in a team yet: run `tincan`");
        }
        Err(e) => println!("✗ daemon: {e:#}"),
    }
    let svc = service_installed();
    println!(
        "{} login service: {}",
        if svc { "✓" } else { "-" },
        if svc { "installed" } else { "not installed (`tincan setup`)" }
    );
    for f in detect(&exe) {
        if f.wired {
            println!("✓ {}: wired up", f.name);
        } else {
            println!("- {}: installed, not on tincan (`tincan setup`)", f.name);
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
            "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "/x/tincan hook codex post-tool-use" }] }],
        }))
        .unwrap();
        let ours = |e: &str| json!({ "hooks": [{ "type": "command", "command": format!("/y/tincan hook codex {e} ") }] });
        merge_hook_groups(&mut hooks, "codex", vec![("Stop", ours("stop")), ("PostToolUse", ours("post-tool-use"))], false);
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 2, "keeps other tools' hooks");
        assert_eq!(hooks["PostToolUse"].as_array().unwrap().len(), 1, "replaces the old tincan hook");
        merge_hook_groups(&mut hooks, "codex", vec![], true);
        assert_eq!(hooks["Stop"].as_array().unwrap().len(), 1);
        assert!(!hooks.contains_key("PostToolUse"));
    }

    #[test]
    #[cfg(unix)]
    fn quoting() {
        assert_eq!(shell_cmd(Path::new("/usr/local/bin/tincan"), "mcp"), "/usr/local/bin/tincan mcp");
        assert_eq!(shell_cmd(Path::new("/Users/a b/tincan"), "mcp"), "'/Users/a b/tincan' mcp");
    }

    #[test]
    #[cfg(windows)]
    fn quoting() {
        assert_eq!(shell_cmd(Path::new(r"C:\Users\alex\.local\bin\tincan.exe"), "mcp"), r"C:\Users\alex\.local\bin\tincan.exe mcp");
        assert_eq!(shell_cmd(Path::new(r"C:\Users\Alex O'Neil\tincan.exe"), "mcp"), r"& 'C:\Users\Alex O''Neil\tincan.exe' mcp");
        assert_eq!(me(Path::new(r"C:\a\tincan.exe")), r"C:\\a\\tincan.exe");
    }
}
