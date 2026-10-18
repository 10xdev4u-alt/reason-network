//! The board: the queue the daemon owns, so coordination survives a closed browser tab. A claim is the worker's
//! own write — the row names them — and it is never liveness derived from a connection a long turn does not hold.
//! Presence is still a snapshot (`sessions.rs`): a claimed task whose session reads `absent` may be its owner
//! working right now, which is why the claim, not the status, is what the board is built from.

use crate::daemon::Daemon;
use crate::proto::{Task, TaskState};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::sync::Arc;
use ulid::Ulid;

impl Daemon {
    /// Every board write wakes the open pages. What the daemon owns should never need a refresh to be seen.
    fn wrote(&self) {
        self.arrived().notify_waiters();
    }

    /// Who an action is from. A client may name itself (an agent claiming as its own session); otherwise it is the
    /// person's terminal, which is what a shell command is.
    fn actor(&self, by: Option<String>) -> String {
        match by.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) {
            Some(b) => b,
            None => self.addr("terminal"),
        }
    }

    pub fn task_add(
        &self,
        title: &str,
        boundary: &str,
        repo: Option<String>,
        branch: Option<String>,
        by: Option<String>,
    ) -> Result<Value> {
        let (title, boundary) = (title.trim(), boundary.trim());
        if title.is_empty() {
            bail!("a task needs a title");
        }
        if boundary.is_empty() {
            bail!("a task needs a boundary — what does done mean? without one nobody can close it");
        }
        let now = crate::util::now_ms();
        let t = Task {
            id: Ulid::new().to_string(),
            title: title.to_string(),
            boundary: boundary.to_string(),
            state: TaskState::Open,
            owner: None,
            repo,
            branch,
            created_at: now,
            updated_at: now,
            blocked_reason: None,
        };
        let by = self.actor(by);
        self.inner.lock().unwrap().store.task_add(&t, &by)?;
        self.wrote();
        Ok(json!({ "id": t.id }))
    }

    /// The board, with the join done here rather than in the page: the page never fabricates liveness.
    pub async fn tasks_json(self: &Arc<Self>, all: bool, filter: Option<String>) -> Result<Value> {
        let tasks = self.inner.lock().unwrap().store.tasks(all, filter.as_deref())?;
        let sessions = self.who(None, None, true).await?;
        let now = crate::util::now_ms();
        let out: Vec<Value> = tasks
            .into_iter()
            .map(|t| {
                let owner = t.owner.as_deref().and_then(|o| sessions.sessions.iter().find(|s| s.address == o));
                json!({
                    "id": t.id,
                    "title": t.title,
                    "boundary": t.boundary,
                    "state": t.state.as_str(),
                    "owner": t.owner,
                    // A holder with no live session is `absent`, not `offline`: the session may be mid-turn.
                    "owner_status": match (owner, &t.owner) {
                        (Some(s), _) => s.status.clone(),
                        (None, Some(_)) => "absent".into(),
                        (None, None) => String::new(),
                    },
                    "owner_doing": owner.and_then(|s| s.doing.clone()),
                    "repo": t.repo,
                    "branch": t.branch,
                    "created_at": t.created_at,
                    "updated_at": t.updated_at,
                    "blocked_reason": t.blocked_reason,
                    "age_ms": now - t.created_at,
                })
            })
            .collect();
        Ok(json!({ "tasks": out }))
    }

    pub fn task_show(&self, id: &str) -> Result<Value> {
        let g = self.inner.lock().unwrap();
        let Some(t) = g.store.task(id)? else {
            bail!("no task {id}");
        };
        let events = g.store.task_events(id)?;
        Ok(json!({
            "task": {
                "id": t.id, "title": t.title, "boundary": t.boundary, "state": t.state.as_str(),
                "owner": t.owner, "repo": t.repo, "branch": t.branch,
                "created_at": t.created_at, "updated_at": t.updated_at, "blocked_reason": t.blocked_reason,
            },
            "events": events,
        }))
    }

    pub fn task_claim(&self, id: &str, by: Option<String>) -> Result<Value> {
        let by = self.actor(by);
        let now = crate::util::now_ms();
        let won = self.inner.lock().unwrap().store.task_claim(id, &by, now)?;
        self.wrote();
        match won {
            // Losing is an answer, not an error: the caller learns who holds it and can pick another.
            Some(owner) => Ok(json!({ "ok": false, "owner": owner })),
            None => Ok(json!({ "ok": true, "owner": by })),
        }
    }

    pub fn task_note(&self, id: &str, text: &str, by: Option<String>) -> Result<Value> {
        let (text, by) = (text.trim(), self.actor(by));
        if text.is_empty() {
            bail!("a note needs text");
        }
        let now = crate::util::now_ms();
        self.inner.lock().unwrap().store.task_note(id, &by, text, now)?;
        self.wrote();
        self.event_count(id)
    }

    pub fn task_block(&self, id: &str, reason: &str, by: Option<String>) -> Result<Value> {
        let (reason, by) = (reason.trim(), self.actor(by));
        if reason.is_empty() {
            bail!("say why it is blocked: a task with no reason cannot be picked up again");
        }
        let now = crate::util::now_ms();
        self.inner.lock().unwrap().store.task_block(id, &by, reason, now)?;
        self.wrote();
        self.event_count(id)
    }

    /// Closing is a claim about the world: with no evidence it is refused, in the store and here.
    pub fn task_close(&self, id: &str, evidence: &str, by: Option<String>) -> Result<Value> {
        let (evidence, by) = (evidence.trim(), self.actor(by));
        if evidence.is_empty() {
            bail!("closing needs evidence: what proves the boundary was met?");
        }
        let now = crate::util::now_ms();
        self.inner.lock().unwrap().store.task_close(id, &by, evidence, now)?;
        self.wrote();
        self.event_count(id)
    }

    pub fn task_release(&self, id: &str, by: Option<String>) -> Result<Value> {
        let (by, now) = (self.actor(by), crate::util::now_ms());
        self.inner.lock().unwrap().store.task_release(id, &by, now)?;
        self.wrote();
        self.event_count(id)
    }

    /// How much history the task now carries: the client's proof that its write landed, without a second read.
    fn event_count(&self, id: &str) -> Result<Value> {
        let n = self.inner.lock().unwrap().store.task_events(id)?.len();
        Ok(json!({ "n": n }))
    }
}
