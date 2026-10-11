//! Team membership: a small replicated state (team metadata, member machines, invites) in which every entry is
//! signed by a member machine's key. Any member can add or remove machines and issue invites; entries merge
//! last-writer-wins by timestamp. Built for a team of friends who trust each other, not adversarial members.

use crate::util;
use anyhow::{Result, anyhow, bail};
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use iroh::{EndpointAddr, EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signed<T> {
    pub body: T,
    pub by: EndpointId,
    pub sig: String,
}

/// What a signature covers: the team, the kind of entry, and the entry itself.
fn payload<T: Serialize>(team: &str, kind: &str, body: &T) -> Vec<u8> {
    let mut p = format!("company-comms/1/{team}/{kind}/").into_bytes();
    p.extend(serde_json::to_vec(body).expect("entries serialize"));
    p
}

impl<T: Serialize> Signed<T> {
    fn new(body: T, key: &SecretKey, team: &str, kind: &str) -> Self {
        let sig = key.sign(&payload(team, kind, &body));
        Signed { by: key.public(), sig: HEXLOWER.encode(&sig.to_bytes()), body }
    }

    fn verify(&self, team: &str, kind: &str) -> bool {
        let Ok(bytes) = HEXLOWER.decode(self.sig.as_bytes()) else { return false };
        let Ok(arr) = <[u8; Signature::LENGTH]>::try_from(bytes.as_slice()) else { return false };
        self.by.verify(&payload(team, kind, &self.body), &Signature::from_bytes(&arr)).is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    /// A self-hosted iroh relay for the team (`https://relay.example.com`); n0's public relays otherwise.
    pub relay: Option<String>,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub endpoint: EndpointId,
    pub person: String,
    pub machine: String,
    pub active: bool,
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    /// blake3 of the secret; the secret itself only travels in the ticket.
    pub hash: String,
    pub expires: i64,
    /// Set for `comms invite --me`: only this person can redeem it (adding a machine of their own).
    pub person: Option<String>,
    pub used_by: Option<EndpointId>,
    pub ts: i64,
}

/// The replicated team state. `admissions` records how each machine got in and is kept forever, so who may
/// sign entries never depends on the order changes arrive in. `members` holds each machine's latest entry
/// (active or removed), last writer wins. A removed machine can't connect or re-admit itself; it isn't made
/// cryptographically powerless (this is for friends, not adversaries).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamState {
    pub id: String,
    pub founder: EndpointId,
    pub meta: Signed<Meta>,
    #[serde(default)]
    pub admissions: BTreeMap<String, Signed<Member>>,
    pub members: BTreeMap<String, Signed<Member>>,
    pub invites: BTreeMap<String, Signed<Invite>>,
}

/// Entries stamped further ahead than this are refused (a broken clock, or worse).
const MAX_SKEW_MS: i64 = 86_400_000;

/// Last-writer-wins order; the signature breaks ties deterministically.
fn newer(ts: i64, sig: &str, than: Option<(i64, &str)>) -> bool {
    than.is_none_or(|(t, s)| (ts, sig) > (t, s))
}

fn next_ts(prev: i64) -> i64 {
    prev.max(util::now_ms() - 1).saturating_add(1)
}

fn plausible(ts: i64) -> bool {
    ts <= util::now_ms().saturating_add(MAX_SKEW_MS)
}

impl TeamState {
    pub fn found(name: &str, person: &str, machine: &str, key: &SecretKey) -> Self {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).expect("system randomness");
        let id = HEXLOWER.encode(&id);
        let now = util::now_ms();
        let meta = Signed::new(Meta { name: name.into(), relay: None, ts: now }, key, &id, "meta");
        let mut state = TeamState {
            id,
            founder: key.public(),
            meta,
            admissions: BTreeMap::new(),
            members: BTreeMap::new(),
            invites: BTreeMap::new(),
        };
        state.put_member(key, key.public(), person, machine, true);
        state
    }

    pub fn name(&self) -> &str {
        &self.meta.body.name
    }

    pub fn member(&self, id: &EndpointId) -> Option<&Member> {
        self.members.get(&id.to_string()).map(|m| &m.body)
    }

    /// Machines that were ever admitted: the founder, then anyone admitted by an admitted machine.
    fn admitted(&self) -> HashSet<EndpointId> {
        let mut set = HashSet::new();
        loop {
            let before = set.len();
            for a in self.admissions.values() {
                let e = a.body.endpoint;
                if !set.contains(&e) && (if a.by == e { e == self.founder } else { set.contains(&a.by) }) {
                    set.insert(e);
                }
            }
            if set.len() == before {
                return set;
            }
        }
    }

    pub fn is_active(&self, id: &EndpointId) -> bool {
        self.member(id).is_some_and(|m| m.active) && self.admitted().contains(id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Member> {
        let admitted = self.admitted();
        self.members.values().map(|m| &m.body).filter(move |m| m.active && admitted.contains(&m.endpoint))
    }

    pub fn people(&self) -> Vec<String> {
        let mut p: Vec<String> = self.active().map(|m| m.person.clone()).collect();
        p.sort();
        p.dedup();
        p
    }

    pub fn put_member(&mut self, key: &SecretKey, endpoint: EndpointId, person: &str, machine: &str, active: bool) {
        let ts = next_ts(self.member(&endpoint).map_or(0, |m| m.ts));
        let body = Member { endpoint, person: person.into(), machine: machine.into(), active, ts };
        let entry = Signed::new(body, key, &self.id, "member");
        if active && !self.admissions.contains_key(&endpoint.to_string()) {
            self.admissions.insert(endpoint.to_string(), entry.clone());
        }
        self.members.insert(endpoint.to_string(), entry);
    }

    pub fn set_relay(&mut self, key: &SecretKey, relay: Option<String>) {
        let body = Meta { name: self.meta.body.name.clone(), relay, ts: next_ts(self.meta.body.ts) };
        self.meta = Signed::new(body, key, &self.id, "meta");
    }

    /// Creates an invite and returns its secret.
    pub fn invite(&mut self, key: &SecretKey, hours: u64, person: Option<String>) -> String {
        let mut secret = [0u8; 16];
        getrandom::fill(&mut secret).expect("system randomness");
        let secret = HEXLOWER.encode(&secret);
        let now = util::now_ms();
        let body = Invite { hash: hash(&secret), expires: now + hours as i64 * 3_600_000, person, used_by: None, ts: now };
        self.invites.insert(body.hash.clone(), Signed::new(body, key, &self.id, "invite"));
        secret
    }

    /// Checks that `secret` admits `person`, and marks the invite used by `joiner`.
    pub fn redeem(&mut self, key: &SecretKey, secret: &str, person: &str, joiner: EndpointId) -> Result<()> {
        let h = hash(secret);
        let signed = self.invites.get(&h).ok_or_else(|| anyhow!("unknown invite"))?;
        let (inv, signer) = (signed.body.clone(), signed.by);
        if !self.is_active(&signer) {
            bail!("this invite was made by a machine that has left the team; ask for a new one");
        }
        match inv.used_by {
            // A retry by a machine that got in with it (its first reply was lost) is fine; anyone else isn't.
            Some(u) if u == joiner && self.is_active(&joiner) => return Ok(()),
            Some(_) => bail!("this invite was already used; ask for a new one"),
            None => {}
        }
        if inv.expires < util::now_ms() {
            bail!("this invite expired; ask for a new one");
        }
        match &inv.person {
            Some(p) if p != person => bail!("this invite is for {p}'s own machines"),
            Some(_) => {}
            None if self.active().any(|m| m.person == person && m.endpoint != joiner) => {
                bail!("'{person}' is already in this team; join with another name (--as)")
            }
            None => {}
        }
        let used = Invite { used_by: Some(joiner), ts: next_ts(inv.ts), ..inv };
        self.invites.insert(h, Signed::new(used, key, &self.id, "invite"));
        Ok(())
    }

    /// Merges another copy of the state; true if anything changed.
    pub fn merge(&mut self, other: &TeamState) -> Result<bool> {
        if other.id != self.id || other.founder != self.founder {
            bail!("state from a different team");
        }
        let mut changed = false;
        // Admissions first: they decide who may sign everything else. One admission can vouch for the signer of
        // another, so repeat until nothing new lands.
        loop {
            let admitted = self.admitted();
            let mut added = false;
            for (k, a) in &other.admissions {
                if self.admissions.contains_key(k) || *k != a.body.endpoint.to_string() || !a.body.active || !plausible(a.body.ts) {
                    continue;
                }
                let vouched = if a.by == a.body.endpoint { a.by == self.founder } else { admitted.contains(&a.by) };
                if vouched && a.verify(&self.id, "member") {
                    self.admissions.insert(k.clone(), a.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
            changed = true;
        }
        let admitted = self.admitted();
        for (k, e) in &other.members {
            let cur = self.members.get(k);
            if *k != e.body.endpoint.to_string()
                || !plausible(e.body.ts)
                || !newer(e.body.ts, &e.sig, cur.map(|c| (c.body.ts, c.sig.as_str())))
            {
                continue;
            }
            // A machine may sign its own departure, never its own return (the founder's very first entry aside).
            let self_admit = e.by == e.body.endpoint && e.body.active;
            if self_admit && !(e.by == self.founder && cur.is_none()) {
                continue;
            }
            if admitted.contains(&e.by) && e.verify(&self.id, "member") {
                self.members.insert(k.clone(), e.clone());
                changed = true;
            }
        }
        let m = &other.meta;
        if newer(m.body.ts, &m.sig, Some((self.meta.body.ts, &self.meta.sig)))
            && plausible(m.body.ts)
            && admitted.contains(&m.by)
            && m.verify(&self.id, "meta")
        {
            self.meta = m.clone();
            changed = true;
        }
        for (h, inv) in &other.invites {
            let cur = self.invites.get(h);
            if &inv.body.hash == h
                && plausible(inv.body.ts)
                && newer(inv.body.ts, &inv.sig, cur.map(|c| (c.body.ts, c.sig.as_str())))
                && admitted.contains(&inv.by)
                && inv.verify(&self.id, "invite")
            {
                self.invites.insert(h.clone(), inv.clone());
                changed = true;
            }
        }
        Ok(changed)
    }

    /// Validates a state received from a stranger (the join reply): every entry must trace back to the founder.
    pub fn verified(other: &TeamState) -> Result<TeamState> {
        let mut s = TeamState {
            id: other.id.clone(),
            founder: other.founder,
            meta: other.meta.clone(),
            admissions: BTreeMap::new(),
            members: BTreeMap::new(),
            invites: BTreeMap::new(),
        };
        s.merge(other)?;
        let admitted = s.admitted();
        if !admitted.contains(&s.founder) {
            bail!("the team's founding entry does not verify");
        }
        if !admitted.contains(&s.meta.by) || !s.meta.verify(&s.id, "meta") {
            bail!("team metadata does not verify");
        }
        Ok(s)
    }

    /// Drops invites that expired more than a week ago.
    pub fn prune_invites(&mut self) {
        let cutoff = util::now_ms() - 7 * 86_400_000;
        self.invites.retain(|_, i| i.body.expires > cutoff);
    }
}

pub fn hash(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

/// Everything a new machine needs to join: printed by `comms invite`, pasted into `comms join`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub team: String,
    pub name: String,
    pub secret: String,
    pub relay: Option<String>,
    pub peers: Vec<EndpointAddr>,
}

const TICKET_PREFIX: &str = "comms1";

impl Ticket {
    pub fn encode(&self) -> String {
        format!("{TICKET_PREFIX}{}", BASE64URL_NOPAD.encode(&serde_json::to_vec(self).expect("ticket serializes")))
    }

    pub fn decode(s: &str) -> Result<Self> {
        let s = s.trim();
        let data = s.strip_prefix(TICKET_PREFIX).ok_or_else(|| anyhow!("not a comms invite (should start with {TICKET_PREFIX})"))?;
        let bytes = BASE64URL_NOPAD.decode(data.as_bytes()).map_err(|_| anyhow!("invite is damaged (was it cut off?)"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::generate()
    }

    #[test]
    fn found_invite_join_merge() {
        let (a, b, c) = (key(), key(), key());
        let mut sa = TeamState::found("vibes", "tal", "mbp", &a);
        let secret = sa.invite(&a, 24, None);

        // b joins through a: a redeems and adds b; b verifies what it receives.
        sa.redeem(&a, &secret, "alice", b.public()).unwrap();
        sa.put_member(&a, b.public(), "alice", "xps", true);
        let mut sb = TeamState::verified(&sa).unwrap();
        assert_eq!(sb, sa);
        assert!(sb.is_active(&b.public()));
        assert!(sa.redeem(&a, &secret, "eve", c.public()).is_err(), "invites are single use");

        // b invites c; a learns about c by merging b's copy.
        let s2 = sb.invite(&b, 1, None);
        sb.redeem(&b, &s2, "sam", c.public()).unwrap();
        sb.put_member(&b, c.public(), "sam", "nuc", true);
        assert!(sa.merge(&sb).unwrap());
        assert!(sa.is_active(&c.public()));
        assert!(!sa.merge(&sb).unwrap(), "merging again changes nothing");
        assert_eq!(sa.people(), vec!["alice", "sam", "tal"]);
    }

    #[test]
    fn rejects_forgery_and_self_readd() {
        let (a, b, mallory) = (key(), key(), key());
        let mut sa = TeamState::found("vibes", "tal", "mbp", &a);
        sa.put_member(&a, b.public(), "alice", "xps", true);

        // A stranger cannot add itself.
        let mut forged = sa.clone();
        forged.put_member(&mallory, mallory.public(), "mallory", "evil", true);
        assert!(!sa.merge(&forged).unwrap());
        assert!(!sa.is_active(&mallory.public()));

        // A tampered entry fails verification.
        let mut tampered = sa.clone();
        let e = tampered.members.get_mut(&b.public().to_string()).unwrap();
        e.body.person = "root".into();
        e.body.ts += 10;
        assert!(!sa.merge(&tampered).unwrap());

        // b may leave on its own, but not come back on its own.
        let mut leaving = sa.clone();
        leaving.put_member(&b, b.public(), "alice", "xps", false);
        assert!(sa.merge(&leaving).unwrap());
        assert!(!sa.is_active(&b.public()));
        sa.put_member(&a, b.public(), "alice", "xps", true);

        // Once removed, b cannot sign itself back in.
        sa.put_member(&a, b.public(), "alice", "xps", false);
        let mut sb = sa.clone();
        sb.put_member(&b, b.public(), "alice", "xps", true);
        assert!(!sa.merge(&sb).unwrap());
        assert!(!sa.is_active(&b.public()));
    }

    #[test]
    fn founder_leaving_doesnt_lock_the_team() {
        let (f, b, d) = (key(), key(), key());
        let mut s = TeamState::found("vibes", "tal", "mbp", &f);
        s.put_member(&f, b.public(), "alice", "xps", true);
        s.put_member(&f, f.public(), "tal", "mbp", false); // founder leaves
        let secret = s.invite(&b, 24, None);
        s.redeem(&b, &secret, "sam", d.public()).unwrap();
        s.put_member(&b, d.public(), "sam", "nuc", true);
        let sd = TeamState::verified(&s).unwrap();
        assert!(sd.is_active(&d.public()) && sd.is_active(&b.public()) && !sd.is_active(&f.public()));
    }

    #[test]
    fn merges_converge_whatever_the_order() {
        let (f, b, c, d) = (key(), key(), key(), key());
        let mut base = TeamState::found("vibes", "tal", "mbp", &f);
        base.put_member(&f, b.public(), "alice", "xps", true);
        // Concurrently: f removes b while b adds c.
        let mut at_f = base.clone();
        at_f.put_member(&f, b.public(), "alice", "xps", false);
        let mut at_b = base.clone();
        at_b.put_member(&b, c.public(), "sam", "nuc", true);
        let mut x = at_f.clone();
        x.merge(&at_b).unwrap();
        let mut y = at_b.clone();
        y.merge(&at_f).unwrap();
        assert_eq!(x, y);
        assert!(x.is_active(&c.public()) && !x.is_active(&b.public()));
        // A newcomer sees the same, including c (admitted by b, who is gone now).
        let secret = x.invite(&f, 24, None);
        x.redeem(&f, &secret, "dee", d.public()).unwrap();
        x.put_member(&f, d.public(), "dee", "pi", true);
        let z = TeamState::verified(&x).unwrap();
        assert!(z.is_active(&c.public()) && !z.is_active(&b.public()));
    }

    #[test]
    fn invites_die_with_their_maker_and_cant_readmit() {
        let (a, b, c) = (key(), key(), key());
        let mut s = TeamState::found("vibes", "tal", "mbp", &a);
        let first = s.invite(&a, 24, None);
        s.redeem(&a, &first, "alice", b.public()).unwrap();
        s.put_member(&a, b.public(), "alice", "xps", true);
        assert!(s.redeem(&a, &first, "alice", b.public()).is_ok(), "a retry by the same machine is fine");
        let from_b = s.invite(&b, 720, None);
        s.put_member(&a, b.public(), "alice", "xps", false);
        assert!(s.redeem(&a, &first, "alice", b.public()).is_err(), "removed: the old invite doesn't readmit");
        assert!(s.redeem(&a, &from_b, "sam", c.public()).is_err(), "invites by a removed machine are dead");
    }

    #[test]
    fn refuses_far_future_entries() {
        let (a, b) = (key(), key());
        let mut s = TeamState::found("vibes", "tal", "mbp", &a);
        let mut evil = s.clone();
        let body = Member { endpoint: b.public(), person: "x".into(), machine: "y".into(), active: true, ts: i64::MAX };
        let e = Signed::new(body, &a, &evil.id, "member");
        evil.admissions.insert(b.public().to_string(), e.clone());
        evil.members.insert(b.public().to_string(), e);
        assert!(!s.merge(&evil).unwrap());
        s.put_member(&a, b.public(), "x", "y", true); // and no overflow on the next change
    }

    #[test]
    fn names_are_unique_unless_invited_for_self() {
        let (a, b) = (key(), key());
        let mut s = TeamState::found("vibes", "tal", "mbp", &a);
        let open = s.invite(&a, 24, None);
        assert!(s.redeem(&a, &open, "tal", b.public()).is_err());
        let mine = s.invite(&a, 24, Some("tal".into()));
        assert!(s.redeem(&a, &mine, "alice", b.public()).is_err());
        s.redeem(&a, &mine, "tal", b.public()).unwrap();
    }

    #[test]
    fn ticket_roundtrip() {
        let a = key();
        let t = Ticket {
            team: "abc".into(),
            name: "vibes".into(),
            secret: "s3cret".into(),
            relay: None,
            peers: vec![EndpointAddr::new(a.public())],
        };
        let back = Ticket::decode(&format!("  {}\n", t.encode())).unwrap();
        assert_eq!(back.peers[0].id, a.public());
        assert!(Ticket::decode("comms1!!!").is_err());
        assert!(Ticket::decode("hello").is_err());
    }
}
