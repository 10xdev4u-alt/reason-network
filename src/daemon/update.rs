//! Keeps tincan up to date: every few hours the daemon looks for a newer release and, if there is one, installs it
//! over its own binary (with install.sh, checksum verified) and restarts on it. Off with `auto_update = false`;
//! never for builds run from a source tree.

use super::{Daemon, log};
use anyhow::{Result, bail};
use std::{sync::Arc, time::Duration};

const EVERY: Duration = Duration::from_secs(6 * 3600);

impl Daemon {
    pub(super) async fn update_loop(self: Arc<Self>) {
        tokio::time::sleep(Duration::from_secs(90)).await;
        loop {
            match update().await {
                Ok(Some(v)) => {
                    log(&format!("updated to tincan {v}; restarting on it"));
                    self.restart_soon();
                    return;
                }
                Ok(None) => {}
                Err(e) => log(&format!("checking for updates: {e:#}")),
            }
            tokio::time::sleep(EVERY).await;
        }
    }
}

/// Whether this binary may replace itself.
pub(super) fn allowed(enabled: bool) -> bool {
    let from_source = std::env::current_exe().is_ok_and(|p| p.components().any(|c| c.as_os_str() == "target"));
    enabled && !from_source && std::env::var_os("TINCAN_NO_UPDATE").is_none()
}

/// Installs the latest release if it is newer than this one; returns its version.
async fn update() -> Result<Option<String>> {
    let latest = latest_version().await?;
    if !newer(&latest, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let dir = std::env::current_exe()?.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("curl -fsSL {} | sh", crate::install_url()))
        .env("TINCAN_BIN_DIR", dir)
        .env("TINCAN_NO_PROMPT", "1")
        .stdin(std::process::Stdio::null())
        .output();
    let out = tokio::time::timeout(Duration::from_secs(300), out).await??;
    if !out.status.success() {
        bail!("install failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(Some(latest))
}

/// The newest release's version, from where GitHub's "latest release" link redirects.
async fn latest_version() -> Result<String> {
    let repo = std::env::var("TINCAN_REPO").unwrap_or_else(|_| "asdf9384/tincan".into());
    let out = tokio::process::Command::new("curl")
        .args(["-fsSLI", "-o", "/dev/null", "-w", "%{url_effective}", &format!("https://github.com/{repo}/releases/latest")])
        .stdin(std::process::Stdio::null())
        .output();
    let out = tokio::time::timeout(Duration::from_secs(60), out).await??;
    let url = String::from_utf8_lossy(&out.stdout).to_string();
    match url.rsplit_once("/tag/v") {
        Some((_, v)) if out.status.success() && !v.is_empty() => Ok(v.trim().to_string()),
        _ => bail!("no release found at {url}"),
    }
}

/// `a` is a later version than `b` (`major.minor.patch`).
fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_by_number() {
        assert!(super::newer("0.10.0", "0.9.3"));
        assert!(super::newer("1.0.0", "0.99.99"));
        assert!(!super::newer("0.6.3", "0.6.3"));
        assert!(!super::newer("0.6.2", "0.6.10"));
    }
}
