//! `yonder secret`: values your jobs need on a machine (tokens, signing keys), saved
//! there through Yonder and handed only to jobs that ask for them with `--secret` or
//! `--secret-file`.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use yonder_core::controller::Controller;
use yonder_core::protocol::now_ms;
use yonder_core::secrets::SecretName;
use yonder_core::store::format_datetime_ms;

#[derive(Debug, clap::Subcommand)]
pub enum SecretCmd {
    /// Save a secret on a machine for your jobs.
    ///
    /// The value comes from --file, from stdin when piped, or is typed (hidden).
    /// Example: shorebird login:ci | yonder secret set win SHOREBIRD_TOKEN
    Set {
        machine: String,
        name: String,
        /// Read the value from this file (keys, certificates).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Stop jobs using it after this long, e.g. 90d, 12h, 2w.
        #[arg(long, value_name = "DURATION")]
        expires: Option<String>,
        /// What it is, shown in lists.
        #[arg(long)]
        note: Option<String>,
    },
    /// List your secrets on a machine (never their values).
    #[command(alias = "ls")]
    List { machine: String },
    /// Delete one of your secrets from a machine.
    #[command(alias = "remove")]
    Rm { machine: String, name: String },
}

pub async fn run(controller: &Controller, command: SecretCmd) -> Result<()> {
    match command {
        SecretCmd::Set {
            machine,
            name,
            file,
            expires,
            note,
        } => {
            let name = parse_name(&name)?;
            let expires_at_ms = expires
                .map(|text| parse_duration(&text).map(|ms| now_ms() + ms))
                .transpose()?;
            let machine = controller.pick_machine(&machine).await?;
            let value = match file {
                Some(path) => std::fs::read(&path)
                    .with_context(|| format!("couldn't read {}", path.display()))?,
                None => read_value(&name)?,
            };
            if value.is_empty() {
                bail!("the value is empty; nothing saved");
            }
            let info = controller
                .set_secret(&machine, name, value, expires_at_ms, note)
                .await?;
            println!(
                "Saved {} ({} bytes) on {}{}.",
                info.name,
                info.size,
                machine.name,
                info.expires_at_ms
                    .map(|at| format!(", until {}", format_datetime_ms(at)))
                    .unwrap_or_default()
            );
            println!(
                "Use it: yonder run {} --secret {} -- …   (or --secret-file {}=<path>)",
                machine.name, info.name, info.name
            );
        }
        SecretCmd::List { machine } => {
            let machine = controller.pick_machine(&machine).await?;
            let secrets = controller.secrets(&machine).await?;
            if secrets.is_empty() {
                println!("No secrets of yours on {}.", machine.name);
                return Ok(());
            }
            let now = now_ms();
            println!(
                "{:24}  {:>8}  {:16}  {:16}  {:16}  NOTE",
                "NAME", "BYTES", "SAVED", "EXPIRES", "LAST USED"
            );
            for s in secrets {
                let expires = match s.expires_at_ms {
                    Some(_) if s.expired(now) => "expired".to_string(),
                    Some(at) => format_datetime_ms(at),
                    None => "never".to_string(),
                };
                println!(
                    "{:24}  {:>8}  {:16}  {:16}  {:16}  {}",
                    s.name.as_str(),
                    s.size,
                    format_datetime_ms(s.created_at_ms),
                    expires,
                    s.last_used_ms
                        .map(format_datetime_ms)
                        .unwrap_or_else(|| "never".into()),
                    s.note.unwrap_or_default()
                );
            }
        }
        SecretCmd::Rm { machine, name } => {
            let name = parse_name(&name)?;
            let machine = controller.pick_machine(&machine).await?;
            controller.remove_secret(&machine, &name).await?;
            println!("Deleted {name} from {}.", machine.name);
        }
    }
    Ok(())
}

fn parse_name(text: &str) -> Result<SecretName> {
    SecretName::parse(text).ok_or_else(|| {
        anyhow::anyhow!("\"{text}\" isn't a secret name (letters, digits, _ - . up to 64)")
    })
}

/// `90d`, `12h`, `30m`, `2w` in milliseconds.
fn parse_duration(text: &str) -> Result<u64> {
    let bad = || anyhow::anyhow!("\"{text}\" isn't a duration; use e.g. 30m, 12h, 90d or 2w");
    let text = text.trim();
    if text.is_empty() || !text.is_ascii() {
        return Err(bad());
    }
    let (number, unit) = text.split_at(text.len() - 1);
    let unit_ms: u64 = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 7 * 86_400_000,
        _ => return Err(bad()),
    };
    let number: u64 = number.parse().ok().filter(|n| *n > 0).ok_or_else(bad)?;
    Ok(number * unit_ms)
}

/// The value from stdin when piped (one trailing newline dropped), or typed without echo.
fn read_value(name: &SecretName) -> Result<Vec<u8>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut value = Vec::new();
        stdin.lock().read_to_end(&mut value)?;
        if value.ends_with(b"\n") {
            value.pop();
            if value.ends_with(b"\r") {
                value.pop();
            }
        }
        return Ok(value);
    }
    eprint!("Value for {name} (hidden; Enter to finish): ");
    std::io::stderr().flush()?;
    let typed = read_hidden();
    eprintln!();
    Ok(typed?.into_bytes())
}

fn read_hidden() -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    crossterm::terminal::enable_raw_mode()?;
    let mut value = String::new();
    let result = loop {
        match crossterm::event::read() {
            Ok(Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            })) => match code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled; nothing saved"));
                }
                KeyCode::Char(c) => value.push(c),
                _ => {}
            },
            Ok(Event::Paste(text)) => value.push_str(&text),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    result.map(|()| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m").unwrap(), 1_800_000);
        assert_eq!(parse_duration("2d").unwrap(), 172_800_000);
        assert_eq!(parse_duration("1w").unwrap(), 604_800_000);
        for bad in ["", "d", "0d", "5", "5y", "-1d"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }
}
