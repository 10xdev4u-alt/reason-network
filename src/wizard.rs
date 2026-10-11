//! The guided setup: what `comms`, `comms init`, `comms join`, `comms invite` and `comms setup` do when a person
//! runs them in a terminal. Without a terminal (an agent running them), the commands use defaults and flags.

use crate::{
    INSTALL_URL,
    client::Link,
    config::{Config, Policy},
    invite,
    proto::*,
    render, setup, util,
};
use anyhow::{Result, anyhow};
use cliclack::{confirm, input, intro, log, multiselect, note, outro, outro_cancel, select, spinner};
use std::io::IsTerminal;

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal() && std::env::var_os("COMMS_NO_PROMPT").is_none()
}

/// Runs a guided flow, turning Ctrl-C at a prompt into a quiet exit.
pub async fn guided(f: impl std::future::Future<Output = Result<()>>) -> Result<()> {
    match f.await {
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::Interrupted) => {
            let _ = outro_cancel("Stopped. Run `comms` to pick up where you left off.");
            Ok(())
        }
        r => r,
    }
}

fn name_ok(s: &str) -> Result<(), &'static str> {
    if util::slug(s).is_empty() { Err("letters or digits, please") } else { Ok(()) }
}

/// Start or join a team, then set up agents. `start`: `Some(true)` from `comms init`, `Some(false)` from
/// `comms join`, `None` to ask.
pub async fn onboard(start: Option<bool>, team: Option<String>, code: Option<String>, person: Option<String>) -> Result<()> {
    intro(" comms ")?;
    let link = Link::new(None);
    if let Ok(t) = link.call::<TeamInfo>(Request::Team).await {
        log::info(format!("You're in team {} as {} on {}.", t.name, t.person, t.machine))?;
        outro("`comms` shows who's around; `comms invite` adds someone; `comms setup` changes your agents.")?;
        return Ok(());
    }
    let mut person = match person {
        Some(p) => util::slug(&p),
        None => util::slug(
            &input("What should teammates call you?").default_input(&Config::load().person()).validate(|s: &String| name_ok(s)).interact::<String>()?,
        ),
    };
    let start = match (start, &code) {
        (Some(s), _) => s,
        (None, Some(_)) => false,
        (None, None) => select("Start a new team, or join one?")
            .item(false, "Join a team", "I have an invite code")
            .item(true, "Start a new team", "")
            .interact()?,
    };

    if start {
        let name: String = match team {
            Some(t) => t,
            None => input("Name your team").placeholder("vibes").validate(|s: &String| name_ok(s)).interact()?,
        };
        let sp = spinner();
        sp.start("Creating the team...");
        let t: TeamInfo = link.call(Request::CreateTeam { name, person }).await.inspect_err(|e| sp.error(format!("{e:#}")))?;
        crate::wait_exit(crate::daemon_pid()).await;
        sp.stop(format!("Created team {}. You are {} on {}.", t.name, t.person, t.machine));
    } else {
        let mut code = code;
        loop {
            let c: String = match code.take() {
                Some(c) => c,
                None => input("Invite code")
                    .placeholder("plum-tiger-lake-orbit")
                    .validate(|s: &String| {
                        if invite::normalize(s).is_some() || s.trim().starts_with("comms1") {
                            Ok(())
                        } else {
                            Err("four words, like plum-tiger-lake-orbit")
                        }
                    })
                    .interact()?,
            };
            let sp = spinner();
            sp.start("Finding the team...");
            match link.call::<TeamInfo>(Request::Join { ticket: c.clone(), person: person.clone() }).await {
                Ok(t) => {
                    crate::wait_exit(crate::daemon_pid()).await;
                    sp.stop(format!("Joined team {} as {} on {}.", t.name, t.person, t.machine));
                    break;
                }
                Err(e) if e.to_string().contains("already in this team") => {
                    sp.error(format!("Someone in the team is already called {person}."));
                    person = util::slug(&input("Pick another name").validate(|s: &String| name_ok(s)).interact::<String>()?);
                    code = Some(c);
                }
                Err(e) => {
                    sp.error(format!("{e:#}"));
                    if !confirm("Try again?").initial_value(true).interact()? {
                        return Err(anyhow!("not joined"));
                    }
                    // Unreachable: the same code may work in a moment. Anything else: ask for a new one.
                    code = e.to_string().contains("reach").then_some(c);
                }
            }
        }
    }

    agents_and_preferences(true).await?;

    if start {
        if confirm("Invite a teammate now?").initial_value(true).interact()? {
            show_invite(false, 24).await?;
        }
    } else if let Ok(t) = Link::new(None).call::<TeamInfo>(Request::Team).await {
        note("Your team", render::team(&t).trim_end())?;
    }
    outro("All set. Restart agent sessions that were already running, then ask one: \"who's on the team?\"")?;
    Ok(())
}

