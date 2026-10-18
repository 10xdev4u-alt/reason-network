//! SQLite: messages, and every session name ever used on this machine (so a new session can inherit a
//! stale name and its unread mail).

use crate::proto::{Message, Tier};
use anyhow::{Result, anyhow, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::Path;

pub struct Store {
    db: Connection,
}

/// What became of one message this machine sent. The ack used to leave nothing behind but an absence, and an
/// absence cannot say whether the mail arrived or was given up on: a receipt outlives the outbox row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Receipt {
    pub id: String,
    pub to: String,
    pub state: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct Remembered {
    pub name: String,
    pub key: String,
    pub agent: String,
    pub cwd: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub doing: Option<String>,
    pub tier: Tier,
    pub last_seen: i64,
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    sender TEXT NOT NULL,
    sender_agent TEXT,
    sender_repo TEXT,
    recipient TEXT NOT NULL,
    via TEXT,
    reply_to TEXT,
    body TEXT NOT NULL,
    wake INTEGER NOT NULL,
    sent_at INTEGER NOT NULL,
    read_at INTEGER,
    held INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS messages_unread ON messages (recipient, read_at);
CREATE TABLE IF NOT EXISTS outbox (
    id TEXT PRIMARY KEY,
    target TEXT NOT NULL,
    msg TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    name TEXT PRIMARY KEY,
    key TEXT NOT NULL,
    agent TEXT NOT NULL,
    cwd TEXT NOT NULL,
    repo TEXT,
    branch TEXT,
    doing TEXT,
    tier TEXT NOT NULL,
    last_seen INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_key ON sessions (key);
";

/// The v1 schema — frozen: schema changes are forward-only steps appended to [`MIGRATIONS`], never edits here
/// (an edit would create a table that the next migration step then fails to alter).
const SCHEMA_VERSION: i64 = 2;

/// Forward-only migration steps as (version reached, SQL); dense, every version up to [`SCHEMA_VERSION`] has
/// one. Each step runs in one transaction together with its version stamp: it lands whole or not at all.
/// Never edit or reorder a shipped step, only append. `(1, "")` adopts pre-versioning state — those databases
/// carry exactly this schema, since re-running `CREATE IF NOT EXISTS` used to be the whole migration story.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, ""),
    (
        2,
        "CREATE TABLE IF NOT EXISTS receipts (id TEXT PRIMARY KEY, to_addr TEXT NOT NULL, state TEXT NOT NULL, at_ms INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS receipts_at ON receipts (at_ms);
         CREATE INDEX IF NOT EXISTS receipts_to ON receipts (to_addr, at_ms);",
    ),
];

/// Applies every step from `from + 1` up to `to`; returns the version reached.
fn apply_migrations(db: &mut Connection, from: i64, to: i64, steps: &[(i64, &str)]) -> Result<i64> {
    let mut v = from;
    while v < to {
        let (next, sql) = steps
            .iter()
            .find(|(n, _)| *n == v + 1)
            .ok_or_else(|| anyhow!("no migration from schema v{v} to v{}", v + 1))?;
        let tx = db.transaction()?;
        if !sql.is_empty() {
            tx.execute_batch(sql)?;
        }
        tx.execute_batch(&format!("PRAGMA user_version = {next}"))?;
        tx.commit()?;
        v = *next;
    }
    Ok(v)
}

const MESSAGE_COLS: &str = "id, sender, sender_agent, sender_repo, recipient, via, reply_to, body, wake, sent_at";
const SESSION_COLS: &str = "name, key, agent, cwd, repo, branch, doing, tier, last_seen";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let mut db = Connection::open(path)?;
        // Downgrade safety (09 §3): a state from a newer build is refused before anything is written.
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!(
                "this state was written by a newer reason-network (schema v{version} > v{SCHEMA_VERSION}); \
                 upgrade it, or point REASON_NETWORK_HOME at a fresh home"
            );
        }
        db.execute_batch(SCHEMA)?;
        if version < SCHEMA_VERSION {
            apply_migrations(&mut db, version, SCHEMA_VERSION, MIGRATIONS)?;
            // Fold the WAL into the database (as paths.rs does before its rename) so a naive .db-only copy is
            // whole: the live state sat in a 2.9MB WAL behind a 4KB main file (12 §4). Best-effort — a busy
            // checkpoint still leaves a correct database.
            let _ = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        }
        Ok(Store { db })
    }

    /// Stores a message; false if one with this id is already here (a retried delivery).
    pub fn insert(&self, m: &Message, held: bool) -> Result<bool> {
        let n = self.db.execute(
            &format!(
                "INSERT OR IGNORE INTO messages ({MESSAGE_COLS}, held) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            ),
            params![m.id, m.from, m.from_agent, m.from_repo, m.to, m.via, m.reply_to, m.body, m.wake, m.sent_at, held],
        )?;
        Ok(n == 1)
    }

    pub fn has(&self, id: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT 1 FROM messages WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
    }

    pub fn unread(&self, recipient: &str) -> Result<Vec<Message>> {
        let mut st = self.db.prepare_cached(&format!(
            "SELECT {MESSAGE_COLS} FROM messages WHERE recipient = ?1 AND read_at IS NULL AND held = 0 ORDER BY sent_at, id"
        ))?;
        let rows = st.query_map([recipient], message)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn unread_count(&self, recipient: &str) -> Result<u32> {
        Ok(self.db.query_row(
            "SELECT COUNT(*) FROM messages WHERE recipient = ?1 AND read_at IS NULL AND held = 0",
            [recipient],
            |r| r.get(0),
        )?)
    }

    pub fn held(&self) -> Result<Vec<Message>> {
        let mut st = self
            .db
            .prepare_cached(&format!("SELECT {MESSAGE_COLS} FROM messages WHERE held = 1 ORDER BY sent_at, id"))?;
        let rows = st.query_map([], message)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Approves (`release`) or drops held messages; returns the ones affected.
    pub fn settle_held(&self, ids: &[String], release: bool) -> Result<Vec<Message>> {
        let held = self.held()?;
        let picked: Vec<Message> = held.into_iter().filter(|m| ids.is_empty() || ids.contains(&m.id)).collect();
        let sql =
            if release { "UPDATE messages SET held = 0 WHERE id = ?1" } else { "DELETE FROM messages WHERE id = ?1" };
        let mut st = self.db.prepare_cached(sql)?;
        for m in &picked {
            st.execute([&m.id])?;
        }
        Ok(picked)
    }

    /// Forgets read, non-held messages older than `age` (09 §4); returns how many went. Unread and held mail
    /// is never touched — `read_at` is only ever set once the message was shown to its agent.
    pub fn prune_messages(&self, now: i64, age: i64) -> Result<usize> {
        Ok(self
            .db
            .execute("DELETE FROM messages WHERE sent_at < ?1 AND read_at IS NOT NULL AND held = 0", [now - age])?)
    }

    /// Forgets session rows unseen for `age` (09 §4) — except one whose mail at `person/<name>` is still
    /// unread: that row is what makes the mail reachable (`who --all`, name inheritance), so it stays.
    pub fn prune_sessions(&self, now: i64, age: i64, person: &str) -> Result<usize> {
        let sql = "DELETE FROM sessions WHERE last_seen < ?1 AND NOT EXISTS (
            SELECT 1 FROM messages m WHERE m.read_at IS NULL AND m.recipient = ?2 || '/' || sessions.name)";
        Ok(self.db.execute(sql, params![now - age, person])?)
    }

    pub fn outbox_add(&self, target: &str, m: &Message) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO outbox (id, target, msg, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![m.id, target, serde_json::to_string(m)?, m.sent_at],
        )?;
        Ok(())
    }

    /// Undelivered messages for a machine; older than a week are given up on. Whatever is dropped is recorded in
    /// the ring first, so the message that never made it leaves a trace instead of an absence.
    pub fn outbox_for(&self, target: &str, now: i64) -> Result<Vec<Message>> {
        self.give_up(now - 7 * 86_400_000, now)?;
        let mut st = self.db.prepare_cached("SELECT msg FROM outbox WHERE target = ?1 ORDER BY created_at, id")?;
        let rows = st.query_map([target], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok().and_then(|s| serde_json::from_str(&s).ok())).collect())
    }

    fn give_up(&self, cutoff: i64, now: i64) -> Result<usize> {
        let tx = self.db.unchecked_transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare("SELECT msg FROM outbox WHERE created_at < ?1")?;
            let rows = st.query_map([cutoff], |r| r.get::<_, String>(0))?;
            for s in rows.flatten() {
                if let Ok(m) = serde_json::from_str::<Message>(&s) {
                    tx.execute(
                        "INSERT OR REPLACE INTO receipts (id, to_addr, state, at_ms) VALUES (?1, ?2, 'abandoned', ?3)",
                        params![m.id, m.to, now],
                    )?;
                    n += 1;
                }
            }
        }
        tx.execute("DELETE FROM outbox WHERE created_at < ?1", [cutoff])?;
        tx.commit()?;
        Ok(n)
    }

    /// Settles one outbox row and keeps the receipt **in the same transaction**: the ack was a bare `DELETE`,
    /// which is why "delivered" and "given up on" were one observation — the row was simply gone.
    pub fn outbox_settle(&self, id: &str, state: &str, now: i64) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        let row: Option<String> =
            tx.query_row("SELECT msg FROM outbox WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        if let Some(m) = row.and_then(|s| serde_json::from_str::<Message>(&s).ok()) {
            tx.execute(
                "INSERT OR REPLACE INTO receipts (id, to_addr, state, at_ms) VALUES (?1, ?2, ?3, ?4)",
                params![m.id, m.to, state, now],
            )?;
        }
        tx.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(())
    }

    /// What happened to the mail this machine sent, newest first: the only place an ack, a refusal or a message
    /// given up on can still be read.
    pub fn receipts(&self, id: Option<&str>, to: Option<&str>, since: Option<i64>) -> Result<Vec<Receipt>> {
        let mut st = self.db.prepare_cached(
            "SELECT id, to_addr, state, at_ms FROM receipts
             WHERE (?1 IS NULL OR id = ?1) AND (?2 IS NULL OR to_addr = ?2) AND (?3 IS NULL OR at_ms >= ?3)
             ORDER BY at_ms DESC, id LIMIT 500",
        )?;
        let rows = st.query_map(params![id, to, since], |r| {
            Ok(Receipt { id: r.get(0)?, to: r.get(1)?, state: r.get(2)?, at_ms: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets receipts past the age horizon and any beyond `max_rows`, oldest first.
    pub fn prune_receipts(&self, now: i64, age: i64, max_rows: i64) -> Result<usize> {
        let mut n = self.db.execute("DELETE FROM receipts WHERE at_ms < ?1", [now - age])?;
        n += self.db.execute(
            "DELETE FROM receipts WHERE id NOT IN (SELECT id FROM receipts ORDER BY at_ms DESC, id LIMIT ?1)",
            [max_rows],
        )?;
        Ok(n)
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)", params![key, value])?;
        Ok(())
    }

    pub fn prefixed(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut st = self.db.prepare_cached("SELECT key, value FROM kv WHERE key >= ?1 AND key < ?1 || x'ff'")?;
        let rows = st.query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_prefix(&self, prefix: &str) -> Result<()> {
        self.db.execute("DELETE FROM kv WHERE key >= ?1 AND key < ?1 || x'ff'", [prefix])?;
        Ok(())
    }

    pub fn mark_read(&self, ids: &[String], now: i64) -> Result<()> {
        let mut st = self.db.prepare_cached("UPDATE messages SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL")?;
        for id in ids {
            st.execute(params![id, now])?;
        }
        Ok(())
    }

    /// A renamed session keeps its unread mail.
    pub fn rename(&self, old_name: &str, new_name: &str, old_addr: &str, new_addr: &str) -> Result<()> {
        self.db.execute(
            "UPDATE messages SET recipient = ?2 WHERE recipient = ?1 AND read_at IS NULL",
            params![old_addr, new_addr],
        )?;
        self.db.execute("DELETE FROM sessions WHERE name = ?1", [new_name])?;
        self.db.execute("UPDATE sessions SET name = ?2 WHERE name = ?1", params![old_name, new_name])?;
        Ok(())
    }

    pub fn upsert_session(&self, s: &Remembered) -> Result<()> {
        self.db.execute(
            &format!(
                "INSERT INTO sessions ({SESSION_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (name) DO UPDATE SET key = ?2, agent = ?3, cwd = ?4, repo = ?5, branch = ?6,
                 doing = ?7, tier = ?8, last_seen = ?9"
            ),
            params![s.name, s.key, s.agent, s.cwd, s.repo, s.branch, s.doing, s.tier.as_str(), s.last_seen],
        )?;
        Ok(())
    }

    pub fn touch_session(&self, name: &str, now: i64) -> Result<()> {
        self.db.execute("UPDATE sessions SET last_seen = ?2 WHERE name = ?1", params![name, now])?;
        Ok(())
    }

    pub fn session_by_key(&self, key: &str) -> Result<Option<Remembered>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE key = ?1 ORDER BY last_seen DESC LIMIT 1");
        Ok(self.db.query_row(&sql, [key], remembered).optional()?)
    }

    pub fn session_by_name(&self, name: &str) -> Result<Option<Remembered>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE name = ?1");
        Ok(self.db.query_row(&sql, [name], remembered).optional()?)
    }

    pub fn sessions(&self) -> Result<Vec<Remembered>> {
        let mut st = self.db.prepare_cached(&format!("SELECT {SESSION_COLS} FROM sessions ORDER BY name"))?;
        let rows = st.query_map([], remembered)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn message(r: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        from: r.get(1)?,
        from_agent: r.get(2)?,
        from_repo: r.get(3)?,
        to: r.get(4)?,
        via: r.get(5)?,
        reply_to: r.get(6)?,
        body: r.get(7)?,
        wake: r.get(8)?,
        notify: false,
        sent_at: r.get(9)?,
    })
}

fn remembered(r: &Row) -> rusqlite::Result<Remembered> {
    Ok(Remembered {
        name: r.get(0)?,
        key: r.get(1)?,
        agent: r.get(2)?,
        cwd: r.get(3)?,
        repo: r.get(4)?,
        branch: r.get(5)?,
        doing: r.get(6)?,
        tier: Tier::parse(&r.get::<_, String>(7)?),
        last_seen: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway database file (plus its WAL/SHM siblings), cleared before use.
    fn fresh(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("rn-store-test-{}-{tag}.db", std::process::id()));
        cleanup(&p);
        p
    }

    fn cleanup(p: &std::path::Path) {
        for f in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{f}", p.display()));
        }
    }

    fn msg(id: &str, to: &str, sent_at: i64) -> Message {
        Message {
            id: id.into(),
            from: "me/x".into(),
            from_agent: None,
            from_repo: None,
            to: to.into(),
            via: None,
            reply_to: None,
            body: "body".into(),
            wake: false,
            notify: false,
            sent_at,
        }
    }

    fn sess(name: &str, last_seen: i64) -> Remembered {
        Remembered {
            name: name.into(),
            key: format!("k:{name}"),
            agent: "pi".into(),
            cwd: "/".into(),
            repo: None,
            branch: None,
            doing: None,
            tier: Tier::Pull,
            last_seen,
        }
    }

    fn version_of(db: &Connection) -> i64 {
        db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn fresh_db_lands_on_the_current_version() {
        let path = fresh("fresh");
        let store = Store::open(&path).unwrap();
        assert_eq!(version_of(&store.db), SCHEMA_VERSION);
        let tables: i64 = store
            .db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
                 AND name IN ('messages', 'outbox', 'kv', 'sessions')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 4);
        cleanup(&path);
    }

    #[test]
    fn adopts_a_version_zero_db_and_keeps_its_data() {
        let path = fresh("adopt");
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(SCHEMA).unwrap();
        raw.execute(
            "INSERT INTO messages (id, sender, recipient, body, wake, sent_at) VALUES ('m1', 's', 'me/x', 'hi', 0, 1)",
            [],
        )
        .unwrap();
        assert_eq!(version_of(&raw), 0);
        drop(raw);
        let store = Store::open(&path).unwrap();
        assert_eq!(version_of(&store.db), SCHEMA_VERSION);
        assert_eq!(store.unread_count("me/x").unwrap(), 1);
        cleanup(&path);
    }

    #[test]
    fn a_receipt_survives_the_ack() {
        let path = fresh("receipts");
        let store = Store::open(&path).unwrap();
        store.outbox_add("peer", &msg("m1", "them/bot", 1)).unwrap();
        store.outbox_settle("m1", "delivered", 9).unwrap();
        assert!(store.outbox_for("peer", 9).unwrap().is_empty(), "the acked row is gone from the outbox");
        let r = store.receipts(None, None, None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(
            (r[0].id.as_str(), r[0].to.as_str(), r[0].state.as_str(), r[0].at_ms),
            ("m1", "them/bot", "delivered", 9)
        );
        // Bounded by rows as well as by age: a busy machine sends more than a person ever reads back.
        store.outbox_add("peer", &msg("m1b", "them/bot", 2)).unwrap();
        store.outbox_settle("m1b", "delivered", 10).unwrap();
        assert_eq!(store.prune_receipts(10, 30 * 86_400_000, 1).unwrap(), 1);
        let r = store.receipts(None, None, None).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "m1b", "the oldest receipt is the one that goes");
        // Bounded by age: a month on, the ring forgets it like everything else.
        assert_eq!(store.prune_receipts(10 + 31 * 86_400_000, 30 * 86_400_000, 5_000).unwrap(), 1);
        assert!(store.receipts(None, None, None).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn giving_up_on_an_undelivered_message_is_recorded_not_absent() {
        let path = fresh("giveup");
        let store = Store::open(&path).unwrap();
        store.outbox_add("peer", &msg("m2", "them/bot", 1)).unwrap();
        assert!(store.outbox_for("peer", 1 + 8 * 86_400_000).unwrap().is_empty());
        let r = store.receipts(None, None, None).unwrap();
        assert_eq!((r.len(), r[0].id.as_str(), r[0].state.as_str()), (1, "m2", "abandoned"));
        cleanup(&path);
    }

    #[test]
    fn refuses_a_state_from_a_newer_build() {
        let path = fresh("newer");
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(SCHEMA).unwrap();
        raw.execute_batch(&format!("PRAGMA user_version = {}", SCHEMA_VERSION + 1)).unwrap();
        drop(raw);
        let err = Store::open(&path).err().expect("a newer schema must be refused");
        assert!(err.to_string().contains("newer"), "{err}");
        // Nothing was written: it arrived with SCHEMA_VERSION + 1, it still has it.
        let raw = Connection::open(&path).unwrap();
        assert_eq!(version_of(&raw), SCHEMA_VERSION + 1);
        cleanup(&path);
    }

    #[test]
    fn a_failing_step_rolls_back_whole() {
        let path = fresh("rollback");
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        assert_eq!(version_of(&db), 0);
        let res = apply_migrations(&mut db, 0, 1, &[(1, "CREATE TABLE half(x); INSERT INTO nope VALUES (1)")]);
        assert!(res.is_err());
        let half: i64 =
            db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'half'", [], |r| r.get(0)).unwrap();
        assert_eq!(half, 0);
        assert_eq!(version_of(&db), 0, "the version stamp rolled back with the DDL");
        cleanup(&path);
    }

    #[test]
    fn a_gap_in_the_steps_is_an_error() {
        let path = fresh("gap");
        let mut db = Connection::open(&path).unwrap();
        db.execute_batch(SCHEMA).unwrap();
        let err = apply_migrations(&mut db, 0, 2, &[]).expect_err("a gap must be refused");
        assert!(err.to_string().contains("no migration from schema v0"), "{err}");
        cleanup(&path);
    }

    #[test]
    fn retention_never_touches_unread_or_held_mail() {
        let path = fresh("retention");
        let store = Store::open(&path).unwrap();
        let now = 1_000_000_000_000i64;
        let month = 86_400_000 * 31;
        store.insert(&msg("read-old", "me/x", now - month), false).unwrap();
        store.insert(&msg("unread-old", "me/x", now - month), false).unwrap();
        store.insert(&msg("held-old", "me/x", now - month), true).unwrap();
        store.insert(&msg("read-recent", "me/x", now - 86_400_000), false).unwrap();
        store.mark_read(&["read-old".into(), "read-recent".into()], now).unwrap();

        assert_eq!(store.prune_messages(now, 86_400_000 * 30).unwrap(), 1);
        assert!(!store.has("read-old").unwrap());
        assert!(store.has("unread-old").unwrap(), "unread mail must never be pruned");
        assert!(store.has("held-old").unwrap(), "held mail must never be pruned");
        assert!(store.has("read-recent").unwrap());
        assert_eq!(store.prune_messages(now, 86_400_000 * 30).unwrap(), 0, "idempotent");
        cleanup(&path);
    }

    #[test]
    fn a_stale_session_goes_but_unread_mail_keeps_its_row() {
        let path = fresh("stale");
        let store = Store::open(&path).unwrap();
        let now = 1_000_000_000_000i64;
        let stale = now - 86_400_000 * 200;
        store.upsert_session(&sess("old", stale)).unwrap();
        store.upsert_session(&sess("quiet", stale)).unwrap();
        store.upsert_session(&sess("fresh", now)).unwrap();
        store.insert(&msg("m-old", "me/old", now - 86_400_000), false).unwrap(); // still unread

        assert_eq!(store.prune_sessions(now, 86_400_000 * 180, "me").unwrap(), 1);
        assert!(store.session_by_name("old").unwrap().is_some(), "unread mail needs its row to stay reachable");
        assert!(store.session_by_name("quiet").unwrap().is_none());
        assert!(store.session_by_name("fresh").unwrap().is_some());
        assert_eq!(store.unread_count("me/old").unwrap(), 1, "mail survives (09 §5 case 9)");
        cleanup(&path);
    }

    #[test]
    fn migrating_folds_the_wal_so_a_db_only_copy_is_whole() {
        let src = fresh("wal-src");
        let raw = Connection::open(&src).unwrap();
        raw.execute_batch(SCHEMA).unwrap();
        raw.execute(
            "INSERT INTO messages (id, sender, recipient, body, wake, sent_at) VALUES ('w1', 's', 'me/x', 'hi', 0, 1)",
            [],
        )
        .unwrap();
        // Copy db + WAL while the writer is still open — the state lives in the WAL, not the .db (12 §4).
        let copy = src.with_extension("copy.db");
        cleanup(&copy);
        std::fs::copy(&src, &copy).unwrap();
        std::fs::copy(format!("{}-wal", src.display()), format!("{}-wal", copy.display())).unwrap();

        // Opening through Store runs the adoption migration, which must checkpoint the WAL away.
        let store = Store::open(&copy).unwrap();
        let wal = std::fs::metadata(format!("{}-wal", copy.display())).unwrap();
        assert_eq!(wal.len(), 0, "migration must leave the WAL checkpointed");

        // A naive backup keeps only the .db file — it must still hold the data.
        drop(store);
        let _ = std::fs::remove_file(format!("{}-wal", copy.display()));
        let _ = std::fs::remove_file(format!("{}-shm", copy.display()));
        let backup = Connection::open(&copy).unwrap();
        let n: i64 = backup.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        drop(backup);
        cleanup(&src);
        cleanup(&copy);
    }
}
