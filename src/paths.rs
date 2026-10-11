//! Where comms keeps its state. `COMMS_HOME` overrides `~/.comms` (handy for running two "machines" side by side).

use std::path::PathBuf;

pub fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn home() -> PathBuf {
    std::env::var_os("COMMS_HOME").map(PathBuf::from).unwrap_or_else(|| user_home().join(".comms"))
}

/// `~/.comms/comms.sock`, unless that is too long for a Unix socket address (104 bytes on macOS): then a short
/// path in a private `/tmp/comms-<uid>` directory, derived from the home directory so every client agrees on it.
pub fn socket() -> PathBuf {
    let path = home().join("comms.sock");
    if path.as_os_str().len() < 100 {
        return path;
    }
    // FNV-1a: stable across builds, unlike `DefaultHasher`.
    let hash = path.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    });
    short_dir().join(format!("{hash:016x}.sock"))
}

fn short_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/comms-{}", unsafe { libc::getuid() }))
}

/// Makes sure the socket's directory exists and that nobody else can get at it.
pub fn ensure_socket_dir() -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let dir = socket().parent().map(PathBuf::from).unwrap_or_else(home);
    if dir != short_dir() {
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
    home().join("comms.db")
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

pub fn ensure_home() -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(home())
}
