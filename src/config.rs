use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const SYSTEM_CONFIG: &str = "/etc/gocryptfs-tray/config.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// pinentry program used by the tray; defaults to `pinentry` on PATH.
    #[serde(default)]
    pub pinentry: Option<String>,
    pub vaults: BTreeMap<String, VaultConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// Encrypted gocryptfs directory.
    pub cipher_dir: PathBuf,
    /// Where the decrypted view is mounted.
    pub mount_dir: PathBuf,
    /// User-facing path to open, e.g. a symlink to `mount_dir`.
    #[serde(default)]
    pub link: Option<PathBuf>,
    /// Only show this vault to this user.
    #[serde(default)]
    pub owner: Option<String>,
    /// Command whose first line of output is prepended to the PIN.
    #[serde(default)]
    pub secret_command: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub pin: bool,
    #[serde(default = "default_min_pin_length")]
    pub min_pin_length: usize,
    /// Lock after this many idle minutes while the tray runs; the tray menu can override it.
    #[serde(default)]
    pub auto_lock_minutes: Option<u64>,
    pub backend: Backend,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Backend {
    /// Run gocryptfs directly as the invoking user.
    Direct {
        #[serde(default)]
        gocryptfs_args: Vec<String>,
    },
    /// Delegate to system units, which read the password from `handoff_dir`.
    Systemd {
        mount_unit: String,
        init_unit: String,
        passwd_unit: String,
        handoff_dir: PathBuf,
    },
}

fn default_true() -> bool {
    true
}

fn default_min_pin_length() -> usize {
    4
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let mut config: Config = toml::from_str(text)?;
        for (name, vault) in &mut config.vaults {
            if vault.secret_command.is_none() && !vault.pin {
                bail!("vault {name}: needs a secret_command, a PIN, or both");
            }
            vault.cipher_dir = expand_home(&vault.cipher_dir);
            vault.mount_dir = expand_home(&vault.mount_dir);
            vault.link = vault.link.as_deref().map(expand_home);
        }
        Ok(config)
    }

    /// `--config`, then `$GOCRYPTFS_TRAY_CONFIG`, then the user config, then the system config.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => default_path()?,
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    /// Vaults visible to the current user.
    pub fn visible_vaults(&self) -> Vec<(&str, &VaultConfig)> {
        let user = current_user();
        self.vaults
            .iter()
            .filter(|(_, vault)| match (&vault.owner, &user) {
                (Some(owner), Some(user)) => owner == user,
                _ => true,
            })
            .map(|(name, vault)| (name.as_str(), vault))
            .collect()
    }

    pub fn select(&self, name: Option<&str>) -> Result<(&str, &VaultConfig)> {
        let vaults = self.visible_vaults();
        match name {
            Some(name) => vaults
                .into_iter()
                .find(|(candidate, _)| *candidate == name)
                .with_context(|| format!("no vault named {name}")),
            None => match vaults.as_slice() {
                [only] => Ok(*only),
                [] => bail!("no vaults configured"),
                _ => bail!("several vaults configured; name one"),
            },
        }
    }
}

fn default_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GOCRYPTFS_TRAY_CONFIG") {
        return Ok(path.into());
    }
    if let Some(dir) = dirs::config_dir() {
        let path = dir.join("gocryptfs-tray/config.toml");
        if path.exists() {
            return Ok(path);
        }
    }
    let system = PathBuf::from(SYSTEM_CONFIG);
    if system.exists() {
        return Ok(system);
    }
    bail!("no config found; create ~/.config/gocryptfs-tray/config.toml or {SYSTEM_CONFIG}")
}

fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

fn current_user() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage or null.
    unsafe {
        let passwd = libc::getpwuid(libc::getuid());
        if passwd.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*passwd).pw_name)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_backends() {
        let config = Config::parse(
            r#"
            [vaults.private]
            cipher_dir = "/srv/sync/Private.encrypted"
            mount_dir = "/run/gocryptfs-tray/private/mnt"
            link = "~/Private"
            secret_command = ["pass", "show", "vault"]
            backend = { type = "systemd", mount_unit = "m.service", init_unit = "i.service", passwd_unit = "p.service", handoff_dir = "/run/gocryptfs-tray/private" }

            [vaults.scratch]
            cipher_dir = "~/scratch.encrypted"
            mount_dir = "~/scratch"
            backend = { type = "direct" }
            "#,
        )
        .unwrap();

        let private = &config.vaults["private"];
        assert!(private.pin);
        assert!(matches!(private.backend, Backend::Systemd { .. }));
        assert!(!private.link.as_ref().unwrap().starts_with("~"));

        let scratch = &config.vaults["scratch"];
        assert!(!scratch.cipher_dir.starts_with("~"));
        assert!(matches!(scratch.backend, Backend::Direct { .. }));
    }

    #[test]
    fn rejects_vault_without_any_secret() {
        let error = Config::parse(
            r#"
            [vaults.v]
            cipher_dir = "/c"
            mount_dir = "/m"
            pin = false
            backend = { type = "direct" }
            "#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("secret_command"));
    }
}
