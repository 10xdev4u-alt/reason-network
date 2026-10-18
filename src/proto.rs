//! The local API between the daemon and its clients (`reason network mcp`, the CLI, hooks): one JSON object per line
//! over `~/.Reason Network/Reason Network.sock`. Requests carry an `id` and get a [`Reply`] with the same id; a connection that
//! registered sessions also receives [`Event`]s for them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How quickly a session sees a message (ordered fastest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Injected into the conversation right away; wakes the session if it is idle.
    Push,
    /// Injected at the next tool call or when the turn ends; an idle session waits for its human.
    Turn,
    /// Seen only when the agent checks its inbox.
    Pull,
    /// Not on Reason Network: cannot receive.
    None,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Push => "push",
            Tier::Turn => "turn",
            Tier::Pull => "pull",
            Tier::None => "none",
        }
    }

    pub fn parse(s: &str) -> Tier {
        match s {
            "push" => Tier::Push,
            "turn" => Tier::Turn,
            "pull" => Tier::Pull,
            _ => Tier::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReg {
    /// Stable id for this agent session: `<agent>:<agent's session id>`, or `proc:<pid>` when the agent gives none.
    pub key: String,
    pub agent: String,
    pub cwd: String,
    #[serde(default)]
    pub agent_session_id: Option<String>,
    /// The agent's process, used to match Claude Code's own session registry.
    #[serde(default)]
    pub pid: Option<u32>,
    pub tier: Tier,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCall {
    pub agent: String,
    /// `start`, `prompt`, `tool`, `stop`, `end`, or `doing` (only reports what the session is doing).
    pub event: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    /// False when this event can't show the agent anything (so mail stays for a later event).
    #[serde(default = "yes")]
    pub mail: bool,
    /// The agent's transcript, which says what the session is doing.
    #[serde(default)]
    pub transcript: Option<String>,
    /// What the session is doing, from agents that say so themselves (the opencode plugin).
    #[serde(default)]
    pub doing: Option<String>,
    /// The session's name, for sessions that pick one (`reason network listen --as`).
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Register {
        session: SessionReg,
    },
    SetStatus {
        key: String,
        #[serde(default)]
        doing: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    Who {
        #[serde(default)]
        me: Option<String>,
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        all: bool,
    },
    /// From an agent session: its key (or, for agents that reply through the CLI, its address or name).
    Send {
        from: String,
        to: String,
        body: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default = "yes")]
        wake: bool,
        /// Also pop it up on the person's screen. With it, `to` may be your own user.
        #[serde(default)]
        notify: bool,
    },
    /// A session's unread messages. With `peek` nothing is marked read: the client acknowledges with `Delivered`
    /// after showing the messages, so nothing is lost if it goes away first.
    Inbox {
        key: String,
        #[serde(default)]
        wait_secs: u64,
        #[serde(default)]
        peek: bool,
    },
    /// A session's unread messages, counted.
    Unread {
        key: String,
    },
    /// The client gave up on this session's `Inbox` wait (the agent cancelled it): end it, and push what it was
    /// waiting for the usual way.
    StopWaiting {
        key: String,
    },
    /// The client injected these messages into its agent's conversation.
    Delivered {
        key: String,
        ids: Vec<String>,
    },
    /// An agent hook fired (`reason network hook`): keeps hook-only sessions alive and returns mail to inject.
    Hook(HookCall),
    /// For agents whose MCP calls carry no session id (Cursor, opencode): the hook-known session of this agent
    /// that is most likely making the call.
    Attach {
        agent: String,
        cwd: String,
    },
    CreateTeam {
        name: String,
        person: String,
        #[serde(default)]
        relay: Option<String>,
    },
    Join {
        ticket: String,
        person: String,
    },
    Invite {
        #[serde(default)]
        me: bool,
        #[serde(default = "day")]
        hours: u64,
    },
    Team,
    /// The team link, which anyone can use to join; `new` replaces it.
    Link {
        #[serde(default)]
        new: bool,
    },
    /// Remove a person (all their machines) or one machine from the team.
    Remove {
        who: String,
    },
    Leave {
        /// Leave even if no teammate is online to hear it (they'll need to `reason network remove` this machine).
        #[serde(default)]
        force: bool,
    },
    SetRelay {
        #[serde(default)]
        url: Option<String>,
    },
    Held,
    /// Approve (`release`) or drop held messages; no ids means all of them.
    Settle {
        #[serde(default)]
        ids: Vec<String>,
        release: bool,
    },
    /// The board: one unit of work the daemon owns (see `board.rs`). All of these are IPC-only — no on-the-wire
    /// type is touched, so a task never appears on a teammate's machine that was not told about it.
    TaskAdd {
        title: String,
        /// What "done" means for this task. Empty is refused: a task with no boundary is a wish.
        boundary: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        branch: Option<String>,
    },
    TaskList {
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        all: bool,
    },
    /// One task with its history.
    TaskShow {
        task_id: String,
    },
    /// First writer wins; the loser is told who holds it rather than silently overwriting.
    TaskClaim {
        task_id: String,
        #[serde(default)]
        by: Option<String>,
    },
    TaskNote {
        task_id: String,
        text: String,
        #[serde(default)]
        by: Option<String>,
    },
    TaskBlock {
        task_id: String,
        reason: String,
        #[serde(default)]
        by: Option<String>,
    },
    /// Refused with no evidence: closing is a claim about the world, not a status change.
    TaskClose {
        task_id: String,
        evidence: String,
        #[serde(default)]
        by: Option<String>,
    },
    TaskRelease {
        task_id: String,
        #[serde(default)]
        by: Option<String>,
    },
    Shutdown,
}

/// One unit of shared work. It lives in the daemon, not in a page: a queue that can be lost by closing a tab is a
/// whiteboard, and a claim has to be readable by every client, including ones with no browser at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub boundary: String,
    pub state: TaskState,
    /// The address that claimed it — the worker's own write, never liveness derived from a connection.
    pub owner: Option<String>,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Open,
    Claimed,
    Blocked,
    Done,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Open => "open",
            TaskState::Claimed => "claimed",
            TaskState::Blocked => "blocked",
            TaskState::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(TaskState::Open),
            "claimed" => Some(TaskState::Claimed),
            "blocked" => Some(TaskState::Blocked),
            "done" => Some(TaskState::Done),
            _ => None,
        }
    }
}

