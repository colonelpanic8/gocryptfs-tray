use std::sync::mpsc::{self, Sender};

use anyhow::Result;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, StandardItem};
use secrecy::SecretString;

use crate::config::Config;
use crate::icons::{self, LockState};
use crate::mounts;
use crate::prompt::Pinentry;
use crate::vault::{self, Vault};

pub const TRAY_ID: &str = "gocryptfs-tray";

enum Action {
    Refresh,
    Unlock(String),
    Lock(String),
    Open(String),
    ChangePin(String),
}

struct VaultStatus {
    name: String,
    mounted: bool,
    busy: bool,
}

struct Tray {
    vaults: Vec<VaultStatus>,
    actions: Sender<Action>,
}

impl Tray {
    fn any_unlocked(&self) -> bool {
        self.vaults.iter().any(|vault| vault.mounted)
    }

    fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }

    fn vault_items(&self, vault: &VaultStatus, prefix: &str) -> Vec<MenuItem<Self>> {
        let name = vault.name.clone();
        let state = match (vault.busy, vault.mounted) {
            (true, _) => "working…",
            (false, true) => "UNLOCKED",
            (false, false) => "locked",
        };
        let mut items: Vec<MenuItem<Self>> = vec![
            StandardItem {
                label: format!("{}: {state}", vault.name),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];
        let toggle: MenuItem<Self> = if vault.mounted {
            let name = name.clone();
            StandardItem {
                label: format!("{prefix}Lock"),
                icon_name: "changes-prevent-symbolic".into(),
                enabled: !vault.busy,
                activate: Box::new(move |tray: &mut Self| tray.send(Action::Lock(name.clone()))),
                ..Default::default()
            }
            .into()
        } else {
            let name = name.clone();
            StandardItem {
                label: format!("{prefix}Unlock…"),
                icon_name: "changes-allow-symbolic".into(),
                enabled: !vault.busy,
                activate: Box::new(move |tray: &mut Self| tray.send(Action::Unlock(name.clone()))),
                ..Default::default()
            }
            .into()
        };
        items.push(toggle);
        let open_name = name.clone();
        items.push(
            StandardItem {
                label: format!("{prefix}Open folder"),
                icon_name: "folder-open-symbolic".into(),
                enabled: vault.mounted,
                activate: Box::new(move |tray: &mut Self| {
                    tray.send(Action::Open(open_name.clone()))
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: format!("{prefix}Change PIN…"),
                enabled: !vault.busy,
                activate: Box::new(move |tray: &mut Self| {
                    tray.send(Action::ChangePin(name.clone()))
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        TRAY_ID.into()
    }

    fn title(&self) -> String {
        "gocryptfs vaults".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn status(&self) -> ksni::Status {
        if self.any_unlocked() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icons::icon(if self.any_unlocked() {
            LockState::Unlocked
        } else {
            LockState::Locked
        })
    }

    fn attention_icon_pixmap(&self) -> Vec<ksni::Icon> {
        icons::icon(LockState::Unlocked)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let unlocked: Vec<&str> = self
            .vaults
            .iter()
            .filter(|vault| vault.mounted)
            .map(|vault| vault.name.as_str())
            .collect();
        ksni::ToolTip {
            title: if unlocked.is_empty() {
                "Vaults locked".into()
            } else {
                format!("UNLOCKED: {}", unlocked.join(", "))
            },
            ..Default::default()
        }
    }

    /// Left click unlocks, or opens an already unlocked vault; locking is menu-only.
    fn activate(&mut self, _x: i32, _y: i32) {
        if let Some(vault) = self.vaults.iter().find(|vault| vault.mounted) {
            self.send(Action::Open(vault.name.clone()));
        } else if let [vault] = self.vaults.as_slice() {
            self.send(Action::Unlock(vault.name.clone()));
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let several = self.vaults.len() > 1;
        let mut items = Vec::new();
        for vault in &self.vaults {
            if !items.is_empty() {
                items.push(MenuItem::Separator);
            }
            let prefix = if several { "    " } else { "" };
            items.extend(self.vault_items(vault, prefix));
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit tray".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

pub fn run(config: Config) -> Result<()> {
    let names: Vec<String> = config
        .visible_vaults()
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    anyhow::ensure!(!names.is_empty(), "no vaults configured for this user");

    let (actions, queue) = mpsc::channel();
    let tray = Tray {
        vaults: names
            .iter()
            .map(|name| VaultStatus {
                name: name.clone(),
                mounted: false,
                busy: false,
            })
            .collect(),
        actions: actions.clone(),
    };
    let handle = tray.spawn()?;

    let watcher = actions.clone();
    std::thread::spawn(move || {
        if let Err(error) = mounts::watch(|| {
            let _ = watcher.send(Action::Refresh);
        }) {
            tracing::error!("watching mounts failed: {error:#}");
        }
    });
    actions.send(Action::Refresh)?;

    let pinentry = Pinentry::new(config.pinentry.clone());
    let set_busy = |name: &str, busy: bool| {
        handle.update(|tray: &mut Tray| {
            for vault in &mut tray.vaults {
                if vault.name == name {
                    vault.busy = busy;
                }
            }
        });
    };

    for action in queue {
        match action {
            Action::Refresh => {}
            Action::Open(name) => {
                let (name, vault_config) = config.select(Some(&name))?;
                let vault = Vault::new(name, vault_config);
                if let Err(error) = std::process::Command::new("xdg-open")
                    .arg(vault.open_path())
                    .spawn()
                {
                    pinentry.show_error(&format!("Could not open the folder: {error}"));
                }
            }
            Action::Unlock(name) => {
                let (name, vault_config) = config.select(Some(&name))?;
                set_busy(name, true);
                unlock(&pinentry, &Vault::new(name, vault_config));
                set_busy(name, false);
            }
            Action::Lock(name) => {
                let (name, vault_config) = config.select(Some(&name))?;
                set_busy(name, true);
                if let Err(error) = Vault::new(name, vault_config).lock() {
                    pinentry.show_error(&format!("Locking {name} failed: {error:#}"));
                }
                set_busy(name, false);
            }
            Action::ChangePin(name) => {
                let (name, vault_config) = config.select(Some(&name))?;
                set_busy(name, true);
                change_pin(&pinentry, &Vault::new(name, vault_config));
                set_busy(name, false);
            }
        }
        refresh(&handle, &config);
    }
    Ok(())
}

fn refresh(handle: &ksni::blocking::Handle<Tray>, config: &Config) {
    handle.update(|tray: &mut Tray| {
        for status in &mut tray.vaults {
            if let Ok((name, vault_config)) = config.select(Some(&status.name)) {
                status.mounted = Vault::new(name, vault_config).is_mounted();
            }
        }
    });
}

fn unlock(pinentry: &Pinentry, vault: &Vault) {
    let description = format!("Unlock the {} vault", vault.name);
    let mut error: Option<String> = None;
    loop {
        let pin = if vault.config.pin {
            match pinentry.ask(&description, "PIN:", error.as_deref(), false) {
                Ok(Some(pin)) => pin,
                Ok(None) => return,
                Err(error) => {
                    pinentry.show_error(&format!("{error:#}"));
                    return;
                }
            }
        } else {
            SecretString::from(String::new())
        };
        match vault.unlock(&pin) {
            Ok(()) => return,
            Err(failure) if vault::is_wrong_password(&failure) && vault.config.pin => {
                error = Some("Wrong PIN".into());
            }
            Err(failure) => {
                pinentry.show_error(&format!("Unlocking {} failed: {failure:#}", vault.name));
                return;
            }
        }
    }
}

fn change_pin(pinentry: &Pinentry, vault: &Vault) {
    let description = format!("Change the PIN of the {} vault", vault.name);
    let Ok(Some(old)) = pinentry.ask(&description, "Current PIN:", None, false) else {
        return;
    };
    let mut error: Option<String> = None;
    let new = loop {
        match pinentry.ask(&description, "New PIN:", error.as_deref(), true) {
            Ok(Some(pin)) => match crate::prompt::check_length(&pin, vault.config.min_pin_length) {
                Ok(()) => break pin,
                Err(failure) => error = Some(failure.to_string()),
            },
            _ => return,
        }
    };
    if let Err(failure) = vault.change_pin(&old, &new) {
        let message = if vault::is_wrong_password(&failure) {
            "The current PIN was wrong; the PIN is unchanged.".to_owned()
        } else {
            format!("Changing the PIN failed: {failure:#}")
        };
        pinentry.show_error(&message);
    }
}
