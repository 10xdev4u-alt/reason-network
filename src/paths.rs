//! Where tincan keeps its state. `TINCAN_HOME` overrides `~/.tincan` (handy for running two "machines" side by side).
//! On Windows, `~` is the profile folder.

use std::path::{Path, PathBuf};

pub fn user_home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn home() -> PathBuf {
    std::env::var_os("TINCAN_HOME").map(PathBuf::from).unwrap_or_else(|| user_home().join(".tincan"))
}

/// `~/.tincan/tincan.sock`, unless that is too long for a Unix socket address (104 bytes on macOS): then a short
/// path in a private `/tmp/tincan-<uid>` directory, derived from the home directory so every client agrees on it.
#[cfg(unix)]
pub fn socket() -> PathBuf {
    let path = home().join("tincan.sock");
    if path.as_os_str().len() < 100 {
        return path;
    }
    short_dir().join(format!("{:016x}.sock", hash(&path)))
}

/// A named pipe, named for the home directory so every client agrees on it.
#[cfg(windows)]
pub fn socket() -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\tincan-{:016x}", hash(&home())))
}

/// FNV-1a: stable across builds, unlike `DefaultHasher`.
fn hash(path: &Path) -> u64 {
    path.as_os_str().as_encoded_bytes().iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(unix)]
fn short_dir() -> PathBuf {
    PathBuf::from(format!("/tmp/tincan-{}", unsafe { libc::getuid() }))
}

/// Makes sure the socket's directory exists and that nobody else can get at it.
#[cfg(unix)]
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
    home().join("tincan.db")
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
