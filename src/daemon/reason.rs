//! Reason sessions. Reason's agent runs in its cloud, so there is no hook or MCP server here: when `reason` is on
//! (`tincan setup`), the daemon lists your live Reason sessions (`reason session list --json`) every few seconds,
//! and delivers to them with `reason session send`. They reply through the tincan CLI, which runs here.

use super::{Daemon, log};
use crate::{agent, git::GitInfo, proto::*, util};
use anyhow::{Result, bail};
use serde_json::Value;
use std::{collections::HashSet, sync::Arc, time::Duration};

const EVERY: Duration = Duration::from_secs(15);

impl Daemon {
    pub(super) async fn reason_loop(self: Arc<Self>) {
        let mut failing = false;
        loop {
            match self.reason_sync().await {
                Ok(()) => failing = false,
                Err(e) if !failing => {
                    log(&format!("listing Reason sessions: {e:#}"));
                    failing = true;
                }
                Err(_) => {}
            }
            tokio::time::sleep(EVERY).await;
        }
    }

    async fn reason_sync(self: &Arc<Self>) -> Result<()> {
        let Some(bin) = agent::find_bin("reason") else { return Ok(()) };
        let out = tokio::process::Command::new(bin)
            .args(["session", "list", "--limit", "20", "--json"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(Duration::from_secs(30), out).await??;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let list: Value = serde_json::from_slice(&out.stdout)?;
        let live: Vec<&Value> = list["items"]
            .as_array()
            .map(|a| a.iter().filter(|s| s["finished_at"].is_null() && s["is_archived"] != true).collect())
            .unwrap_or_default();

        let now = util::now_ms();
        let mut seen = HashSet::new();
        for s in live {
            let Some(id) = s["session_id"].as_str().or(s["id"].as_str()) else { continue };
            let key = format!("reason:{id}");
            let text = |k: &str| s[k].as_str().filter(|v| !v.is_empty()).map(str::to_string);
            let title = text("title");
            seen.insert(key.clone());
            if !self.inner.lock().unwrap().live.contains_key(&key) {
                let git = GitInfo { root: None, repo: text("repo"), branch: text("branch") };
                // Named for its repo, else for the first words of its title (kept once chosen).
                let name = git.repo.is_none().then(|| title.as_deref().map(first_words)).flatten().map(|t| format!("{t}-reason"));
                let reg = SessionReg {
                    key: key.clone(),
                    agent: "reason".into(),
                    cwd: "~".into(),
                    agent_session_id: Some(id.to_string()),
                    pid: None,
                    tier: Tier::Push,
                    name,
                };
                self.register(None, reg, git)?;
            }
            let mut g = self.inner.lock().unwrap();
            if let Some(l) = g.live.get_mut(&key) {
                // Seen just now: keeps it from being pruned like a session whose hooks went quiet.
                l.last_hook = Some(now);
                l.doing = title;
                let status = if s["status"].as_str() == Some("running") { "busy" } else { "idle" };
                if l.status.as_ref().is_none_or(|(st, _)| st != status) {
                    l.status = Some((status.to_string(), now));
                }
            }
            // Mail a failed push left behind goes out again.
            let address = g.live.get(&key).map(|l| self.addr(&l.name));
            if let Some(address) = address {
                let waiting: Vec<_> = g.pending(&address)?.into_iter().filter(|m| m.wake).collect();
                if !waiting.is_empty() {
                    self.deliver(&mut g, &key, waiting, true);
                }
            }
        }
        let mut g = self.inner.lock().unwrap();
        let gone: Vec<String> = g.live.iter().filter(|(k, l)| l.reg.agent == "reason" && !seen.contains(*k)).map(|(k, _)| k.clone()).collect();
        for key in gone {
            if let Some(l) = g.live.remove(&key) {
                let _ = g.store.touch_session(&l.name, now);
            }
        }
        Ok(())
    }
}

fn first_words(title: &str) -> String {
    util::slug(&title.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
}
