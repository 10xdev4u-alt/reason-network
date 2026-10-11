use crate::{paths, util};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Your name in the team, e.g. `alex`. Defaults to `$USER`.
    pub person: Option<String>,
    /// Show teammates full working directories (otherwise only the last component, plus repo@branch).
    pub share_paths: bool,
    /// Show teammates this machine's Claude Code sessions that don't have comms loaded.
    pub share_unmanaged: bool,
    /// Find teammates on the local network with mDNS.
    pub lan: bool,
    pub inbound: Inbound,
}

/// What happens to messages from teammates' agents: `accept` delivers them, `hold` waits for you to approve
/// (`comms held`), `refuse` bounces them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Inbound {
    pub default: Policy,
    pub people: BTreeMap<String, Policy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Accept,
    Hold,
    Refuse,
}

impl Default for Config {
    fn default() -> Self {
        Config { person: None, share_paths: false, share_unmanaged: false, lan: true, inbound: Inbound::default() }
    }
}

impl Default for Inbound {
    fn default() -> Self {
        Inbound { default: Policy::Accept, people: BTreeMap::new() }
    }
}

impl Config {
    pub fn load() -> Self {
        Self::load_strict().unwrap_or_else(|e| {
            eprintln!("comms: ignoring {} (fix or delete it): {e}", paths::config().display());
            Config::default()
        })
    }

    /// Like `load`, but a file that doesn't parse is an error (before rewriting it, say).
    pub fn load_strict() -> Result<Self> {
        match std::fs::read_to_string(paths::config()) {
            Ok(s) => Ok(toml::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_home()?;
        std::fs::write(paths::config(), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn person(&self) -> String {
        self.person
            .clone()
            .or_else(|| std::env::var("USER").ok())
            .map(|s| util::slug(&s))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "me".into())
    }

    pub fn policy_for(&self, person: &str) -> Policy {
        self.inbound.people.get(person).copied().unwrap_or(self.inbound.default)
    }

    pub fn lan(&self) -> bool {
        self.lan && std::env::var_os("COMMS_NO_MDNS").is_none()
    }
}
