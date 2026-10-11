//! Sending, receiving and delivering messages.
//!
//! Delivery is at least once: a message stays unread until whoever showed it to the agent (or human) confirms
//! with `Delivered`. Messages handed to a push channel are "in flight" meanwhile, so the inbox and hooks don't
//! show them a second time.

use super::{Daemon, Inner, directory::Target, log};
use crate::{agent, config::Policy, proto::*, render, util};
use anyhow::{Result, anyhow, bail};
use iroh::EndpointId;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use ulid::Ulid;

const MAX_BODY: usize = 64 * 1024;
/// Messages per minute accepted from one teammate's machine; more are deferred, not dropped.
const RATE_LIMIT: usize = 60;
/// Agent-to-agent messages allowed between one pair in `LOOP_WINDOW`; past that it is probably a loop.
const LOOP_LIMIT: usize = 12;
const LOOP_WINDOW: Duration = Duration::from_secs(600);
/// A push not confirmed within this long is shown through the inbox and hooks again.
const IN_FLIGHT_TTL: Duration = Duration::from_secs(90);

/// How a session on this machine gets a message into its conversation.
enum Channel {
    /// Its `comms mcp` posts into Claude Code's session socket.
    ClaudeSocket(u64),
    /// `codex queue --thread <id>`.
    CodexQueue(String),
    /// Hooks inject it at the next tool call or turn end.
    Hook,
    /// Only `check_inbox`.
    Pull,
}

impl Inner {
    /// Unread mail for an address that isn't already on its way through a push.
    pub(super) fn pending(&mut self, address: &str) -> Result<Vec<Message>> {
        self.in_flight.retain(|_, at| at.elapsed() < IN_FLIGHT_TTL);
        let msgs = self.store.unread(address)?;
        Ok(msgs.into_iter().filter(|m| !self.in_flight.contains_key(&m.id)).collect())
    }

    /// Records an agent-to-agent message; false if this pair has been talking suspiciously much.
    fn loop_ok(&mut self, from: &str, to: &str) -> bool {
        if !from.contains('/') || !to.contains('/') {
            return true;
        }
        let q = self.pairs.entry((from.to_string(), to.to_string())).or_default();
        while q.front().is_some_and(|t| t.elapsed() > LOOP_WINDOW) {
            q.pop_front();
        }
        if q.len() >= LOOP_LIMIT {
            return false;
        }
        q.push_back(Instant::now());
        true
    }
}

const LOOP_NOTE: &str = "refused: these two agents have exchanged a lot of messages in the last few minutes, which \
                         looks like a loop. Check with your user before continuing.";

