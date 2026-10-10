use std::io::{BufRead, IsTerminal};

use anyhow::{Context, Result, bail};
use secrecy::{ExposeSecret, SecretString};

/// Reads a PIN from the terminal, or one line of stdin when stdin is not a terminal.
pub fn terminal_pin(prompt: &str) -> Result<SecretString> {
    if std::io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password(prompt)?.into());
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let pin = SecretString::from(line);
    Ok(pin
        .expose_secret()
        .trim_end_matches(['\r', '\n'])
        .to_owned()
        .into())
}

pub fn terminal_new_pin(min_length: usize) -> Result<SecretString> {
    let pin = terminal_pin("New PIN: ")?;
    check_length(&pin, min_length)?;
    let repeat = terminal_pin("Repeat new PIN: ")?;
    if pin.expose_secret() != repeat.expose_secret() {
        bail!("PINs do not match");
    }
    Ok(pin)
}

pub fn check_length(pin: &SecretString, min_length: usize) -> Result<()> {
    if pin.expose_secret().chars().count() < min_length {
        bail!("PIN must be at least {min_length} characters");
    }
    Ok(())
}

#[derive(Clone)]
pub struct Pinentry {
    program: Option<String>,
}

impl Pinentry {
    pub fn new(program: Option<String>) -> Self {
        Self { program }
    }

    /// Returns `None` if the user cancelled.
    pub fn ask(
        &self,
        description: &str,
        prompt: &str,
        error: Option<&str>,
        confirm: bool,
    ) -> Result<Option<SecretString>> {
        let mut input = match &self.program {
            Some(program) => pinentry::PassphraseInput::with_binary(program),
            None => pinentry::PassphraseInput::with_default_binary(),
        }
        .context("no pinentry program found")?;
        input
            .with_title("gocryptfs-tray")
            .with_description(description)
            .with_prompt(prompt)
            .required("A PIN is required");
        if let Some(error) = error {
            input.with_error(error);
        }
        if confirm {
            input.with_confirmation("Repeat:", "PINs do not match");
        }
        match input.interact() {
            Ok(pin) => Ok(Some(pin)),
            Err(pinentry::Error::Cancelled) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("pinentry failed: {error}")),
        }
    }

    /// Asks a yes/no question; anything but an explicit yes is a no.
    pub fn confirm(&self, question: &str, yes: &str) -> bool {
        let dialog = match &self.program {
            Some(program) => pinentry::ConfirmationDialog::with_binary(program),
            None => pinentry::ConfirmationDialog::with_default_binary(),
        };
        dialog.is_some_and(|mut dialog| {
            dialog
                .with_title("gocryptfs-tray")
                .with_ok(yes)
                .with_cancel("Cancel")
                .confirm(question)
                .unwrap_or(false)
        })
    }

    pub fn show_error(&self, message: &str) {
        let dialog = match &self.program {
            Some(program) => pinentry::MessageDialog::with_binary(program),
            None => pinentry::MessageDialog::with_default_binary(),
        };
        if let Some(mut dialog) = dialog {
            let _ = dialog.with_title("gocryptfs-tray").show_message(message);
        }
    }
}
