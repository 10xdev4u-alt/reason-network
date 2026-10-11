//! The local API between the daemon and its clients (`comms mcp`, the CLI, hooks): one JSON object per line
//! over `~/.comms/comms.sock`. Requests carry an `id` and get a [`Reply`] with the same id; a connection that
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
    /// Not on comms: cannot receive.
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
    /// `from` is a session key, or absent for the human at this machine.
    Send {
        #[serde(default)]
        from: Option<String>,
        to: String,
        body: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default = "yes")]
        wake: bool,
    },
    /// `key` is a session key, or absent for the human's inbox. With `peek` nothing is marked read: the client
    /// acknowledges with `Delivered` after showing the messages, so nothing is lost if it goes away first.
    Inbox {
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        wait_secs: u64,
        #[serde(default)]
        peek: bool,
    },
    Unread {
        key: String,
    },
    /// The client injected these messages into its agent's conversation.
    Delivered {
        key: String,
        ids: Vec<String>,
    },
    /// An agent hook fired (`comms hook`): keeps hook-only sessions alive and returns mail to inject.
    Hook {
        agent: String,
        /// `start`, `prompt`, `tool`, `stop` or `end`.
        event: String,
        session_id: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
        /// False when this event can't show the agent anything (so mail stays for a later event).
        #[serde(default = "yes")]
        mail: bool,
    },
    /// For agents whose MCP calls carry no session id (Cursor, opencode): the hook-known session of this agent
    /// that is most likely making the call.
    Attach {
        agent: String,
        cwd: String,
    },
    CreateTeam {
        name: String,
        person: String,
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
    /// Remove a person (all their machines) or one machine from the team.
    Remove {
        who: String,
    },
    Leave {
        /// Leave even if no teammate is online to hear it (they'll need to `comms remove` this machine).
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
    Shutdown,
}

fn day() -> u64 {
    24
}

fn yes() -> bool {
    true
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
    /// `person/session` for an agent, `person` for a human.
    pub from: String,
    pub from_agent: Option<String>,
    pub from_repo: Option<String>,
    /// The resolved recipient address.
    pub to: String,
    /// The address as the sender typed it, when it was a group (`*`, `repo:api`, `tal/*`).
    pub via: Option<String>,
    pub reply_to: Option<String>,
    pub body: String,
    pub wake: bool,
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
    pub person: String,
    pub machine: String,
    pub machines: Vec<MachineInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteInfo {
    /// Four words, like `plum-tiger-lake-orbit`.
    pub code: String,
    /// The same invite as a long `comms1…` string, for when the inviting machine will be offline.
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
