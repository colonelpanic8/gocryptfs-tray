use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use secrecy::{ExposeSecret, SecretString};

use crate::activity;
use crate::config::{Backend, VaultConfig};
use crate::mounts;

/// gocryptfs exit code for a wrong password.
const EXIT_PASSWORD_INCORRECT: i32 = 12;

#[derive(Debug)]
pub struct WrongPassword;

impl std::fmt::Display for WrongPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("wrong PIN or secret")
    }
}

impl std::error::Error for WrongPassword {}

pub fn is_wrong_password(error: &anyhow::Error) -> bool {
    error.downcast_ref::<WrongPassword>().is_some()
}

/// Locking was refused because these programs are using the vault.
#[derive(Debug)]
pub struct InUse(pub Vec<String>);

impl std::fmt::Display for InUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the vault is in use by: {}", self.0.join(", "))
    }
}

impl std::error::Error for InUse {}

pub struct Vault<'a> {
    pub name: &'a str,
    pub config: &'a VaultConfig,
}

impl<'a> Vault<'a> {
    pub fn new(name: &'a str, config: &'a VaultConfig) -> Self {
        Self { name, config }
    }

    pub fn is_mounted(&self) -> bool {
        mounts::is_mounted(&self.config.mount_dir).unwrap_or(false)
    }

    pub fn is_initialized(&self) -> Result<bool> {
        let conf = self.config.cipher_dir.join("gocryptfs.conf");
        conf.try_exists()
            .with_context(|| format!("checking {}", conf.display()))
    }

    /// The path a user should open to see the decrypted files.
    pub fn open_path(&self) -> &Path {
        self.config
            .link
            .as_deref()
            .unwrap_or(&self.config.mount_dir)
    }

    pub fn unlock(&self, pin: &SecretString) -> Result<()> {
        if self.is_mounted() {
            return Ok(());
        }
        let password = self.password(pin)?;
        match &self.config.backend {
            Backend::Direct { gocryptfs_args } => {
                std::fs::create_dir_all(&self.config.mount_dir)?;
                let mut command = Command::new("gocryptfs");
                command
                    .arg("-q")
                    .args(gocryptfs_args)
                    .arg("--")
                    .arg(&self.config.cipher_dir)
                    .arg(&self.config.mount_dir);
                run_with_stdin(command, &password)?;
            }
            Backend::Systemd {
                mount_unit,
                handoff_dir,
                ..
            } => {
                let handoff = Handoff::write(handoff_dir.join("passphrase"), &password)?;
                let result = start_unit(mount_unit);
                drop(handoff);
                result?;
            }
        }
        if !self.is_mounted() {
            bail!(
                "{} did not appear as a mount point",
                self.config.mount_dir.display()
            );
        }
        Ok(())
    }

    /// Unless `force`, refuses while programs are using the vault. With
    /// `force`, unmounts lazily: the vault disappears at once, and programs
    /// still using it may lose access to their open files.
    pub fn lock(&self, force: bool) -> Result<()> {
        if !self.is_mounted() {
            return Ok(());
        }
        if !force {
            let users = activity::users(&self.config.mount_dir);
            if !users.is_empty() {
                return Err(InUse(users).into());
            }
        }
        match &self.config.backend {
            Backend::Direct { .. } => {
                let flags: &[&str] = if force { &["-u", "-z"] } else { &["-u"] };
                let status = Command::new("fusermount3")
                    .args(flags)
                    .arg(&self.config.mount_dir)
                    .status()
                    .or_else(|_| {
                        Command::new("fusermount")
                            .args(flags)
                            .arg(&self.config.mount_dir)
                            .status()
                    })
                    .context("running fusermount")?;
                if !status.success() {
                    bail!("fusermount failed; is something still using the vault?");
                }
            }
            // gocryptfs falls back to a lazy unmount itself when it is stopped while busy.
            Backend::Systemd { mount_unit, .. } => systemctl(&["stop", mount_unit])?,
        }
        Ok(())
    }

    pub fn change_pin(&self, old_pin: &SecretString, new_pin: &SecretString) -> Result<()> {
        let old = self.password(old_pin)?;
        let new = self.password(new_pin)?;
        match &self.config.backend {
            Backend::Direct { .. } => {
                let runtime = dirs::runtime_dir().context("XDG_RUNTIME_DIR is not set")?;
                let old_file = Handoff::write(
                    runtime.join(format!("gocryptfs-tray-{}.old", std::process::id())),
                    &old,
                )?;
                let mut command = Command::new("gocryptfs");
                command
                    .args(["-q", "-passwd", "-passfile"])
                    .arg(&old_file.path)
                    .arg("--")
                    .arg(&self.config.cipher_dir);
                run_with_stdin(command, &new)
            }
            Backend::Systemd {
                passwd_unit,
                handoff_dir,
                ..
            } => {
                let old_file = Handoff::write(handoff_dir.join("passphrase.old"), &old)?;
                let new_file = Handoff::write(handoff_dir.join("passphrase.new"), &new)?;
                let result = start_unit(passwd_unit);
                drop((old_file, new_file));
                result
            }
        }
    }

    pub fn init(&self, pin: &SecretString) -> Result<()> {
        if self.is_initialized().unwrap_or(false) {
            bail!("vault {} is already initialized", self.name);
        }
        let password = self.password(pin)?;
        match &self.config.backend {
            Backend::Direct { .. } => {
                std::fs::create_dir_all(&self.config.cipher_dir)?;
                let mut command = Command::new("gocryptfs");
                command
                    .args(["-q", "-init", "--"])
                    .arg(&self.config.cipher_dir);
                run_with_stdin(command, &password)
            }
            Backend::Systemd {
                init_unit,
                handoff_dir,
                ..
            } => {
                let new_file = Handoff::write(handoff_dir.join("passphrase.new"), &password)?;
                let result = start_unit(init_unit);
                drop(new_file);
                result
            }
        }
    }

