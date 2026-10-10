use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use ksni::blocking::TrayMethods;
use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use secrecy::SecretString;

use crate::activity::{self, LastActivity};
use crate::config::Config;
use crate::icons::{self, LockState};
use crate::mounts;
use crate::prompt::Pinentry;
use crate::state::State;
use crate::vault::{self, Vault};

pub const TRAY_ID: &str = "gocryptfs-tray";

const TICK: Duration = Duration::from_secs(30);
const AUTO_LOCK_CHOICES: [u64; 6] = [5, 15, 30, 60, 120, 240];

enum Action {
    Refresh,
    Tick,
    Unlock(String),
    Lock(String),
    Open(String),
    ChangePin(String),
    SetAutoLock(String, Option<u64>),
}

struct VaultStatus {
    name: String,
    mounted: bool,
    busy: bool,
    auto_lock_minutes: Option<u64>,
    /// Minutes left before auto-lock, while unlocked.
    locks_in: Option<u64>,
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
        let state = match (vault.busy, vault.mounted, vault.locks_in) {
            (true, _, _) => "working…".to_owned(),
            (false, true, Some(minutes)) => format!("UNLOCKED, locks in {minutes} min"),
            (false, true, None) => "UNLOCKED".to_owned(),
            (false, false, _) => "locked".to_owned(),
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
        items.push(auto_lock_menu(vault, prefix));
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

fn auto_lock_menu(vault: &VaultStatus, prefix: &str) -> MenuItem<Tray> {
    // Off, the standard choices, and a configured value that is not among them.
    let mut choices: Vec<Option<u64>> = vec![None];
    choices.extend(AUTO_LOCK_CHOICES.iter().map(|minutes| Some(*minutes)));
    if !choices.contains(&vault.auto_lock_minutes) {
        choices.push(vault.auto_lock_minutes);
        choices[1..].sort();
    }
    let selected = choices
        .iter()
        .position(|choice| *choice == vault.auto_lock_minutes)
        .unwrap_or(0);
    let options = choices
        .iter()
        .map(|choice| RadioItem {
            label: match choice {
                None => "Off".into(),
                Some(minutes) => describe_minutes(*minutes),
            },
            ..Default::default()
        })
        .collect();
    let name = vault.name.clone();
    SubMenu {
        label: format!("{prefix}Auto-lock when idle"),
        submenu: vec![
            RadioGroup {
                selected,
                select: Box::new(move |tray: &mut Tray, index| {
                    tray.send(Action::SetAutoLock(name.clone(), choices[index]));
                }),
                options,
            }
            .into(),
        ],
        ..Default::default()
    }
    .into()
}

fn describe_minutes(minutes: u64) -> String {
    match minutes {
        60 => "1 hour".into(),
        minutes if minutes % 60 == 0 => format!("{} hours", minutes / 60),
        minutes => format!("{minutes} minutes"),
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
        let unlocked: Vec<String> = self
            .vaults
            .iter()
            .filter(|vault| vault.mounted)
            .map(|vault| match vault.locks_in {
                Some(minutes) => format!("{} (locks in {minutes} min)", vault.name),
                None => vault.name.clone(),
            })
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

/// The worker's view of each vault: config, auto-lock setting and activity.
struct Worker {
    config: Config,
    state: State,
    pinentry: Pinentry,
    activity: HashMap<String, LastActivity>,
    handle: ksni::blocking::Handle<Tray>,
}

pub fn run(config: Config) -> Result<()> {
    let names: Vec<String> = config
        .visible_vaults()
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    anyhow::ensure!(!names.is_empty(), "no vaults configured for this user");

    let state = State::load();
    let (actions, queue) = mpsc::channel();
    let tray = Tray {
        vaults: names
            .iter()
            .map(|name| VaultStatus {
                name: name.clone(),
                mounted: false,
                busy: false,
                auto_lock_minutes: state
                    .auto_lock_minutes(name, config.vaults[name].auto_lock_minutes),
                locks_in: None,
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
    let ticker = actions.clone();
    std::thread::spawn(move || {
        while ticker.send(Action::Tick).is_ok() {
            std::thread::sleep(TICK);
        }
    });

    let mut worker = Worker {
        pinentry: Pinentry::new(config.pinentry.clone()),
        config,
        state,
        activity: HashMap::new(),
        handle,
    };
    for action in queue {
        worker.handle_action(action);
        worker.refresh();
    }
    Ok(())
}

impl Worker {
    fn vault(&self, name: &str) -> Option<Vault<'_>> {
        self.config
            .select(Some(name))
            .ok()
            .map(|(name, config)| Vault::new(name, config))
    }

    fn auto_lock_minutes(&self, name: &str) -> Option<u64> {
        let configured = self.config.vaults.get(name)?.auto_lock_minutes;
        self.state.auto_lock_minutes(name, configured)
    }

    fn set_busy(&self, name: &str, busy: bool) {
        self.handle.update(|tray: &mut Tray| {
            for vault in &mut tray.vaults {
                if vault.name == name {
                    vault.busy = busy;
                }
            }
        });
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Refresh => {}
            Action::Tick => self.auto_lock(),
            Action::Open(name) => {
                let Some(vault) = self.vault(&name) else {
                    return;
                };
                open_folder(self.pinentry.clone(), vault.open_path().to_path_buf());
            }
            Action::Unlock(name) => {
                let Some(vault) = self.vault(&name) else {
                    return;
                };
                self.set_busy(&name, true);
                unlock(&self.pinentry, &vault);
                self.set_busy(&name, false);
            }
            Action::Lock(name) => {
                let Some(vault) = self.vault(&name) else {
                    return;
                };
                self.set_busy(&name, true);
                lock(&self.pinentry, &vault);
                self.set_busy(&name, false);
            }
            Action::ChangePin(name) => {
                let Some(vault) = self.vault(&name) else {
                    return;
                };
                self.set_busy(&name, true);
                change_pin(&self.pinentry, &vault);
                self.set_busy(&name, false);
            }
            Action::SetAutoLock(name, minutes) => {
                self.state
                    .auto_lock_minutes
                    .insert(name.clone(), minutes.unwrap_or(0));
                if let Err(error) = self.state.save() {
                    tracing::warn!("saving the auto-lock setting failed: {error:#}");
                }
                // Count the change as activity so a shorter timeout does not lock at once.
                if let Some(last) = self.activity.get(&name) {
                    *last.lock().unwrap() = Instant::now();
                }
            }
        }
    }

    /// Locks vaults that have been idle for longer than their timeout.
    fn auto_lock(&mut self) {
        let names: Vec<String> = self.activity.keys().cloned().collect();
        for name in names {
            let Some(timeout) = self.auto_lock_minutes(&name) else {
                continue;
            };
            let Some(vault) = self.vault(&name) else {
                continue;
            };
            let last = self.activity[&name].clone();
            if activity::in_use(&vault.config.mount_dir) {
                *last.lock().unwrap() = Instant::now();
                continue;
            }
            let idle = last.lock().unwrap().elapsed();
            if idle < Duration::from_secs(timeout * 60) {
                continue;
            }
            tracing::info!(
                "auto-locking {name} after {} idle minutes",
                idle.as_secs() / 60
            );
            match vault.lock(false) {
                Ok(()) => notify(&format!(
                    "Locked {name} after {} of inactivity",
                    describe_minutes(timeout)
                )),
                Err(error) => tracing::warn!("auto-locking {name} failed: {error:#}"),
            }
        }
    }

    /// Re-reads mount state, starts or drops activity watchers, and updates the tray.
    fn refresh(&mut self) {
        let mut statuses = Vec::new();
        for (name, vault_config) in self.config.visible_vaults() {
            let vault = Vault::new(name, vault_config);
            let mounted = vault.is_mounted();
            if !mounted {
                self.activity.remove(name);
            } else if !self.activity.contains_key(name) {
                let last = Arc::new(Mutex::new(Instant::now()));
                let watched = last.clone();
                let root = vault_config.mount_dir.clone();
                std::thread::spawn(move || {
                    if let Err(error) = activity::watch(&root, watched) {
                        tracing::warn!("watching {} failed: {error:#}", root.display());
                    }
                });
                self.activity.insert(name.to_owned(), last);
            }
            let auto_lock_minutes = self
                .state
                .auto_lock_minutes(name, vault_config.auto_lock_minutes);
            let locks_in = match (auto_lock_minutes, self.activity.get(name)) {
                (Some(timeout), Some(last)) => {
                    let idle = last.lock().unwrap().elapsed().as_secs() / 60;
                    Some(timeout.saturating_sub(idle).max(1))
                }
                _ => None,
            };
            statuses.push((name.to_owned(), mounted, auto_lock_minutes, locks_in));
        }
        self.handle.update(|tray: &mut Tray| {
            for (name, mounted, auto_lock_minutes, locks_in) in statuses {
                if let Some(status) = tray.vaults.iter_mut().find(|vault| vault.name == name) {
                    status.mounted = mounted;
                    status.auto_lock_minutes = auto_lock_minutes;
                    status.locks_in = locks_in;
                }
            }
        });
    }
}

/// Locks, asking first if programs are using the vault.
fn lock(pinentry: &Pinentry, vault: &Vault) {
    let result = match vault.lock(false) {
        Err(error) if error.downcast_ref::<vault::InUse>().is_some() => {
            let users = &error.downcast_ref::<vault::InUse>().unwrap().0;
            let question = format!(
                "{} {} using the {} vault. Lock it anyway? They may lose access to open files, and unsaved changes may be lost.",
                users.join(", "),
                if users.len() == 1 { "is" } else { "are" },
                vault.name
            );
            if !pinentry.confirm(&question, "Lock anyway") {
                return;
            }
            vault.lock(true)
        }
        result => result,
    };
    if let Err(error) = result {
        pinentry.show_error(&format!("Locking {} failed: {error:#}", vault.name));
    }
}

/// Opens a folder without blocking the worker; falls back from xdg-open to gio.
fn open_folder(pinentry: Pinentry, path: std::path::PathBuf) {
    std::thread::spawn(move || {
        let openers: [&[&str]; 2] = [&["xdg-open"], &["gio", "open"]];
        for opener in openers {
            let (program, args) = opener.split_first().unwrap();
            let opened = std::process::Command::new(program)
                .args(args)
                .arg(&path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if opened {
                return;
            }
        }
        pinentry.show_error(&format!(
            "Could not open {}: no file manager is registered for folders.",
            path.display()
        ));
    });
}

fn notify(message: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=gocryptfs-tray", "Vault locked", message])
        .spawn();
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
