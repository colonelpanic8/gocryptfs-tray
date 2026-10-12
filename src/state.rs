//! Settings changed from the tray menu, kept in `$XDG_STATE_HOME/gocryptfs-tray/state.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Per-vault auto-lock timeout in seconds; 0 disables it.
    #[serde(default)]
    pub auto_lock_seconds: BTreeMap<String, u64>,
    /// Minutes, as saved by earlier versions; folded into `auto_lock_seconds` on load.
    #[serde(default, skip_serializing)]
    auto_lock_minutes: BTreeMap<String, u64>,
}

impl State {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        let mut state: Self = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("ignoring unreadable {}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        };
        for (vault, minutes) in std::mem::take(&mut state.auto_lock_minutes) {
            state.auto_lock_seconds.entry(vault).or_insert(minutes * 60);
        }
        state
    }

    pub fn save(&self) -> Result<()> {
        let path = path().context("no state directory")?;
        let dir = path.parent().context("state path has no parent")?;
        std::fs::create_dir_all(dir)?;
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, toml::to_string(self)?)?;
        std::fs::rename(&temporary, &path)?;
        Ok(())
    }

    /// The menu's choice if one was made, else the configured default in minutes.
    pub fn auto_lock(&self, vault: &str, configured_minutes: Option<u64>) -> Option<Duration> {
        self.auto_lock_seconds
            .get(vault)
            .copied()
            .or(configured_minutes.map(|minutes| minutes * 60))
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs)
    }
}

fn path() -> Option<PathBuf> {
    dirs::state_dir().map(|dir| dir.join("gocryptfs-tray/state.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_choice_overrides_config_and_zero_disables() {
        let mut state = State::default();
        let minutes = |n: u64| Some(Duration::from_secs(n * 60));
        assert_eq!(state.auto_lock("v", Some(15)), minutes(15));
        state.auto_lock_seconds.insert("v".into(), 30);
        assert_eq!(
            state.auto_lock("v", Some(15)),
            Some(Duration::from_secs(30))
        );
        state.auto_lock_seconds.insert("v".into(), 0);
        assert_eq!(state.auto_lock("v", Some(15)), None);
        assert_eq!(state.auto_lock("other", None), None);
    }
}