/// Which agents are on comms, the background service, and how teammates' messages are handled.
pub async fn agents_and_preferences(first: bool) -> Result<()> {
    let exe = setup::exe()?;
    let found = setup::detect(&exe);
    let chosen: Vec<&str> = if found.is_empty() {
        log::warning("No coding agents found here (Claude Code, Codex, Cursor, Gemini CLI, Copilot CLI, opencode). Install one, then run `comms setup`.")?;
        Vec::new()
    } else {
        let initial: Vec<&str> = found.iter().filter(|f| first || f.wired).map(|f| f.id).collect();
        let mut pick = multiselect("Which agents should be on comms?  (space to toggle, enter to confirm)").required(false).initial_values(initial);
        for f in &found {
            pick = pick.item(f.id, f.name, f.hint);
        }
        pick.interact()?
    };
    let service = confirm("Keep comms running in the background, so teammates can always reach you?").initial_value(true).interact()?;
    let mut cfg = Config::load_strict().map_err(|e| anyhow!("fix {} first: {e}", crate::paths::config().display()))?;
    cfg.inbound.default = select("When a teammate's agent messages one of yours")
        .item(Policy::Accept, "Deliver it", "recommended")
        .item(Policy::Hold, "Hold it until I approve", "you approve with `comms held`")
        .initial_value(cfg.inbound.default)
        .interact()?;
    cfg.share_paths = confirm("Show teammates your full working directories? (otherwise just the folder name)")
        .initial_value(cfg.share_paths)
        .interact()?;
    cfg.save()?;

    let remove: Vec<&str> = found.iter().filter(|f| f.wired && !chosen.contains(&f.id)).map(|f| f.id).collect();
    let sp = spinner();
    sp.start("Setting up...");
    // The daemon reads its config at start: restart it (the service manager or the next command brings it back).
    crate::stop_daemon().await;
    let service = if service { Some(true) } else { setup::service_installed().then_some(false) };
    let out = setup::apply(&exe, &chosen, &remove, service, false).await;
    sp.stop("Done");
    for l in &out.lines {
        log::success(l)?;
    }
    for n in &out.notes {
        log::warning(n)?;
    }
    Ok(())
}

/// Makes an invite and shows it, ready to send.
pub async fn show_invite(me: bool, hours: u64) -> Result<()> {
    let i: InviteInfo = Link::new(None).call(Request::Invite { me, hours }).await?;
    let one_liner = format!("curl -fsSL {INSTALL_URL} | sh -s -- {}", i.code);
    let copied = copy(&one_liner);
    let who = if me { "your other machine" } else { "your teammate" };
    note(
        format!("Invite code: {}", i.code),
        format!(
            "Send {who} this line, which installs comms and joins the team:\n\n  {one_liner}\n\n{}Already have comms? `comms join {}`\nWorks once, within {}h, while this machine is online.",
            if copied { "(It's on your clipboard.)\n" } else { "" },
            i.code,
            i.hours
        ),
    )?;
    Ok(())
}

/// Puts text on the clipboard, if there's a clipboard tool. True if it worked.
fn copy(text: &str) -> bool {
    use std::io::Write;
    let tools: [(&str, &[&str]); 3] = [("pbcopy", &[]), ("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];
    tools.iter().any(|(bin, args)| {
        let Ok(mut child) = std::process::Command::new(bin)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return false;
        };
        let wrote = child.stdin.take().is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
        child.wait().is_ok_and(|st| st.success()) && wrote
    })
}

/// `comms invite` in a terminal: ask who it's for.
pub async fn invite(hours: u64) -> Result<()> {
    intro(" comms invite ")?;
    let me = select("Who's this invite for?").item(false, "A teammate", "").item(true, "Another machine of mine", "").interact()?;
    show_invite(me, hours).await?;
    outro("They'll show up in `comms` once they've joined.")?;
    Ok(())
}

/// `comms setup` in a terminal.
pub async fn setup_agents() -> Result<()> {
    intro(" comms setup ")?;
    agents_and_preferences(false).await?;
    outro("Restart agent sessions that were already running to pick up changes.")?;
    Ok(())
}