impl Daemon {
    pub(super) async fn send(
        self: &Arc<Self>,
        from: Option<String>,
        to: &str,
        body: &str,
        reply_to: Option<String>,
        wake: bool,
    ) -> Result<SendResult> {
        let (body, to) = (body.trim(), to.trim());
        if body.is_empty() {
            bail!("empty message");
        }
        if body.len() > MAX_BODY {
            bail!("message is {} KB; the limit is {} KB (send a path or link instead)", body.len() / 1024, MAX_BODY / 1024);
        }
        let now = util::now_ms();
        let net = self.net();
        let remotes = net.as_ref().map(|n| n.remotes()).unwrap_or_default();

        let mut res = SendResult { ids: Vec::new(), recipients: Vec::new() };
        let mut remote_jobs = Vec::new();
        {
            let mut g = self.inner.lock().unwrap();
            self.prune(&mut g);
            let sender = match from.as_deref() {
                Some(k) => Some(g.live.get(k).ok_or_else(unregistered)?),
                None => None,
            };
            let (from_addr, from_agent, from_repo) = match sender {
                Some(l) => (self.addr(&l.name), Some(l.reg.agent.clone()), l.git.repo.clone()),
                None => (self.person.clone(), None, None),
            };
            let exclude = sender.map(|l| l.reg.key.clone());
            let targets = self.resolve(&g, &remotes, to, exclude.as_deref())?;
            for t in targets {
                let msg = Message {
                    id: Ulid::new().to_string(),
                    from: from_addr.clone(),
                    from_agent: from_agent.clone(),
                    from_repo: from_repo.clone(),
                    to: t.address(&self.person),
                    via: is_group(to).then(|| to.to_string()),
                    reply_to: reply_to.clone(),
                    body: body.to_string(),
                    wake,
                    sent_at: now,
                };
                res.ids.push(msg.id.clone());
                let (state, note) = match t {
                    _ if !matches!(t, Target::Remote { .. }) && !g.loop_ok(&msg.from, &msg.to) => {
                        ("refused".into(), LOOP_NOTE.into())
                    }
                    Target::Human => {
                        g.store.insert(&msg, false)?;
                        ("inbox".into(), format!("in {}'s inbox (`comms inbox`)", self.person))
                    }
                    Target::Session { key: Some(key), .. } => {
                        g.store.insert(&msg, false)?;
                        self.deliver(&mut g, &key, vec![msg.clone()], wake)
                    }
                    Target::Session { key: None, .. } => {
                        g.store.insert(&msg, false)?;
                        ("queued".into(), "queued: the session is offline; delivered when it is back".into())
                    }
                    Target::Remote { machine, machine_name, online, .. } => {
                        g.store.outbox_add(&machine.to_string(), &msg)?;
                        remote_jobs.push((res.recipients.len(), machine, machine_name.clone(), online, msg.clone()));
                        ("queued".into(), format!("queued: not connected to {machine_name} right now; it goes out as soon as it is"))
                    }
                };
                res.recipients.push(Recipient { address: msg.to.clone(), state, note });
            }
        }
        self.arrived.notify_waiters();

        if let Some(net) = net {
            let sends = remote_jobs.into_iter().filter(|j| j.3).map(|(i, machine, name, _, msg)| {
                let net = net.clone();
                async move {
                    let verdict = net.send_msg(&machine, &msg).await;
                    (i, name, verdict)
                }
            });
            for (i, name, verdict) in futures_join(sends).await {
                let r = &mut res.recipients[i];
                match verdict {
                    Some((state, note)) => {
                        r.state = state;
                        r.note = note;
                    }
                    None => r.note = format!("queued: no answer from {name} yet; it will be retried"),
                }
            }
        }
        Ok(res)
    }

    /// Hands messages to a session here by the fastest way it has. Returns the state and a note for the sender.
    pub(super) fn deliver(self: &Arc<Self>, g: &mut Inner, key: &str, msgs: Vec<Message>, wake: bool) -> (String, String) {
        let Some(l) = g.live.get(key) else {
            return ("queued".into(), "queued: the session is offline; delivered when it is back".into());
        };
        if !wake {
            return ("queued".into(), "queued as FYI: it reads it at its next natural pause".into());
        }
        if g.polling.get(key).is_some_and(|n| *n > 0) {
            return ("delivered".into(), "delivered: the session was waiting for messages".into());
        }
        let codex = || agent::find_bin("codex").is_some();
        let channel = match (l.reg.tier, l.conn, &l.reg.agent_session_id) {
            (Tier::Push, Some(conn), _) if l.reg.agent == agent::CLAUDE_CODE => Channel::ClaudeSocket(conn),
            (Tier::Push, _, Some(id)) if l.reg.agent == "codex" && !l.push_failed && codex() => Channel::CodexQueue(id.clone()),
            _ if l.last_hook.is_some() => Channel::Hook,
            _ => Channel::Pull,
        };
        let ids: Vec<String> = msgs.iter().map(|m| m.id.clone()).collect();
        let text = render::for_agent(&msgs, &self.person, Some(&l.reg.agent));
        let hook_note = if l.reg.agent == "opencode" {
            "queued: delivered within seconds once the session is idle"
        } else {
            "queued: it sees it at its next tool call or when its turn ends"
        };
        match channel {
            Channel::ClaudeSocket(conn) => {
                let Some(tx) = g.conns.get(&conn) else {
                    return ("queued".into(), "queued: this agent only sees it when it checks its inbox".into());
                };
                let ev = Event::Deliver { key: key.to_string(), ids: ids.clone(), text };
                let _ = tx.send(serde_json::to_string(&ev).expect("event serializes"));
                for id in ids {
                    g.in_flight.insert(id, Instant::now());
                }
                ("pushed".into(), "delivered into the session: it reads it between tool calls, or wakes up if idle".into())
            }
            Channel::CodexQueue(thread) => {
                for id in &ids {
                    g.in_flight.insert(id.clone(), Instant::now());
                }
                self.codex_queue(key.to_string(), thread, ids, text);
                ("pushed".into(), "queued into the Codex session: it runs after the current turn, or now if idle".into())
            }
            Channel::Hook => ("queued".into(), hook_note.into()),
            Channel::Pull => ("queued".into(), "queued: this agent only sees it when it checks its inbox".into()),
        }
    }

