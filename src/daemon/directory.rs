//! Who is where: this machine's sessions, teammates' sessions (from their presence), and resolving an address
//! like `alice/api-cursor`, `repo:api` or `*` to recipients.

use super::Daemon;
use crate::{
    agent,
    claude::{self, ClaudeSession},
    git::GitInfo,
    net::{Presence, Remote},
    proto::*,
    util,
};
use anyhow::{Result, bail};
use iroh::EndpointId;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

/// A resolved recipient.
pub(super) enum Target {
    /// A session on this machine; `key` is set when it is running.
    Session { address: String, key: Option<String> },
    /// The human at this machine.
    Human,
    /// A session or person on a teammate's machine.
    Remote { address: String, machine: EndpointId, machine_name: String, online: bool },
}

impl Target {
    pub(super) fn address(&self, me: &str) -> String {
        match self {
            Target::Session { address, .. } | Target::Remote { address, .. } => address.clone(),
            Target::Human => me.to_string(),
        }
    }
}

/// Something an address can match.
struct Cand {
    person: String,
    name: String,
    machine: String,
    live: bool,
    repo: Option<String>,
    place: Place,
}

enum Place {
    Local(Option<String>),
    Remote(EndpointId, bool),
}

impl Daemon {
    /// This machine's sessions: running ones, plus (if `unmanaged`) Claude Code sessions without comms.
    pub(super) async fn local_rows(self: &Arc<Self>, me: Option<&str>, unmanaged: bool) -> Result<Vec<SessionInfo>> {
        let claude = tokio::task::spawn_blocking(claude::scan).await?;
        let mut cwds: Vec<String> = self.inner.lock().unwrap().live.values().map(|l| l.reg.cwd.clone()).collect();
        cwds.extend(claude.iter().filter_map(|c| c.cwd.clone()));
        let mut gits: HashMap<String, GitInfo> = HashMap::new();
        for cwd in cwds {
            let info = self.git(&cwd).await;
            gits.insert(cwd, info);
        }

        let mut g = self.inner.lock().unwrap();
        self.prune(&mut g);
        for l in g.live.values_mut() {
            if let Some(info) = gits.get(&l.reg.cwd) {
                l.git = info.clone();
            }
        }
        let mut rows = Vec::new();
        let mut matched = HashSet::new();
        for l in g.live.values() {
            let mut row = self.row(&l.name, &l.reg.agent, &l.reg.cwd, Some(&l.git));
            row.since = Some(l.since);
            if let Some((st, at)) = &l.status {
                row.status = st.clone();
                row.since = Some(*at);
            }
            // Claude Code's registry knows whether the session is busy or idle.
            if let Some(c) = claude.iter().find(|c| claude_match(c, &l.reg)) {
                matched.insert(c.pid);
                if let Some(st) = &c.status {
                    row.status = st.clone();
                    row.since = c.status_updated_at.or(row.since);
                }
            }
            row.doing = l.doing.clone();
            row.tier = l.reg.tier;
            row.unread = g.store.unread_count(&row.address)?;
            row.is_me = me == Some(l.reg.key.as_str());
            rows.push(row);
        }
        if unmanaged {
            for c in claude.iter().filter(|c| !matched.contains(&c.pid)) {
                let cwd = c.cwd.clone().unwrap_or_default();
                let name = c.name.clone().unwrap_or_else(|| format!("claude-{}", c.pid));
                let mut row = self.row(&name, agent::CLAUDE_CODE, &cwd, gits.get(&cwd));
                row.status = c.status.clone().unwrap_or_else(|| "live".into());
                row.since = c.status_updated_at.or(c.started_at);
                rows.push(row);
            }
        }
        Ok(rows)
    }

    fn row(&self, name: &str, agent: &str, cwd: &str, git: Option<&GitInfo>) -> SessionInfo {
        SessionInfo {
            address: self.addr(name),
            name: name.to_string(),
            person: self.person.clone(),
            machine: self.machine.clone(),
            agent: agent.to_string(),
            cwd: util::tilde(cwd),
            repo: git.and_then(|g| g.repo.clone()),
            branch: git.and_then(|g| g.branch.clone()),
            status: "live".into(),
            since: None,
            doing: None,
            tier: Tier::None,
            unread: 0,
            is_me: false,
        }
    }

    /// What this machine tells teammates about itself.
    pub async fn presence(self: &Arc<Self>) -> Presence {
        let mut sessions = self.local_rows(None, self.cfg.share_unmanaged).await.unwrap_or_default();
        if !self.cfg.share_paths {
            for s in &mut sessions {
                s.cwd = s.cwd.rsplit('/').find(|p| !p.is_empty()).unwrap_or("~").to_string();
            }
        }
        let g = self.inner.lock().unwrap();
        let live: HashSet<&str> = g.live.values().map(|l| l.name.as_str()).collect();
        let mut known = g.store.sessions().unwrap_or_default();
        known.retain(|r| !live.contains(r.name.as_str()));
        known.sort_by_key(|r| -r.last_seen);
        Presence { sessions, known: known.into_iter().take(100).map(|r| r.name).collect() }
    }

