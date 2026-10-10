use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use gocryptfs_tray::config::Config;
use gocryptfs_tray::prompt::{check_length, terminal_new_pin, terminal_pin};
use gocryptfs_tray::tray;
use gocryptfs_tray::vault::{self, Vault};
use secrecy::SecretString;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file (default: $GOCRYPTFS_TRAY_CONFIG, ~/.config/gocryptfs-tray/config.toml,
    /// then /etc/gocryptfs-tray/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the system tray icon
    Tray,
    /// Show whether vaults are initialized and unlocked
    Status,
    /// Unlock a vault (prompts for the PIN)
    #[command(alias = "mount")]
    Unlock { vault: Option<String> },
    /// Lock a vault
    #[command(alias = "unmount")]
    Lock {
        vault: Option<String>,
        /// Lock even while programs are using the vault (they may lose access to open files)
        #[arg(long)]
        force: bool,
    },
    /// Change a vault's PIN (an empty current PIN means none is set yet)
    ChangePin { vault: Option<String> },
    /// Create a new, empty vault
    Init { vault: Option<String> },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Tray => tray::run(config),
        Command::Status => {
            for (name, vault_config) in config.visible_vaults() {
                let vault = Vault::new(name, vault_config);
                let initialized = match vault.is_initialized() {
                    Ok(true) => "initialized",
                    Ok(false) => "not initialized",
                    Err(_) => "unknown (no access)",
                };
                let state = if vault.is_mounted() {
                    "UNLOCKED"
                } else {
                    "locked"
                };
                println!(
                    "{name}: {state}, {initialized}, at {}",
                    vault.open_path().display()
                );
            }
            Ok(())
        }
        Command::Unlock { vault } => {
            let (name, vault_config) = config.select(vault.as_deref())?;
            let vault = Vault::new(name, vault_config);
            if vault.is_mounted() {
                println!("{name} is already unlocked");
                return Ok(());
            }
            let pin = pin_for(&vault, "PIN: ")?;
            vault.unlock(&pin).map_err(explain)?;
            println!("{name} unlocked at {}", vault.open_path().display());
            Ok(())
        }
        Command::Lock { vault, force } => {
            let (name, vault_config) = config.select(vault.as_deref())?;
            Vault::new(name, vault_config)
                .lock(force)
                .map_err(|error| {
                    if error.downcast_ref::<vault::InUse>().is_some() {
                        error.context("close those programs or pass --force")
                    } else {
                        error
                    }
                })?;
            println!("{name} locked");
            Ok(())
        }
        Command::ChangePin { vault } => {
            let (name, vault_config) = config.select(vault.as_deref())?;
            if !vault_config.pin {
                bail!("vault {name} does not use a PIN");
            }
            let vault = Vault::new(name, vault_config);
            let old = terminal_pin("Current PIN (empty if none): ")?;
            let new = terminal_new_pin(vault_config.min_pin_length)?;
            vault.change_pin(&old, &new).map_err(explain)?;
            println!("PIN changed");
            Ok(())
        }
        Command::Init { vault } => {
            let (name, vault_config) = config.select(vault.as_deref())?;
            let vault = Vault::new(name, vault_config);
            let pin = if vault_config.pin {
                terminal_new_pin(vault_config.min_pin_length)?
            } else {
                SecretString::from(String::new())
            };
            vault.init(&pin)?;
            println!("{name} initialized");
            Ok(())
        }
    }
}

fn pin_for(vault: &Vault, prompt: &str) -> Result<SecretString> {
    if !vault.config.pin {
        return Ok(SecretString::from(String::new()));
    }
    let pin = terminal_pin(prompt)?;
    check_length(&pin, 1)?;
    Ok(pin)
}

fn explain(error: anyhow::Error) -> anyhow::Error {
    if vault::is_wrong_password(&error) {
        anyhow::anyhow!("wrong PIN")
    } else {
        error
    }
}