    fn codex_queue(self: &Arc<Self>, key: String, thread: String, ids: Vec<String>, text: String) {
        let this = self.clone();
        tokio::spawn(async move {
            let run = async {
                let bin = agent::find_bin("codex").ok_or_else(|| "codex not found".to_string())?;
                let out = tokio::process::Command::new(bin)
                    .args(["queue", "--thread", &thread, "--message", &text])
                    .stdin(std::process::Stdio::null())
                    .output();
                match tokio::time::timeout(Duration::from_secs(30), out).await {
                    Ok(Ok(o)) if o.status.success() => Ok(()),
                    Ok(Ok(o)) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(_) => Err("timed out".into()),
                }
            };
            let res = run.await;
            let mut g = this.inner.lock().unwrap();
            for id in &ids {
                g.in_flight.remove(id);
            }
            match res {
                Ok(()) => {
                    let _ = g.store.mark_read(&ids, util::now_ms());
                }
                Err(why) => {
                    log(&format!("codex queue for {key} failed ({why}); falling back to hooks"));
                    // Hooks (or check_inbox) show it instead; tell senders the truth from now on.
                    if let Some(l) = g.live.get_mut(&key) {
                        l.push_failed = true;
                        l.reg.tier = Tier::Turn;
                    }
                }
            }
        });
    }

    /// A message from a teammate's machine. Returns the state and a note for the sender. A `retry` state
    /// means "not now": the sender keeps it in its outbox.
    pub fn receive(self: &Arc<Self>, peer: EndpointId, msg: Message) -> Result<(String, String)> {
        let sender = self
            .team_state()
            .and_then(|t| t.member(&peer).map(|m| m.person.clone()))
            .ok_or_else(|| anyhow!("not a team member"))?;
        let msg = clean(msg)?;
        // A machine speaks only for its own person.
        if msg.from != sender && !msg.from.starts_with(&format!("{sender}/")) {
            bail!("{} cannot send as {}", sender, msg.from);
        }
        let policy = self.cfg.policy_for(&sender);
        if policy == Policy::Refuse {
            return Ok(("refused".into(), format!("{} doesn't accept messages from {sender}", self.person)));
        }
        if !self.allow(&peer) {
            return Ok(("retry".into(), format!("queued: {} is receiving a lot right now; retrying shortly", self.machine)));
        }
        let (person, name) = match msg.to.split_once('/') {
            Some((p, n)) => (p, Some(n)),
            None => (msg.to.as_str(), None),
        };
        if person != self.person {
            bail!("{} is not on this machine", msg.to);
        }

        let res = {
            let mut g = self.inner.lock().unwrap();
            self.prune(&mut g);
            match name {
                None => {
                    if !g.store.insert(&msg, false)? {
                        return Ok(("inbox".into(), "already delivered".into()));
                    }
                    ("inbox".into(), format!("in {}'s inbox", self.person))
                }
                Some(name) => {
                    let key = g.live.iter().find(|(_, l)| l.name == name).map(|(k, _)| k.clone());
                    if key.is_none() && g.store.session_by_name(name)?.is_none() {
                        return Ok(("refused".into(), format!("no session called {} on {}", msg.to, self.machine)));
                    }
                    if g.store.has(&msg.id)? {
                        return Ok(("queued".into(), "already delivered".into()));
                    }
                    if !g.loop_ok(&msg.from, &msg.to) {
                        return Ok(("refused".into(), LOOP_NOTE.into()));
                    }
                    let held = policy == Policy::Hold;
                    g.store.insert(&msg, held)?;
                    match key {
                        _ if held => ("held".into(), format!("held until {} approves it", self.person)),
                        Some(key) => self.deliver(&mut g, &key, vec![msg.clone()], msg.wake),
                        None => ("queued".into(), "queued: the session is offline; delivered when it is back".into()),
                    }
                }
            }
        };
        self.arrived.notify_waiters();
        Ok(res)
    }

