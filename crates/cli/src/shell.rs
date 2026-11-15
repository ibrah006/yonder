//! `yonder shell`: an interactive terminal on a machine that survives disconnects.
//!
//! Your terminal switches to raw mode so every key (Ctrl-C, arrows, tab) goes to the
//! remote shell. Ctrl-] detaches and leaves the shell running. If the connection drops,
//! this reconnects and continues from the last byte received.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::sync::mpsc;
use yonder_core::controller::{Controller, TerminalEvent};
use yonder_core::store::{PairedMachine, format_datetime_ms};
use yonder_core::terminal::SessionId;

/// Ctrl-] — the same detach key as telnet.
const DETACH_KEY: u8 = 0x1d;

/// Puts the local terminal in raw mode and restores it however we leave.
struct RawMode;

impl RawMode {
    fn enable() -> Result<RawMode> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// The local window's size; 80x24 when the terminal doesn't say (pipes, some CI shells).
fn local_size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (cols, rows),
        _ => (80, 24),
    }
}

/// Writes to the local terminal while in raw mode (newlines need a carriage return).
fn note(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\r\n[yonder] {text}\r\n");
    let _ = out.flush();
}

/// Reads keys on a plain thread (stdin reads block) and hands them over.
fn spawn_stdin() -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

/// Opens a new shell, or attaches to `session`, and runs it until exit or detach.
pub async fn run(
    controller: &Controller,
    machine: &PairedMachine,
    session: Option<SessionId>,
) -> Result<ExitCode> {
    let (cols, rows) = local_size();
    // Attaching shows the scrollback so you can see where you were.
    let (info, mut writer, mut reader) = controller
        .open_terminal(machine, session, cols, rows, None)
        .await?;
    let id = info.id.clone();
    eprintln!(
        "[yonder] {} shell {id} on {}. Ctrl-] to detach (it keeps running).",
        if info.output_end > 0 {
            "Attached to"
        } else {
            "Started"
        },
        machine.name
    );

    let _raw = RawMode::enable()?;
    let mut keys = spawn_stdin();
    let mut size = (cols, rows);
    let mut resize_tick = tokio::time::interval(Duration::from_millis(500));
    let mut offset = 0u64;

    loop {
        tokio::select! {
            event = reader.next() => match event {
                Ok(TerminalEvent::Output { offset: at, data }) => {
                    let skip = offset.saturating_sub(at) as usize;
                    if skip < data.len() {
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(&data[skip..]);
                        let _ = out.flush();
                    }
                    offset = offset.max(at + data.len() as u64);
                }
                Ok(TerminalEvent::Exit(code)) => {
                    note(&format!("shell exited{}", code.map(|c| format!(" with code {c}")).unwrap_or_default()));
                    return Ok(ExitCode::from(code.and_then(|c| u8::try_from(c).ok()).unwrap_or(0)));
                }
                Err(yonder_core::Error::Remote(err)) => {
                    drop(_raw);
                    bail!(err.to_string());
                }
                Err(err) => {
                    note(&format!("connection lost ({err}); reconnecting… (Ctrl-] to give up)"));
                    match reconnect(controller, machine, &id, offset, &mut keys).await? {
                        Some((w, r)) => {
                            writer = w;
                            reader = r;
                            note("reconnected");
                        }
                        None => {
                            note(&format!("detached; reattach with `yonder shell {} {id}`", machine.name));
                            return Ok(ExitCode::SUCCESS);
                        }
                    }
                }
            },
            key = keys.recv() => match key {
                Some(bytes) => {
                    if let Some(pos) = bytes.iter().position(|b| *b == DETACH_KEY) {
                        if pos > 0 {
                            let _ = writer.send(&bytes[..pos]).await;
                        }
                        writer.detach();
                        note(&format!("detached; the shell keeps running. Reattach: yonder shell {} {id}", machine.name));
                        return Ok(ExitCode::SUCCESS);
                    }
                    // A failed send shows up as a read error, which reconnects.
                    let _ = writer.send(&bytes).await;
                }
                None => {
                    writer.detach();
                    return Ok(ExitCode::SUCCESS);
                }
            },
            _ = resize_tick.tick() => {
                let now = local_size();
                if now != size {
                    size = now;
                    let _ = writer.resize(now.0, now.1).await;
                }
            }
        }
    }
}

type Halves = (
    yonder_core::controller::TerminalWriter,
    yonder_core::controller::TerminalReader,
);

/// Retries until reattached, or `None` if the user pressed Ctrl-].
async fn reconnect(
    controller: &Controller,
    machine: &PairedMachine,
    id: &SessionId,
    offset: u64,
    keys: &mut mpsc::Receiver<Vec<u8>>,
) -> Result<Option<Halves>> {
    let mut backoff = Duration::from_secs(1);
    loop {
        let (cols, rows) = local_size();
        let attempt = controller.open_terminal(machine, Some(id.clone()), cols, rows, Some(offset));
        tokio::select! {
            result = attempt => match result {
                Ok((_, w, r)) => return Ok(Some((w, r))),
                Err(yonder_core::Error::Remote(err)) => bail!(err.to_string()),
                Err(_) => {}
            },
            key = keys.recv() => {
                if key.is_none_or(|k| k.contains(&DETACH_KEY)) {
                    return Ok(None);
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            key = keys.recv() => {
                if key.is_none_or(|k| k.contains(&DETACH_KEY)) {
                    return Ok(None);
                }
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(10));
    }
}

/// Resolves a session: nothing or `last` means the newest; otherwise an id or its start.
pub async fn resolve(
    controller: &Controller,
    machine: &PairedMachine,
    query: &str,
) -> Result<SessionId> {
    if let Some(id) = SessionId::parse(query) {
        return Ok(id);
    }
    let sessions = controller.terminals(machine).await?;
    let query = query.trim().to_lowercase();
    if query == "last" {
        return sessions
            .first()
            .map(|s| s.id.clone())
            .ok_or_else(|| anyhow::anyhow!("{} has no shells", machine.name));
    }
    let matches: Vec<_> = sessions
        .iter()
        .filter(|s| s.id.as_str().starts_with(&query))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("no shell on {} starts with \"{query}\"", machine.name),
        _ => bail!("several shells start with \"{query}\"; type more of the id"),
    }
}

pub async fn list(controller: &Controller, machine: &PairedMachine) -> Result<()> {
    let sessions = controller.terminals(machine).await?;
    if sessions.is_empty() {
        println!(
            "No shells on {}. Start one with `yonder shell {}`.",
            machine.name, machine.name
        );
        return Ok(());
    }
    println!(
        "{:16}  {:16}  {:12}  {:9}  STATE",
        "ID", "STARTED", "BY", "SIZE"
    );
    for s in sessions {
        let state = if s.exited {
            format!(
                "exited {}",
                s.exit_code.map(|c| c.to_string()).unwrap_or_default()
            )
        } else if s.attached > 0 {
            format!("attached ({})", s.attached)
        } else {
            "detached".to_string()
        };
        println!(
            "{:16}  {:16}  {:12}  {:9}  {}",
            s.id.as_str(),
            format_datetime_ms(s.created_at_ms),
            s.started_by.chars().take(12).collect::<String>(),
            format!("{}x{}", s.cols, s.rows),
            state
        );
    }
    Ok(())
}
