//! A machine that ran tincan on its own moves over once. tincan's last update installed reason-network beside it,
//! with a copy under the old name for what still starts that: the old login service, agents' configs, and the old
//! updater checking the version. `paths` has already moved the state; the daemon's first start here rewires the
//! agents and swaps the login service. Agent sessions started before keep working until they end.

use crate::{AGENT_NAME, LEGACY_NAME, setup, util};

/// Started under the old name, runs as reason-network instead, so everything after this is our own binary.
pub fn as_ours() {
    let Ok(me) = util::current_exe() else { return };
    let ours = me.with_file_name(format!("{AGENT_NAME}{}", std::env::consts::EXE_SUFFIX));
    if me.file_stem().is_none_or(|s| s != LEGACY_NAME) || !ours.exists() {
        return;
    }
    let mut cmd = std::process::Command::new(ours);
    cmd.args(std::env::args_os().skip(1));
    #[cfg(unix)]
    eprintln!("{AGENT_NAME}: {}", std::os::unix::process::CommandExt::exec(&mut cmd));
    #[cfg(windows)]
    if let Ok(status) = cmd.status() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Moves this machine's agents and login service over from the tincan beside us, if that hasn't happened yet.
/// Lines go to `log`.
pub async fn run(log: fn(&str)) {
    let from_source = util::current_exe().is_ok_and(|p| p.components().any(|c| c.as_os_str() == "target"));
    let Ok(exe) = setup::exe() else { return };
    let old = setup::legacy_exe(&exe);
    if from_source || crate::managed() || !old.exists() {
        return;
    }
    #[cfg(unix)]
    let service = crate::paths::env_var_os("NO_SERVICE").is_none() && setup::unregister_legacy_service(&exe);
    #[cfg(windows)]
    setup::move_legacy_run_entry(&exe);
    let out = setup::apply(&exe, &setup::legacy_wiring(&exe), &[], None, false).await;
    for line in out.lines.iter().chain(&out.notes) {
        log(&format!("moved over from {LEGACY_NAME}: {line}"));
    }
    #[cfg(unix)]
    if service {
        match setup::install_service_from_daemon(&exe).await {
            Ok(s) => log(&format!("moved over from {LEGACY_NAME}: service: {s}")),
            Err(e) => log(&format!("moved over from {LEGACY_NAME}: service not installed: {e:#}")),
        }
    }
    // The copy tincan's update left under the old name. (Windows keeps it while it still runs; the next start goes.)
    if std::fs::read(&old).is_ok_and(|b| std::fs::read(&exe).is_ok_and(|o| o == b)) {
        let _ = std::fs::remove_file(&old);
    }
    // Last: the old service may be what runs this very process. Ours takes over once it has gone.
    #[cfg(unix)]
    if service {
        setup::stop_legacy_service();
    }
}