    pub(super) async fn who(self: &Arc<Self>, me: Option<String>, filter: Option<String>, all: bool) -> Result<Directory> {
        let mut rows = self.local_rows(me.as_deref(), true).await?;
        if all {
            let g = self.inner.lock().unwrap();
            let live: HashSet<&str> = g.live.values().map(|l| l.name.as_str()).collect();
            for r in g.store.sessions()?.into_iter().filter(|r| !live.contains(r.name.as_str())) {
                let git = GitInfo { root: None, repo: r.repo.clone(), branch: r.branch.clone() };
                let mut row = self.row(&r.name, &r.agent, &r.cwd, Some(&git));
                row.status = "offline".into();
                row.since = Some(r.last_seen);
                row.doing = r.doing.clone();
                row.tier = r.tier;
                row.unread = g.store.unread_count(&row.address)?;
                rows.push(row);
            }
        }
        let machines = self.machines();
        if let (Some(net), Some(team)) = (self.net(), self.team_state()) {
            for (id, rem) in net.remotes() {
                let Some(m) = team.member(&id).filter(|m| m.active) else { continue };
                if !rem.online && !all {
                    continue;
                }
                for mut s in rem.presence.sessions {
                    // Trust the roster, not the peer, for who and where.
                    s.person = m.person.clone();
                    s.machine = m.machine.clone();
                    s.address = format!("{}/{}", m.person, s.name);
                    s.is_me = false;
                    if !rem.online {
                        s.status = "offline".into();
                        s.since = Some(rem.last_seen);
                    }
                    rows.push(s);
                }
            }
        }
        if let Some(f) = filter.map(|f| f.to_lowercase()).filter(|f| !f.is_empty()) {
            rows.retain(|r| {
                [&r.address, &r.agent, &r.cwd, &r.machine]
                    .into_iter()
                    .chain([&r.repo, &r.branch, &r.doing].into_iter().flatten())
                    .any(|s| s.to_lowercase().contains(&f))
            });
        }
        // Me first, then teammates; on comms before not; live before offline.
        let rank = |r: &SessionInfo| {
            (r.person != self.person, r.person.clone(), r.machine.clone(), r.tier == Tier::None, r.status == "offline", r.name.clone())
        };
        rows.sort_by_key(rank);
        Ok(Directory {
            team: self.team_state().map(|t| t.name().to_string()),
            person: self.person.clone(),
            machine: self.machine.clone(),
            machines,
            sessions: rows,
        })
    }

    /// Every address target we know about, here and on teammates' machines.
    fn candidates(&self, g: &super::Inner, remotes: &HashMap<EndpointId, Remote>) -> Vec<Cand> {
        let mut out = Vec::new();
        let live: HashSet<&str> = g.live.values().map(|l| l.name.as_str()).collect();
        for l in g.live.values() {
            out.push(Cand {
                person: self.person.clone(),
                name: l.name.clone(),
                machine: self.machine.clone(),
                live: true,
                repo: l.git.repo.clone(),
                place: Place::Local(Some(l.reg.key.clone())),
            });
        }
        for r in g.store.sessions().unwrap_or_default().into_iter().filter(|r| !live.contains(r.name.as_str())) {
            out.push(Cand {
                person: self.person.clone(),
                name: r.name,
                machine: self.machine.clone(),
                live: false,
                repo: r.repo,
                place: Place::Local(None),
            });
        }
        let Some(team) = &g.team else { return out };
        for (id, rem) in remotes {
            let Some(m) = team.member(id).filter(|m| m.active) else { continue };
            let names: HashSet<&str> = rem.presence.sessions.iter().map(|s| s.name.as_str()).collect();
            for s in &rem.presence.sessions {
                out.push(Cand {
                    person: m.person.clone(),
                    name: s.name.clone(),
                    machine: m.machine.clone(),
                    live: rem.online && s.tier != Tier::None,
                    repo: s.repo.clone(),
                    place: Place::Remote(*id, rem.online),
                });
            }
            for n in rem.presence.known.iter().filter(|n| !names.contains(n.as_str())) {
                out.push(Cand {
                    person: m.person.clone(),
                    name: n.clone(),
                    machine: m.machine.clone(),
                    live: false,
                    repo: None,
                    place: Place::Remote(*id, rem.online),
                });
            }
        }
        out
    }

