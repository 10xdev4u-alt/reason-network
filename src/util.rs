use std::{fs::File, os::fd::AsRawFd, path::Path};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Short machine name: `Tals-MacBook-Pro.local` -> `tals-macbook-pro`. `COMMS_MACHINE` overrides it.
pub fn hostname() -> String {
    if let Some(name) = std::env::var("COMMS_MACHINE").ok().map(|n| slug(&n)).filter(|n| !n.is_empty()) {
        return name;
    }
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    let raw = if rc == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..end]).into_owned()
    } else {
        String::new()
    };
    let short = raw.split('.').next().unwrap_or_default();
    let s = slug(short);
    if s.is_empty() { "machine".into() } else { s }
}

pub fn pid_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn parent_pid() -> u32 {
    unsafe { libc::getppid() as u32 }
}

/// Lowercase letters, digits and single dashes: a name that is safe in an address.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect()
}

/// Compact age: `45s`, `12m`, `3h`, `2d`.
pub fn ago(then_ms: i64, now_ms: i64) -> String {
    let s = ((now_ms - then_ms) / 1000).max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

pub fn tilde(path: &str) -> String {
    let home = crate::paths::user_home();
    match Path::new(path).strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.into(),
    }
}

/// Takes an exclusive lock on `path` (waiting for it if `wait`) and writes our pid into it. `None` means
/// someone else holds it.
pub fn lock(path: &Path, wait: bool) -> std::io::Result<Option<File>> {
    use std::io::Write;
    let mut f = File::options().create(true).truncate(false).write(true).open(path)?;
    let flags = if wait { libc::LOCK_EX } else { libc::LOCK_EX | libc::LOCK_NB };
    if unsafe { libc::flock(f.as_raw_fd(), flags) } != 0 {
        return Ok(None);
    }
    f.set_len(0)?;
    write!(f, "{}", std::process::id())?;
    Ok(Some(f))
}

pub fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Tals-MacBook-Pro"), "tals-macbook-pro");
        assert_eq!(slug("  my repo (v2)!"), "my-repo-v2");
        assert_eq!(slug("--"), "");
    }

    #[test]
    fn ages() {
        assert_eq!(ago(0, 59_000), "59s");
        assert_eq!(ago(0, 125_000), "2m");
        assert_eq!(ago(0, 7_200_000), "2h");
        assert_eq!(ago(0, 200_000_000), "2d");
        assert_eq!(ago(10, 0), "0s");
    }
}
