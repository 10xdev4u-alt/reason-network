mod agent;
mod claude;
mod client;
mod config;
mod daemon;
mod doing;
mod git;
mod hook;
mod invite;
mod mcp;
mod net;
mod paths;
mod proto;
mod render;
mod setup;
mod store;
mod team;
mod tray;
mod util;
mod wizard;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use client::Link;
use proto::*;
use std::{io::Read, time::Duration};

/// The project's site, which serves `install.sh` and the join page. Release builds set `TINCAN_SITE_URL` to the
/// project's domain.
pub const SITE_URL: &str = match option_env!("TINCAN_SITE_URL") {
    Some(u) if !u.is_empty() => u,
    _ => "https://asdf9384.github.io/tincan",
};

pub fn install_url() -> String {
    format!("{SITE_URL}/install.sh")
}

/// The team link: a page showing the one command that installs tincan and joins with `ticket`.
pub fn join_url(ticket: &str) -> String {
    format!("{SITE_URL}/join/#{ticket}")
}

#[derive(Parser)]
#[command(
    name = "tincan",
    version,
    about = "Your team's coding agents, talking to each other",
    after_help = "Run `tincan` on its own to get set up, then to see who's working on what."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show who is working on what, across the team
    Who {
        /// Only sessions matching this text
        filter: Option<String>,
        /// Include offline sessions and machines
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Send a message to a session (`person/session`), a person, `repo:<name>` or `*`
    Send {
        to: String,
        /// The message; read from stdin when omitted or `-`
        message: Vec<String>,
        #[arg(long)]
        reply_to: Option<String>,
        /// Don't wake the recipient; it reads this at its next natural pause
        #[arg(long)]
        fyi: bool,
        /// Send as this agent session here (for agents that reply through the CLI)
        #[arg(long, hide = true)]
        from: Option<String>,
    },
    /// Read your messages (or, inside an agent's shell, that session's)
    Inbox {
        /// Wait up to this many seconds for a message
        #[arg(long, default_value_t = 0)]
        wait: u64,
        /// Show without marking as read
        #[arg(long)]
        peek: bool,
        #[arg(long)]
        json: bool,
    },
    /// Set what the current agent session is doing (run from inside the agent's shell)
    Status {
        doing: Vec<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Start a team, with you as its first member
    Init {
        team: Option<String>,
        /// Your name in the team (default: $USER)
        #[arg(long = "as")]
        person: Option<String>,
        /// Don't configure agents or install the background service
        #[arg(long)]
        no_setup: bool,
    },
    /// Show the team link to send friends (or, with --me / --code, a one-time code)
    Invite {
        /// A one-time code for another machine of yours
        #[arg(long)]
        me: bool,
        /// A one-time four-word code instead of the link
        #[arg(long)]
        code: bool,
        /// Hours until it expires
        #[arg(long, default_value_t = 24)]
        hours: u64,
    },
    /// Show the team link (anyone with it can join)
    Link {
        /// Replace it: the old link stops working
        #[arg(long)]
        new: bool,
    },
    /// Join a team with a team link (or an invite code)
    Join {
        invite: Option<String>,
        /// Your name in the team (default: $USER)
        #[arg(long = "as")]
        person: Option<String>,
        #[arg(long)]
        no_setup: bool,
    },
    /// Show the team's people and machines, and how each is reached
    Team,
    /// Remove a person (all their machines) or one machine from the team
    Remove { who: String },
    /// Leave the team on this machine
    Leave {
        /// Leave even if no teammate is online to hear it
        #[arg(long)]
        force: bool,
    },
    /// Use your own iroh relay for the team (or `default` for n0's public ones)
    Relay { url: String },
    /// List messages waiting for your approval (when `inbound` is `hold`)
    Held,
    /// Deliver held messages
    Approve {
        ids: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Drop held messages
    Reject {
        ids: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Wire tincan into the agents on this machine and install the background service
    Setup {
        /// Show what would change without changing anything
        #[arg(long)]
        dry_run: bool,
        /// Undo: remove tincan from every agent and remove the service
        #[arg(long)]
        remove: bool,
        /// Don't install the background service
        #[arg(long)]
        no_service: bool,
    },
    /// Check that everything is working
    Doctor,
    /// Install the latest release over this one
    Upgrade,
    /// Called by agents' hooks
    #[command(hide = true)]
    Hook {
        agent: String,
        event: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        doing: Option<String>,
    },
    /// Run the daemon in the foreground (normally started on demand or by the service)
    Daemon {
        /// Wait for a running daemon to exit instead of quitting (for service managers)
        #[arg(long)]
        service: bool,
    },
    /// Restart the daemon
    Stop,
    /// Run the MCP server on stdio (what agents launch)
    Mcp,
    /// The menu bar icon (macOS; the background service starts it)
    #[command(hide = true)]
    Tray,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().cmd {
        Some(Cmd::Daemon { service }) => daemon::run(service).await,
        Some(Cmd::Mcp) => mcp::run().await,
        // Blocks this (the main) thread with AppKit's event loop; its daemon calls run on the runtime's workers.
        Some(Cmd::Tray) => tray::run(),
        Some(Cmd::Hook { agent, event, session, state, doing }) => {
            hook::run(&agent, &event, session, state, doing).await;
            // Don't wait on anything left behind (a stuck stdin read): the agent is waiting on us.
            std::process::exit(0)
        }
        Some(cmd) => cli(cmd).await,
        // `tincan` alone: get set up if needed, else show who's around.
        None => {
            let in_team = Link::new(None).call::<TeamInfo>(Request::Team).await.is_ok();
            if !in_team && wizard::interactive() {
                return wizard::guided(wizard::onboard(None, None, None, None)).await;
            }
            if !in_team {
                println!("Not in a team yet. Run `tincan` in a terminal to get set up (or `tincan init <team>` / `tincan join <code>`).");
                return Ok(());
            }
            cli(Cmd::Who { filter: None, all: false, json: false }).await
        }
    }
}

/// The agent session this shell belongs to, if it runs inside one.
fn current_session() -> Option<String> {
    let agent = agent::detect_env()?;
    Some(format!("{agent}:{}", agent::env_session_id(agent)?))
}

fn default_person(p: Option<String>) -> String {
    p.unwrap_or_else(|| config::Config::load().person())
}

async fn cli(cmd: Cmd) -> Result<()> {
    let link = Link::new(None);
    match cmd {
        Cmd::Who { filter, all, json } => {
            let d: Directory = link.call(Request::Who { me: current_session(), filter, all }).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                print!("{}", render::directory(&d, false, render::Paint::terminal()));
            }
        }
        Cmd::Send { to, message, reply_to, fyi, from } => {
            let mut body = message.join(" ");
            if body.is_empty() || body == "-" {
                body.clear();
                std::io::stdin().read_to_string(&mut body)?;
            }
            let req = Request::Send { from: from.or_else(current_session), to, body, reply_to, wake: !fyi };
            let r: SendResult = link.call(req).await?;
            print!("{}", render::send_result(&r));
        }
        Cmd::Inbox { wait, peek, json } => {
            let key = current_session();
            let i: Inbox = link.call(Request::Inbox { key: key.clone(), wait_secs: wait, peek: true }).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&i.messages)?);
            } else if i.messages.is_empty() {
                println!("No messages.");
            } else {
                print!("{}", render::for_human(&i.messages));
            }
            // Marked read only once shown.
            if !peek && !i.messages.is_empty() {
                let ids = i.messages.iter().map(|m| m.id.clone()).collect();
                link.call::<serde_json::Value>(Request::Delivered { key: key.unwrap_or_default(), ids }).await?;
            }
        }
        Cmd::Status { doing, name } => {
            let Some(key) = current_session() else {
                bail!("run this from inside an agent session's shell");
            };
            let doing = (!doing.is_empty()).then(|| doing.join(" "));
            let r: Registered = link.call(Request::SetStatus { key, doing, name }).await?;
            println!("You are {}.", r.address);
        }
        Cmd::Init { team, person, no_setup } if !no_setup && wizard::interactive() => {
            wizard::guided(wizard::onboard(Some(true), team, None, person)).await?
        }
        Cmd::Join { invite, person, no_setup } if !no_setup && wizard::interactive() => {
            wizard::guided(wizard::onboard(Some(false), None, invite, person)).await?
        }
        Cmd::Invite { me: false, code: false, .. } if wizard::interactive() => wizard::guided(wizard::invite()).await?,
        Cmd::Link { new } => {
            let l: LinkInfo = link.call(Request::Link { new }).await?;
            let copied = wizard::interactive() && wizard::copy(&l.url);
            println!("Team link for {}{}:\n\n{}\n", l.team, if copied { " (on your clipboard)" } else { "" }, l.url);
            println!("Anyone with it can join: it opens a page with one command. `tincan link --new` replaces it.");
        }
        Cmd::Invite { me: false, code: false, .. } => Box::pin(cli(Cmd::Link { new: false })).await?,
        Cmd::Setup { dry_run: false, remove: false, no_service: false } if wizard::interactive() => {
            wizard::guided(wizard::setup_agents()).await?
        }
        Cmd::Init { team, person, no_setup } => {
            let Some(team) = team else { bail!("name the team: `tincan init <team>` (or run it in a terminal to be asked)") };
            let t: TeamInfo = link.call(Request::CreateTeam { name: team, person: default_person(person) }).await?;
            wait_exit(daemon_pid()).await;
            println!("Created team {}. You are {} on {}.", t.name, t.person, t.machine);
            if !no_setup {
                println!();
                setup::run(false, false, false).await?;
            }
            println!("\nNext: `tincan link` prints the team link to send your teammates.");
        }
        Cmd::Join { invite, person, no_setup } => {
            let Some(invite) = invite else { bail!("give the invite code: `tincan join <code>`") };
            let t: TeamInfo = link.call(Request::Join { ticket: invite, person: default_person(person) }).await?;
            wait_exit(daemon_pid()).await;
            println!("Joined team {} as {} on {}.", t.name, t.person, t.machine);
            if !no_setup {
                println!();
                setup::run(false, false, false).await?;
            }
            // Give the new daemon a moment to reach teammates, then show them.
            let fresh = Link::new(None);
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if let Ok(t) = fresh.call::<TeamInfo>(Request::Team).await
                    && t.machines.iter().any(|m| !m.is_me && m.online)
                {
                    break;
                }
            }
            if let Ok(t) = fresh.call::<TeamInfo>(Request::Team).await {
                print!("\n{}", render::team(&t));
            }
        }
        Cmd::Invite { me, hours, .. } => {
            let i: InviteInfo = link.call(Request::Invite { me, hours: hours.clamp(1, 24 * 30) }).await?;
            let who = if me { "another machine of yours" } else { "a teammate" };
            println!("Invite code for {who}: {}\n", i.code);
            println!("They run this, which installs tincan and joins the team:\n");
            println!("  curl -fsSL {} | sh -s -- {}\n", install_url(), i.code);
            println!("(or `tincan join {}` if they have tincan). Works once, within {}h, while this machine is online.", i.code, i.hours);
            println!("If this machine will be offline, use the long form instead: tincan join {}", i.ticket);
        }
        Cmd::Team => {
            let t: TeamInfo = link.call(Request::Team).await?;
            print!("{}", render::team(&t));
        }
        Cmd::Remove { who } => println!("{}", link.call::<String>(Request::Remove { who }).await?),
        Cmd::Leave { force } => {
            println!("{}", link.call::<String>(Request::Leave { force }).await?);
            wait_exit(daemon_pid()).await;
        }
        Cmd::Relay { url } => {
            let url = (url != "default").then_some(url);
            println!("{}", link.call::<String>(Request::SetRelay { url }).await?);
            wait_exit(daemon_pid()).await;
        }
        Cmd::Held => {
            let msgs: Vec<Message> = link.call(Request::Held).await?;
            if msgs.is_empty() {
                println!("Nothing held.");
            } else {
                print!("{}", render::for_human(&msgs));
                println!("`tincan approve <id>` delivers, `tincan reject <id>` drops (or --all).");
            }
        }
        Cmd::Approve { ids, all } | Cmd::Reject { ids, all }
            if ids.is_empty() && !all =>
        {
            bail!("give message ids, or --all")
        }
        Cmd::Approve { ids, .. } => {
            let n: usize = link.call(Request::Settle { ids, release: true }).await?;
            println!("Delivered {n}.");
        }
        Cmd::Reject { ids, .. } => {
            let n: usize = link.call(Request::Settle { ids, release: false }).await?;
            println!("Dropped {n}.");
        }
        Cmd::Setup { dry_run, remove, no_service } => setup::run(dry_run, remove, no_service).await?,
        Cmd::Doctor => setup::doctor().await?,
        Cmd::Upgrade => {
            let dir = std::env::current_exe()?.parent().map(|d| d.display().to_string()).unwrap_or_default();
            let status = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("curl -fsSL {} | sh", install_url()))
                .env("TINCAN_BIN_DIR", dir)
                .status()?;
            if !status.success() {
                bail!("upgrade failed");
            }
            stop_daemon().await;
            println!("The daemon restarted on the new version. Restart agent sessions to update their tincan tools.");
        }
        Cmd::Stop => {
            if !stop_daemon().await {
                println!("The daemon is not running.");
            }
        }
        Cmd::Daemon { .. } | Cmd::Mcp | Cmd::Hook { .. } | Cmd::Tray => unreachable!("handled in main"),
    }
    Ok(())
}

/// The running daemon's pid, from its lock file.
fn daemon_pid() -> Option<u32> {
    std::fs::read_to_string(paths::lock()).ok()?.trim().parse().ok().filter(|p| util::pid_alive(*p))
}

/// Waits (briefly) for a daemon that is restarting to exit, so the next request reaches the new one. Call it
/// right after the reply: the daemon exits a moment later.
async fn wait_exit(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    for _ in 0..50 {
        if !util::pid_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Asks a running daemon to exit and waits until it has. False if none was running.
pub async fn stop_daemon() -> bool {
    if tokio::net::UnixStream::connect(paths::socket()).await.is_err() {
        return false;
    }
    let pid = daemon_pid();
    let _ = Link::existing().call::<serde_json::Value>(Request::Shutdown).await;
    wait_exit(pid).await;
    true
}