    /// Resolves an address. `exclude` is the sender's session key (not messaged by its own broadcasts).
    pub(super) fn resolve(
        &self,
        g: &super::Inner,
        remotes: &HashMap<EndpointId, Remote>,
        to: &str,
        exclude: Option<&str>,
    ) -> Result<Vec<Target>> {
        let all = self.candidates(g, remotes);
        let (spec, at) = match to.rsplit_once('@') {
            Some((s, m)) if !s.is_empty() && !m.is_empty() => (s, Some(m)),
            _ => (to, None),
        };
        let cands: Vec<&Cand> = all
            .iter()
            .filter(|c| at.is_none_or(|m| c.machine == m))
            .filter(|c| !matches!(&c.place, Place::Local(Some(k)) if Some(k.as_str()) == exclude))
            .filter(|c| !(c.person == self.person && c.name == "*"))
            .collect();
        let mut people: Vec<String> = g.team.as_ref().map(|t| t.people()).unwrap_or_default();
        if !people.contains(&self.person) {
            people.push(self.person.clone());
        }
        let target = |c: &Cand| match &c.place {
            Place::Local(key) => Target::Session { address: self.addr(&c.name), key: key.clone() },
            Place::Remote(id, online) => Target::Remote {
                address: format!("{}/{}", c.person, c.name),
                machine: *id,
                machine_name: c.machine.clone(),
                online: *online,
            },
        };
        let live_matching = |f: &dyn Fn(&Cand) -> bool| -> Vec<Target> {
            cands.iter().filter(|c| c.live && f(c)).map(|c| target(c)).collect()
        };

        let targets = if spec == "*" {
            live_matching(&|_| true)
        } else if let Some(repo) = spec.strip_prefix("repo:") {
            let repo = repo.to_lowercase();
            live_matching(&|c| {
                c.repo.as_deref().is_some_and(|r| {
                    let r = r.to_lowercase();
                    r == repo || r.ends_with(&format!("/{repo}"))
                })
            })
        } else if let Some((person, name)) = spec.split_once('/') {
            if !people.iter().any(|p| p == person) {
                bail!("nobody called '{person}' in the team (people: {})", people.join(", "));
            }
            if name == "*" {
                live_matching(&|c| c.person == person)
            } else {
                self.pick(cands.iter().copied().filter(|c| c.person == person && c.name == name).collect(), spec, &target)?
            }
        } else if people.iter().any(|p| p == spec) {
            self.human(spec, remotes, g)?
        } else {
            self.pick(cands.iter().copied().filter(|c| c.name == spec).collect(), spec, &target)?
        };
        if targets.is_empty() {
            bail!("nobody matches '{to}' right now; run list_team to see who is around");
        }
        Ok(targets)
    }

    /// One session among same-named candidates: the live one, else an offline one to queue for.
    fn pick(&self, found: Vec<&Cand>, spec: &str, target: &dyn Fn(&Cand) -> Target) -> Result<Vec<Target>> {
        let live: Vec<&&Cand> = found.iter().filter(|c| c.live).collect();
        let pool: Vec<&&Cand> = if live.is_empty() { found.iter().collect() } else { live };
        match pool.as_slice() {
            [] => bail!(
                "no session called '{spec}'. Run list_team to see who is around (sessions without comms can't \
                 receive)."
            ),
            [one] => Ok(vec![target(one)]),
            many => {
                let opts: Vec<String> = many.iter().map(|c| format!("{}/{}@{}", c.person, c.name, c.machine)).collect();
                bail!("'{spec}' is ambiguous: {}", opts.join(", "))
            }
        }
    }

    /// A person's human inbox: here if it's me, else on each of their machines that is online (all of them
    /// if none is, to wait in the outbox).
    fn human(&self, person: &str, remotes: &HashMap<EndpointId, Remote>, g: &super::Inner) -> Result<Vec<Target>> {
        if person == self.person {
            return Ok(vec![Target::Human]);
        }
        let Some(team) = &g.team else { bail!("no team") };
        let machines: Vec<(EndpointId, String, bool)> = team
            .active()
            .filter(|m| m.person == person)
            .map(|m| (m.endpoint, m.machine.clone(), remotes.get(&m.endpoint).is_some_and(|r| r.online)))
            .collect();
        let online: Vec<_> = machines.iter().filter(|m| m.2).cloned().collect();
        let pick = if online.is_empty() { machines } else { online };
        Ok(pick
            .into_iter()
            .map(|(id, name, online)| Target::Remote { address: person.to_string(), machine: id, machine_name: name, online })
            .collect())
    }
}

fn claude_match(c: &ClaudeSession, reg: &SessionReg) -> bool {
    (reg.agent_session_id.is_some() && c.session_id == reg.agent_session_id) || reg.pid == Some(c.pid)
}
