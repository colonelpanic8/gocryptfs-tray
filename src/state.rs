//! Settings changed from the tray menu, kept in `$XDG_STATE_HOME/gocryptfs-tray/state.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Per-vault auto-lock timeout in minutes; 0 disables it.
    #[serde(default)]
    pub auto_lock_minutes: BTreeMap<String, u64>,
}

impl State {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|error| {
                tracing::warn!("ignoring unreadable {}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
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

    /// The menu's choice if one was made, else the configured default.
    pub fn auto_lock_minutes(&self, vault: &str, configured: Option<u64>) -> Option<u64> {
        self.auto_lock_minutes
            .get(vault)
            .copied()
            .or(configured)
            .filter(|minutes| *minutes > 0)
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
        assert_eq!(state.auto_lock_minutes("v", Some(15)), Some(15));
        state.auto_lock_minutes.insert("v".into(), 5);
        assert_eq!(state.auto_lock_minutes("v", Some(15)), Some(5));
        state.auto_lock_minutes.insert("v".into(), 0);
        assert_eq!(state.auto_lock_minutes("v", Some(15)), None);
        assert_eq!(state.auto_lock_minutes("other", None), None);
    }
}
