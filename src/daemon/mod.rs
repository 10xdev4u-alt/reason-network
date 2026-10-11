//! The per-machine daemon: knows the live sessions here, talks to teammates' daemons, routes messages, keeps
//! mailboxes. Split by concern: `sessions` (who is running here), `directory` (who is where, and resolving
//! addresses), `mail` (sending, receiving, delivering), `teamops` (membership and the network).

mod directory;
mod mail;
mod sessions;
mod teamops;

use crate::{
    config::Config,
    git::{self, GitInfo},
    net::Net,
    paths,
    proto::*,
    store::Store,
    team::TeamState,
    util,
};
use anyhow::{Context, Result, anyhow};
use iroh::SecretKey;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Notify, mpsc},
};

const GIT_TTL: Duration = Duration::from_secs(30);
const MAX_WAIT: u64 = 600;

/// Runs the daemon. Only one runs per `COMMS_HOME`: a second one exits, or with `service` waits its turn.
pub async fn run(service: bool) -> Result<()> {
    paths::ensure_home()?;
    let lock = tokio::task::spawn_blocking(move || util::lock(&paths::lock(), service)).await??;
    let Some(_lock) = lock else {
        log("another daemon is already running");
        return Ok(());
    };
    let sock = paths::socket();
    paths::ensure_socket_dir()?;
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    util::chmod(&sock, 0o600)?;
    let daemon = Arc::new(Daemon::open()?);
    log(&format!("listening on {} as {}@{}", sock.display(), daemon.person, daemon.machine));
    if daemon.team_state().is_some()
        && let Err(e) = daemon.start_net().await
    {
        log(&format!("network not started: {e:#}"));
    }
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(daemon.clone().serve(stream));
    }
}

fn log(msg: &str) {
    eprintln!("[{}] {msg}", util::now_ms());
}

pub struct Daemon {
    person: String,
    machine: String,
    cfg: Config,
    key: SecretKey,
    inner: Mutex<Inner>,
    /// Woken on every new message, for `Inbox` long-polls.
    arrived: Notify,
    git_cache: Mutex<HashMap<String, (Instant, GitInfo)>>,
    net: RwLock<Option<Arc<Net>>>,
    /// Recent message times per sender, for the rate limit.
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
}

struct Inner {
    store: Store,
    live: HashMap<String, Live>,
    conns: HashMap<u64, mpsc::UnboundedSender<String>>,
    next_conn: u64,
    team: Option<TeamState>,
    /// Messages handed to a push channel and not yet confirmed; hidden from inbox and hooks meanwhile.
    in_flight: HashMap<String, Instant>,
    /// Sessions waiting in `check_inbox` right now (they get new mail there instead of a push).
    polling: HashMap<String, usize>,
    /// Recent agent-to-agent message times per (from, to), for the loop guard.
    pairs: HashMap<(String, String), VecDeque<Instant>>,
}

/// A session running on this machine.
struct Live {
    reg: SessionReg,
    name: String,
    /// The client connection (`comms mcp`) that owns it; `None` for sessions known only through hooks.
    conn: Option<u64>,
    git: GitInfo,
    doing: Option<String>,
    since: i64,
    /// From hooks: `busy`/`idle` and when it changed.
    status: Option<(String, i64)>,
    last_hook: Option<i64>,
    /// Its push channel failed (e.g. `codex queue`); stick to hooks from now on.
    push_failed: bool,
}

fn load_key() -> Result<SecretKey> {
    let path = paths::home().join("machine.key");
    if let Ok(bytes) = std::fs::read(&path) {
        let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| anyhow!("{} is corrupt", path.display()))?;
        return Ok(SecretKey::from_bytes(&arr));
    }
    let key = SecretKey::generate();
    std::fs::write(&path, key.to_bytes())?;
    util::chmod(&path, 0o600)?;
    Ok(key)
}

impl Daemon {
    fn open() -> Result<Self> {
        let cfg = Config::load();
        let store = Store::open(&paths::db())?;
        let team: Option<TeamState> = store.get("team")?.and_then(|s| serde_json::from_str(&s).ok());
        let key = load_key()?;
        // In a team, who we are is what the signed roster says, whatever the config file says.
        let member = team.as_ref().and_then(|t| t.member(&key.public()).cloned());
        Ok(Daemon {
            person: member.as_ref().map_or_else(|| cfg.person(), |m| m.person.clone()),
            machine: member.map_or_else(util::hostname, |m| m.machine),
            key,
            cfg,
            inner: Mutex::new(Inner {
                store,
                live: HashMap::new(),
                conns: HashMap::new(),
                next_conn: 0,
                team,
                in_flight: HashMap::new(),
                polling: HashMap::new(),
                pairs: HashMap::new(),
            }),
            arrived: Notify::new(),
            git_cache: Mutex::new(HashMap::new()),
            net: RwLock::new(None),
            rate: Mutex::new(HashMap::new()),
        })
    }

