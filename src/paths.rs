//! Where Reason Network keeps its state: `~/.reason/network`, or `REASON_NETWORK_HOME` (handy for running two
//! "machines" side by side). On Windows, `~` is the profile folder.
//!
//! Builds before 0.10.1 kept it in `~/.<LEGACY_NAME>` (or `TINCAN_HOME`) as `<LEGACY_NAME>.db` and `.sock`. [`migrate`]
//! moves it here once, with the daemon that holds it stopped. Until that has happened, every path below still names
//! the old state, so a machine never loses its team.

use crate::LEGACY_NAME;
use std::path::{Path, PathBuf};

pub fn user_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// A setting from the environment: `REASON_NETWORK_<name>`, else the name earlier builds read (which earlier Reason
/// CLIs and agent configs set).
pub fn env_var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("REASON_NETWORK_{name}")).or_else(|_| std::env::var(legacy_env(name)))
}

pub fn env_var_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("REASON_NETWORK_{name}")).or_else(|| std::env::var_os(legacy_env(name)))
}

fn legacy_env(name: &str) -> String {
    format!("{}_{name}", LEGACY_NAME.to_uppercase())
}

fn default_home() -> PathBuf {
    user_home().join(".reason").join("network")
}

fn legacy_home() -> PathBuf {
    user_home().join(format!(".{LEGACY_NAME}"))
}

pub fn home() -> PathBuf {
    if let Some(h) = env_var_os("HOME") {
        return h.into();
    }
    let home = default_home();
    if !home.exists() && legacy_home().is_dir() {
        return legacy_home();
    }
    home
}

/// What the state files are called in `home`: `network`, or the old name while they haven't been moved.
fn stem(home: &Path) -> &'static str {
    if !home.join("network.db").exists() && home.join(format!("{LEGACY_NAME}.db")).exists() {
        LEGACY_NAME
    } else {
        "network"
    }
}

/// `~/.reason/network/network.sock`, unless that is too long for a Unix socket address (104 bytes on macOS): then a
/// short path in a private `/tmp/reason-network-<uid>` directory, derived from the home directory so every client
/// agrees on it.
#[cfg(unix)]
pub fn socket() -> PathBuf {
    socket_in(&home())
}

#[cfg(unix)]
fn socket_in(home: &Path) -> PathBuf {
    let stem = stem(home);
    let path = home.join(format!("{stem}.sock"));
    if path.as_os_str().len() < 100 {
        return path;
    }
    short_dir(stem).join(format!("{:016x}.sock", hash(&path)))
}

/// A named pipe, named for the home directory so every client agrees on it.
#[cfg(windows)]
pub fn socket() -> PathBuf {
    let home = home();
    let stem = if stem(&home) == LEGACY_NAME { LEGACY_NAME } else { "reason-network" };
    PathBuf::from(format!(r"\\.\pipe\{stem}-{:016x}", hash(&home)))
}

/// FNV-1a: stable across builds, unlike `DefaultHasher`.
fn hash(path: &Path) -> u64 {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(unix)]
fn short_dir(stem: &str) -> PathBuf {
    let name = if stem == LEGACY_NAME { LEGACY_NAME } else { "reason-network" };
    PathBuf::from(format!("/tmp/{name}-{}", unsafe { libc::getuid() }))
}

/// Moves state an earlier build left under the old names (see the module docs), once. Every command runs it first;
/// it costs a stat or two when there is nothing to move. One process moves while the others wait for it, and anything
/// that goes wrong leaves the old state in place, where [`home`] keeps finding it, to be moved next time.
pub fn migrate() {
    #[cfg(unix)]
    let _ = migrate_unix();
}

#[cfg(unix)]
fn migrate_unix() -> std::io::Result<()> {
    let explicit = env_var_os("HOME").map(PathBuf::from);
    let to = explicit.clone().unwrap_or_else(default_home);
    let folder = || explicit.is_none() && !to.exists() && legacy_home().is_dir();
    let files = || stem(&to) == LEGACY_NAME;
    if !folder() && !files() {
        return Ok(());
    }
    let dir = user_home().join(".reason");
    std::fs::create_dir_all(&dir)?;
    let Some(_moving) = crate::util::lock(&dir.join(".network-move.lock"), true)? else { return Ok(()) };
    if folder() {
        stop_daemon_in(&legacy_home())?;
        std::fs::rename(legacy_home(), &to)?;
    }
    if files() {
        stop_daemon_in(&to)?;
        // The write-ahead log first: until the database itself is renamed, this all runs again and finishes.
        for suffix in ["-wal", ""] {
            let (old, new) = (to.join(format!("{LEGACY_NAME}.db{suffix}")), to.join(format!("network.db{suffix}")));
            if old.exists() && !new.exists() {
                std::fs::rename(old, new)?;
            }
        }
        let _ = std::fs::remove_file(to.join(format!("{LEGACY_NAME}.db-shm")));
        let _ = std::fs::remove_file(to.join(format!("{LEGACY_NAME}.sock")));
        // The earlier build's log talks about its own names; the moved state starts a fresh one.
        let _ = std::fs::remove_file(to.join("daemon.log"));
    }
    Ok(())
}

/// Asks the daemon holding `home` (an earlier build, on the old names) to exit, and waits until it has.
#[cfg(unix)]
fn stop_daemon_in(home: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let lock = home.join("daemon.lock");
    if !crate::util::locked(&lock) {
        return Ok(());
    }
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(socket_in(home)) {
        let _ = s.write_all(b"{\"id\":1,\"op\":\"shutdown\"}\n");
    }
    for _ in 0..100 {
        if !crate::util::locked(&lock) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(std::io::Error::other("the daemon on the old state didn't stop"))
}

/// Makes sure the socket's directory exists and that nobody else can get at it.
#[cfg(unix)]
pub fn ensure_socket_dir() -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = socket().parent().map(PathBuf::from).unwrap_or_else(home);
    if dir != short_dir("network") && dir != short_dir(LEGACY_NAME) {
        return ensure_home();
    }
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    let meta = std::fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } || meta.mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!("{} is not a private directory of yours; remove it", dir.display())));
    }
    Ok(())
}

pub fn db() -> PathBuf {
    let home = home();
    home.join(format!("{}.db", stem(&home)))
}

pub fn config() -> PathBuf {
    home().join("config.toml")
}

pub fn log() -> PathBuf {
    home().join("daemon.log")
}

pub fn lock() -> PathBuf {
    home().join("daemon.lock")
}

/// Claude Code's own directory (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| user_home().join(".claude"))
}

/// Claude Code's registry of live sessions, one `<pid>.json` per session.
pub fn claude_sessions() -> PathBuf {
    claude_dir().join("sessions")
}

/// Only for you: mode 0700 (on Windows, the profile folder already is).
pub fn ensure_home() -> std::io::Result<()> {
    let mut dir = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.recursive(true).create(home())
}
