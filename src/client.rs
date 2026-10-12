//! Connection to the local daemon, starting it if needed. Long-lived clients (`tincan mcp`) reconnect on their
//! own and re-register their sessions, so a daemon restart is invisible to the agent.

use crate::{paths, proto::*};
use anyhow::{Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    collections::HashMap,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::{mpsc, oneshot},
};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

#[derive(Clone)]
struct Conn {
    tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    alive: Arc<AtomicBool>,
}

pub struct Link {
    conn: tokio::sync::Mutex<Option<Conn>>,
    regs: Mutex<HashMap<String, SessionReg>>,
    /// Where pushed events go. `None` for one-shot clients, which also don't reconnect.
    events: Option<mpsc::UnboundedSender<Event>>,
    next_id: AtomicU64,
    /// Start the daemon when it isn't running.
    start: bool,
}

impl Link {
    pub fn new(events: Option<mpsc::UnboundedSender<Event>>) -> Arc<Self> {
        Self::with(events, true)
    }

    /// A one-shot client that never starts the daemon: it fails while none is running.
    pub fn existing() -> Arc<Self> {
        Self::with(None, false)
    }

    fn with(events: Option<mpsc::UnboundedSender<Event>>, start: bool) -> Arc<Self> {
        Arc::new(Link { conn: Default::default(), regs: Default::default(), events, next_id: AtomicU64::new(1), start })
    }

    pub async fn call<T: DeserializeOwned>(self: &Arc<Self>, req: Request) -> Result<T> {
        let conn = self.connect().await?;
        let v = self.raw(&conn, &req).await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn register(self: &Arc<Self>, reg: SessionReg) -> Result<Registered> {
        let r = self.call(Request::Register { session: reg.clone() }).await?;
        self.regs.lock().unwrap().insert(reg.key.clone(), reg);
        Ok(r)
    }

    pub fn is_registered(&self, key: &str) -> bool {
        self.regs.lock().unwrap().contains_key(key)
    }

    async fn raw(&self, conn: &Conn, req: &Request) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        let line = serde_json::to_string(&Frame { id, body: req })?;
        conn.tx.send(line).map_err(|_| anyhow!("tincan daemon connection closed"))?;
        let reply = rx.await.map_err(|_| anyhow!("tincan daemon connection closed"))?;
        if reply.ok {
            Ok(reply.result.unwrap_or(Value::Null))
        } else {
            bail!(reply.error.unwrap_or_else(|| "unknown error".into()))
        }
    }

    async fn connect(self: &Arc<Self>) -> Result<Conn> {
        let mut slot = self.conn.lock().await;
        if let Some(c) = slot.as_ref()
            && c.alive.load(Ordering::Relaxed)
        {
            return Ok(c.clone());
        }
        let c = self.spawn_io(connect_or_start(self.start).await?);
        let regs: Vec<SessionReg> = self.regs.lock().unwrap().values().cloned().collect();
        for reg in regs {
            self.raw(&c, &Request::Register { session: reg }).await?;
        }
        *slot = Some(c.clone());
        Ok(c)
    }

    fn spawn_io(self: &Arc<Self>, stream: UnixStream) -> Conn {
        let (rd, mut wr) = stream.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = Conn { tx, pending: Pending::default(), alive: Arc::new(AtomicBool::new(true)) };
        tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if wr.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let (pending, alive, events, link) = (conn.pending.clone(), conn.alive.clone(), self.events.clone(), Arc::downgrade(self));
        tokio::spawn(async move {
            let mut lines = BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if v.get("event").is_some() {
                    if let (Some(events), Ok(ev)) = (&events, serde_json::from_value::<Event>(v)) {
                        let _ = events.send(ev);
                    }
                } else if let Ok(reply) = serde_json::from_value::<Reply>(v)
                    && let Some(tx) = pending.lock().unwrap().remove(&reply.id)
                {
                    let _ = tx.send(reply);
                }
            }
            alive.store(false, Ordering::Relaxed);
            pending.lock().unwrap().clear();
            if events.is_some() {
                tokio::spawn(reconnect(link));
            }
        });
        conn
    }
}

async fn reconnect(link: Weak<Link>) {
    // Backs off: every agent session runs one of these, and they must not storm a daemon that can't start.
    let mut wait = Duration::from_secs(1);
    loop {
        tokio::time::sleep(wait).await;
        let Some(link) = link.upgrade() else { return };
        match link.connect().await {
            Ok(_) => return,
            Err(e) => eprintln!("tincan: reconnecting to daemon: {e:#}"),
        }
        wait = (wait * 2).min(Duration::from_secs(30));
    }
}

/// The daemon that holds the lock, if it is alive (it may still be starting).
pub fn running_daemon() -> Option<u32> {
    std::fs::read_to_string(paths::lock()).ok()?.trim().parse().ok().filter(|p| crate::util::pid_alive(*p))
}

async fn connect_or_start(start: bool) -> Result<UnixStream> {
    let sock = paths::socket();
    if let Ok(s) = UnixStream::connect(&sock).await {
        return Ok(s);
    }
    if !start {
        bail!("the tincan daemon isn't running");
    }
    // One is running (it may be starting or restarting): wait for it rather than start another.
    if running_daemon().is_none() {
        // With the background service installed, the service starts it, so there is only ever its daemon.
        let by_service = crate::setup::service_installed()
            && std::env::var_os("TINCAN_NO_SERVICE").is_none()
            && crate::setup::start_service();
        if !by_service {
            start_daemon()?;
        }
    }
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(s) = UnixStream::connect(&sock).await {
            return Ok(s);
        }
    }
    bail!("could not start the tincan daemon; see {}", paths::log().display())
}

/// Starts `tincan daemon` detached from this process group, logging to `~/.tincan/daemon.log`.
fn start_daemon() -> Result<()> {
    paths::ensure_home()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::log())?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()?;
    // Reaped when it exits, so a long-lived client (`tincan mcp`) never collects zombies.
    std::thread::spawn(move || child.wait());
    Ok(())
}