    fn addr(&self, name: &str) -> String {
        format!("{}/{name}", self.person)
    }

    fn net(&self) -> Option<Arc<Net>> {
        self.net.read().unwrap().clone()
    }

    async fn serve(self: Arc<Self>, stream: UnixStream) {
        let (rd, mut wr) = stream.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = {
            let mut g = self.inner.lock().unwrap();
            g.next_conn += 1;
            let id = g.next_conn;
            g.conns.insert(id, tx.clone());
            id
        };
        let writer = tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if wr.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let mut lines = BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let (this, tx) = (self.clone(), tx.clone());
            // Each request runs on its own task: an inbox long-poll must not block the connection.
            tokio::spawn(async move {
                let reply = match serde_json::from_str::<Frame<Request>>(&line) {
                    Ok(f) => match this.handle(conn, f.body).await {
                        Ok(v) => Reply::ok(f.id, v),
                        Err(e) => Reply::err(f.id, e),
                    },
                    Err(e) => Reply::err(0, anyhow!("bad request: {e}")),
                };
                let _ = tx.send(serde_json::to_string(&reply).expect("reply serializes"));
            });
        }
        self.drop_conn(conn);
        writer.abort();
    }

    async fn handle(self: &Arc<Self>, conn: u64, req: Request) -> Result<Value> {
        Ok(match req {
            Request::Register { session } => {
                let git = self.git(&session.cwd).await;
                json!(self.register(Some(conn), session, git)?)
            }
            Request::SetStatus { key, doing, name } => json!(self.set_status(&key, doing, name)?),
            Request::Who { me, filter, all } => json!(self.who(me, filter, all).await?),
            Request::Send { from, to, body, reply_to, wake } => json!(self.send(from, &to, &body, reply_to, wake).await?),
            Request::Inbox { key, wait_secs, peek } => json!(self.inbox(key, wait_secs.min(MAX_WAIT), peek).await?),
            Request::Unread { key } => json!(self.unread(&key)?),
            Request::Delivered { ids, .. } => {
                let mut g = self.inner.lock().unwrap();
                g.store.mark_read(&ids, util::now_ms())?;
                for id in &ids {
                    g.in_flight.remove(id);
                }
                Value::Null
            }
            Request::Hook { agent, event, session_id, cwd, pid, mail } => {
                json!(self.hook(&agent, &event, &session_id, cwd, pid, mail).await?)
            }
            Request::Attach { agent, cwd } => json!(self.attach(&agent, &cwd)),
            Request::CreateTeam { name, person } => json!(self.create_team(&name, &person).await?),
            Request::Join { ticket, person } => json!(self.join_team(&ticket, &person).await?),
            Request::Invite { me, hours } => json!(self.invite(me, hours).await?),
            Request::Team => json!(self.team_info()?),
            Request::Remove { who } => json!(self.remove(&who)?),
            Request::Leave { force } => json!(self.leave(force).await?),
            Request::SetRelay { url } => json!(self.set_relay(url).await?),
            Request::Held => json!(self.inner.lock().unwrap().store.held()?),
            Request::Settle { ids, release } => json!(self.settle(&ids, release)?),
            Request::Shutdown => {
                self.restart_soon();
                Value::Null
            }
        })
    }

    /// Exits shortly (after the reply goes out). The service manager, or the next client, starts a fresh
    /// daemon, which picks up changed config and team state.
    fn restart_soon(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            if let Some(net) = this.net() {
                net.shutdown().await;
            }
            let _ = std::fs::remove_file(paths::socket());
            std::process::exit(0);
        });
    }

    async fn git(&self, cwd: &str) -> GitInfo {
        if let Some((at, info)) = self.git_cache.lock().unwrap().get(cwd)
            && at.elapsed() < GIT_TTL
        {
            return info.clone();
        }
        let dir = cwd.to_string();
        let info = tokio::task::spawn_blocking(move || git::lookup(&dir)).await.unwrap_or_default();
        self.git_cache.lock().unwrap().insert(cwd.to_string(), (Instant::now(), info.clone()));
        info
    }
}