    /// The vault password: the secret command's first line followed by the PIN.
    fn password(&self, pin: &SecretString) -> Result<SecretString> {
        let mut password = match &self.config.secret_command {
            Some(command) => read_secret(command)?,
            None => String::new(),
        };
        if self.config.pin {
            password.push_str(pin.expose_secret());
        }
        if password.is_empty() {
            bail!("refusing to use an empty password");
        }
        Ok(password.into())
    }
}

fn read_secret(command: &[String]) -> Result<String> {
    let (program, args) = command.split_first().context("secret_command is empty")?;
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!("{program} exited with {}", output.status);
    }
    let stdout =
        SecretString::from(String::from_utf8(output.stdout).context("secret is not UTF-8")?);
    let secret = stdout.expose_secret().lines().next().unwrap_or_default();
    if secret.is_empty() {
        bail!("{program} printed an empty secret");
    }
    Ok(secret.to_owned())
}

fn run_with_stdin(mut command: Command, password: &SecretString) -> Result<()> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running gocryptfs")?;
    {
        let mut stdin = child.stdin.take().context("gocryptfs stdin")?;
        stdin.write_all(password.expose_secret().as_bytes())?;
        stdin.write_all(b"\n")?;
    }
    let output = child.wait_with_output()?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(EXIT_PASSWORD_INCORRECT) => Err(WrongPassword.into()),
        _ => bail!(
            "gocryptfs failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .context("running systemctl")?;
    if !status.success() {
        bail!("systemctl {} failed", args.join(" "));
    }
    Ok(())
}

fn start_unit(unit: &str) -> Result<()> {
    let status = Command::new("systemctl")
        .args(["start", unit])
        .stderr(Stdio::null())
        .status()
        .context("running systemctl")?;
    if status.success() {
        return Ok(());
    }
    let show = Command::new("systemctl")
        .args([
            "show",
            "--property=ExecMainStatus",
            "--property=ExecStart",
            unit,
        ])
        .output()
        .context("running systemctl show")?;
    if failed_with_wrong_password(&String::from_utf8_lossy(&show.stdout)) {
        return Err(WrongPassword.into());
    }
    bail!("{unit} failed; see: journalctl -u {unit}")
}

/// Oneshot units report the exit code as ExecMainStatus; a forking unit's start
/// process is a control process, whose code only appears in ExecStart.
fn failed_with_wrong_password(show: &str) -> bool {
    let code = EXIT_PASSWORD_INCORRECT.to_string();
    show.lines().any(|line| {
        if let Some(status) = line.strip_prefix("ExecMainStatus=") {
            return status == code;
        }
        // e.g. `ExecStart={ path=… ; code=exited ; status=12 }`, or `status=1/FAILURE`.
        line.starts_with("ExecStart=")
            && line.split(';').any(|field| {
                field
                    .trim()
                    .trim_end_matches('}')
                    .trim()
                    .strip_prefix("status=")
                    .is_some_and(|status| status.split('/').next() == Some(code.as_str()))
            })
    })
}

/// A password written to a group-readable file for a system unit; removed on drop.
struct Handoff {
    path: PathBuf,
}

impl Handoff {
    fn write(path: PathBuf, password: &SecretString) -> Result<Self> {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o640)
            .open(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        let handoff = Self { path };
        // The umask may have stripped group read, which the unit's user needs.
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        file.write_all(password.expose_secret().as_bytes())?;
        file.write_all(b"\n")?;
        Ok(handoff)
    }
}

impl Drop for Handoff {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault_config(secret_command: Option<Vec<String>>, pin: bool) -> VaultConfig {
        VaultConfig {
            cipher_dir: "/nonexistent/cipher".into(),
            mount_dir: "/nonexistent/mount".into(),
            link: None,
            owner: None,
            secret_command,
            pin,
            min_pin_length: 4,
            auto_lock_minutes: None,
            backend: Backend::Direct {
                gocryptfs_args: vec![],
            },
        }
    }

    fn command(script: &str) -> Option<Vec<String>> {
        Some(vec!["sh".into(), "-c".into(), script.into()])
    }

    #[test]
    fn password_is_first_secret_line_then_pin() {
        let config = vault_config(command("printf 'abc\\nignored\\n'"), true);
        let password = Vault::new("v", &config)
            .password(&SecretString::from("1234"))
            .unwrap();
        assert_eq!(password.expose_secret(), "abc1234");
    }

    #[test]
    fn pin_is_ignored_when_disabled() {
        let config = vault_config(command("echo abc"), false);
        let password = Vault::new("v", &config)
            .password(&SecretString::from("1234"))
            .unwrap();
        assert_eq!(password.expose_secret(), "abc");
    }

    #[test]
    fn recognizes_wrong_password_from_either_unit_type() {
        let forking =
            "ExecMainStatus=0\nExecStart={ path=/bin/start ; code=exited ; status=12/n/a }";
        let oneshot = "ExecMainStatus=12\nExecStart={ path=/bin/start ; status=0/0 }";
        let other =
            "ExecMainStatus=0\nExecStart={ path=/bin/start ; code=exited ; status=1/FAILURE }";
        assert!(failed_with_wrong_password(forking));
        assert!(failed_with_wrong_password(oneshot));
        assert!(!failed_with_wrong_password(other));
    }

    #[test]
    fn failing_secret_command_is_an_error() {
        let config = vault_config(command("exit 3"), true);
        assert!(
            Vault::new("v", &config)
                .password(&SecretString::from("1234"))
                .is_err()
        );
    }
}