/// What happened to a task, who did it and when. Append-only: a closed task is not a deleted row with no trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    pub task_id: String,
    pub at_ms: i64,
    pub kind: TaskKind,
    pub by: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Created,
    Claimed,
    Note,
    Blocked,
    Released,
    Closed,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Created => "created",
            TaskKind::Claimed => "claimed",
            TaskKind::Note => "note",
            TaskKind::Blocked => "blocked",
            TaskKind::Released => "released",
            TaskKind::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "created" => Some(TaskKind::Created),
            "claimed" => Some(TaskKind::Claimed),
            "note" => Some(TaskKind::Note),
            "blocked" => Some(TaskKind::Blocked),
            "released" => Some(TaskKind::Released),
            "closed" => Some(TaskKind::Closed),
            _ => None,
        }
    }
}

fn day() -> u64 {
    24
}

fn yes() -> bool {
    true
}

/// `*`, `person/*` or `repo:<name>`, on any machine (`…@machine`): for a person at a terminal, never an agent.
pub fn is_group(to: &str) -> bool {
    let to = to.rsplit_once('@').map_or(to, |(s, _)| s);
    to == "*" || to.starts_with("repo:") || to.ends_with("/*")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame<T> {
    pub id: u64,
    #[serde(flatten)]
    pub body: T,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    pub fn ok(id: u64, result: Value) -> Self {
        Reply { id, ok: true, result: Some(result), error: None }
    }

    pub fn err(id: u64, e: anyhow::Error) -> Self {
        Reply { id, ok: false, result: None, error: Some(format!("{e:#}")) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Inject `text` into the conversation of session `key`, then acknowledge with [`Request::Delivered`].
    Deliver { key: String, ids: Vec<String>, text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    /// The agent session it came from (`person/session`).
    pub from: String,
    pub from_agent: Option<String>,
    pub from_repo: Option<String>,
    /// The resolved recipient address.
    pub to: String,
    /// The address as the sender typed it, when it was a group (`*`, `repo:api`, `alex/*`).
    pub via: Option<String>,
    pub reply_to: Option<String>,
    pub body: String,
    pub wake: bool,
    /// Pop it up on the person's screen too (and, sent to your own user rather than a session, only that).
    #[serde(default)]
    pub notify: bool,
    pub sent_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registered {
    pub name: String,
    pub address: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Directory {
    pub team: Option<String>,
    pub person: String,
    pub machine: String,
    pub machines: Vec<MachineInfo>,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineInfo {
    pub person: String,
    pub machine: String,
    pub endpoint: String,
    pub is_me: bool,
    pub online: bool,
    /// `direct` or `relay` while connected.
    pub link: Option<String>,
    pub rtt_ms: Option<u64>,
    pub last_seen: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInfo {
    pub name: String,
    pub relay: Option<String>,
    /// Whether this machine is on its relay right now (`None` while the network isn't up).
    #[serde(default)]
    pub relay_connected: Option<bool>,
    pub person: String,
    pub machine: String,
    pub machines: Vec<MachineInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    pub team: String,
    /// A page with the one command to run (the ticket rides in the `#fragment`, which never reaches the server).
    pub url: String,
    pub ticket: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteInfo {
    /// Four words, like `plum-tiger-lake-orbit`. For a team with its own relay, the long form (a code would need n0's
    /// address lookup to find this machine).
    pub code: String,
    /// The same invite as a long `tincan1…` string, for when the inviting machine will be offline.
    pub ticket: String,
    pub hours: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookReply {
    pub address: Option<String>,
    /// Messages to put in the agent's context, framed.
    pub text: Option<String>,
    /// Their ids, to acknowledge with `Delivered` once shown.
    #[serde(default)]
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub address: String,
    pub name: String,
    pub person: String,
    pub machine: String,
    pub agent: String,
    pub cwd: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    /// `busy`, `idle`, `live` (running, activity unknown) or `offline`.
    pub status: String,
    /// When `status` last changed (for `offline`: last seen), ms since epoch.
    pub since: Option<i64>,
    pub doing: Option<String>,
    pub tier: Tier,
    pub unread: u32,
    pub is_me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResult {
    pub ids: Vec<String>,
    pub recipients: Vec<Recipient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipient {
    pub address: String,
    /// `pushed`, `queued` or `inbox`.
    pub state: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inbox {
    pub messages: Vec<Message>,
    /// The messages framed for an agent's context.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unread {
    pub count: u32,
    pub from: Vec<String>,
}