    fn allow(&self, peer: &EndpointId) -> bool {
        let mut rate = self.rate.lock().unwrap();
        let q = rate.entry(peer.to_string()).or_default();
        while q.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
            q.pop_front();
        }
        if q.len() >= RATE_LIMIT {
            return false;
        }
        q.push_back(Instant::now());
        true
    }

    /// Unread messages for a session (or, with no key, the human). Waits up to `wait_secs` for one. Marks
    /// them read unless `peek`, in which case the client confirms with `Delivered`.
    pub(super) async fn inbox(&self, key: Option<String>, wait_secs: u64, peek: bool) -> Result<Inbox> {
        let (address, agent) = {
            let g = self.inner.lock().unwrap();
            match key.as_deref() {
                Some(k) => {
                    let l = g.live.get(k).ok_or_else(unregistered)?;
                    (self.addr(&l.name), Some(l.reg.agent.clone()))
                }
                None => (self.person.clone(), None),
            }
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
        let _polling = key.as_deref().filter(|_| wait_secs > 0).map(|k| Polling::start(self, k));
        loop {
            // Register interest before checking, so a message landing in between still wakes us.
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            let messages = {
                let mut g = self.inner.lock().unwrap();
                let messages = g.pending(&address)?;
                if !messages.is_empty() && !peek {
                    let ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
                    g.store.mark_read(&ids, util::now_ms())?;
                }
                messages
            };
            if !messages.is_empty() || tokio::time::Instant::now() >= deadline {
                let text = render::for_agent(&messages, &self.person, agent.as_deref());
                return Ok(Inbox { messages, text });
            }
            let _ = tokio::time::timeout_at(deadline, arrived).await;
        }
    }

    /// Approves or drops held messages. Approved ones go to their session now.
    pub(super) fn settle(self: &Arc<Self>, ids: &[String], release: bool) -> Result<usize> {
        let mut g = self.inner.lock().unwrap();
        let msgs = g.store.settle_held(ids, release)?;
        if release {
            for m in &msgs {
                let name = m.to.split_once('/').map_or("", |(_, n)| n);
                if let Some(key) = g.live.iter().find(|(_, l)| l.name == name).map(|(k, _)| k.clone()) {
                    self.deliver(&mut g, &key, vec![m.clone()], m.wake);
                }
            }
        }
        drop(g);
        self.arrived.notify_waiters();
        Ok(msgs.len())
    }
}

/// Marks a session as waiting in `check_inbox` for as long as it lives.
struct Polling<'a> {
    daemon: &'a Daemon,
    key: String,
}

impl<'a> Polling<'a> {
    fn start(daemon: &'a Daemon, key: &str) -> Self {
        *daemon.inner.lock().unwrap().polling.entry(key.to_string()).or_default() += 1;
        Polling { daemon, key: key.to_string() }
    }
}

impl Drop for Polling<'_> {
    fn drop(&mut self) {
        let mut g = self.daemon.inner.lock().unwrap();
        if let Some(n) = g.polling.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                g.polling.remove(&self.key);
            }
        }
    }
}

fn unregistered() -> anyhow::Error {
    anyhow!("this session isn't registered with comms; restart the agent session, or use the comms tools")
}

fn is_group(to: &str) -> bool {
    to == "*" || to.starts_with("repo:") || to.ends_with("/*")
}

/// Keeps a field to plain characters, so nothing a peer sends can reshape what agents are shown.
fn tidy(s: &str, extra: &str, max: usize) -> Option<String> {
    let t: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || extra.contains(*c)).take(max).collect();
    (!t.is_empty()).then_some(t)
}

/// Validates a message from a peer and strips its metadata down to safe characters.
fn clean(m: Message) -> Result<Message> {
    let ok = |s: &str, extra: &str| !s.is_empty() && s.len() <= 120 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if !ok(&m.id, "") || !ok(&m.from, "-/") || !ok(&m.to, "-/") {
        bail!("malformed message");
    }
    Ok(Message {
        from_agent: m.from_agent.as_deref().and_then(|s| tidy(s, "-", 30)),
        from_repo: m.from_repo.as_deref().and_then(|s| tidy(s, "-_./", 100)),
        via: m.via.as_deref().and_then(|s| tidy(s, "-_./:*@", 100)),
        reply_to: m.reply_to.as_deref().and_then(|s| tidy(s, "", 40)),
        ..m
    })
}

/// Runs futures concurrently and collects their outputs in order.
async fn futures_join<F>(futs: impl Iterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}
