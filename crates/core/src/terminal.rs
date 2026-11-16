//! Persistent terminal sessions on the agent.
//!
//! A session is a shell running in a pseudo-terminal (ConPTY on Windows, a Unix PTY
//! elsewhere). It belongs to the agent, not to a connection: closing the laptop lid or
//! losing the network leaves it running, and any paired device with the terminal
//! permission can reattach.
//!
//! Output is kept in a scrollback buffer addressed by byte offset. Attaching "from offset
//! N" replays exactly what that client missed, the same way job logs work.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tracing::{debug, info};

use crate::error::{Error, Result};
use crate::protocol::now_ms;

/// Identifies a terminal session. Same format as job ids: 16 lowercase hex characters.
pub type SessionId = crate::jobs::JobId;

/// Scrollback kept per session for replay after reconnecting.
const SCROLLBACK_BYTES: usize = 512 * 1024;
/// Sessions a machine keeps at once.
const MAX_SESSIONS: usize = 16;
/// Exited sessions are forgotten after this long.
const KEEP_EXITED: Duration = Duration::from_secs(10 * 60);

/// A session as shown to controllers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: SessionId,
    pub created_at_ms: u64,
    /// Device that started it.
    pub started_by: String,
    pub cols: u16,
    pub rows: u16,
    /// Clients attached right now.
    pub attached: u32,
    /// Total output so far, in bytes. Attach from here to see only new output.
    pub output_end: u64,
    /// Set once the shell has exited.
    pub exit_code: Option<u32>,
    pub exited: bool,
}

/// What a client sends after attaching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalInput {
    /// Keystrokes and pasted text.
    Data(Vec<u8>),
    /// The client's window changed size.
    Resize { cols: u16, rows: u16 },
}

struct Scrollback {
    /// Offset of the first byte still kept.
    base: u64,
    data: VecDeque<u8>,
}

impl Scrollback {
    fn end(&self) -> u64 {
        self.base + self.data.len() as u64
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes);
        let excess = self.data.len().saturating_sub(SCROLLBACK_BYTES);
        if excess > 0 {
            self.data.drain(..excess);
            self.base += excess as u64;
        }
    }

    /// Bytes from `from` (or from the oldest kept byte), at most `max`. Returns the offset
    /// the bytes start at.
    fn read(&self, from: u64, max: usize) -> (u64, Vec<u8>) {
        let start = from.max(self.base).min(self.end());
        let skip = (start - self.base) as usize;
        (
            start,
            self.data.iter().skip(skip).take(max).copied().collect(),
        )
    }
}

/// One shell.
pub struct Session {
    id: SessionId,
    created_at_ms: u64,
    started_by: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    scrollback: Mutex<Scrollback>,
    size: Mutex<(u16, u16)>,
    attached: Mutex<u32>,
    /// `Some(code)` once exited (`None` inside means no code was available).
    exit: Mutex<Option<Option<u32>>>,
    exited_at: Mutex<Option<std::time::Instant>>,
    /// Bumped whenever there's new output or the shell exits.
    changed: watch::Sender<u64>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("id", &self.id).finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Session {
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn info(&self) -> TerminalInfo {
        let (cols, rows) = *lock(&self.size);
        let exit = *lock(&self.exit);
        TerminalInfo {
            id: self.id.clone(),
            created_at_ms: self.created_at_ms,
            started_by: self.started_by.clone(),
            cols,
            rows,
            attached: *lock(&self.attached),
            output_end: lock(&self.scrollback).end(),
            exit_code: exit.flatten(),
            exited: exit.is_some(),
        }
    }

    pub fn is_exited(&self) -> bool {
        lock(&self.exit).is_some()
    }

    /// Sends keystrokes to the shell.
    pub fn write(&self, data: &[u8]) -> Result<()> {
        let mut writer = lock(&self.writer);
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|e| Error::Protocol(format!("couldn't write to the terminal: {e}")))
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.clamp(10, 1000), rows.clamp(2, 1000));
        *lock(&self.size) = (cols, rows);
        let _ = lock(&self.master).resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Output from `from`, at most `max` bytes, and the offset it starts at.
    pub fn read(&self, from: u64, max: usize) -> (u64, Vec<u8>) {
        lock(&self.scrollback).read(from, max)
    }

    pub fn output_end(&self) -> u64 {
        lock(&self.scrollback).end()
    }

    /// Exit code, once the shell has exited.
    pub fn exit(&self) -> Option<Option<u32>> {
        *lock(&self.exit)
    }

    /// Wakes when there's new output or the shell exits.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Counts an attached client until the guard is dropped.
    pub fn attach(self: &Arc<Self>) -> AttachGuard {
        *lock(&self.attached) += 1;
        AttachGuard(self.clone())
    }

    pub fn kill(&self) {
        let _ = lock(&self.killer).kill();
    }
}

/// Keeps a session's attached count while a client is connected.
#[derive(Debug)]
pub struct AttachGuard(Arc<Session>);

impl Drop for AttachGuard {
    fn drop(&mut self) {
        let mut attached = lock(&self.0.attached);
        *attached = attached.saturating_sub(1);
    }
}

/// All sessions on this machine.
#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
}

impl std::fmt::Debug for TerminalManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalManager").finish()
    }
}

impl TerminalManager {
    pub fn new() -> Arc<TerminalManager> {
        Arc::new(TerminalManager::default())
    }

    /// Starts a new shell.
    pub fn open(&self, started_by: &str, cols: u16, rows: u16) -> Result<Arc<Session>> {
        self.forget_old();
        if lock(&self.sessions).len() >= MAX_SESSIONS {
            return Err(Error::Protocol(format!(
                "this machine already has {MAX_SESSIONS} terminal sessions; close some first"
            )));
        }
        let (cols, rows) = (cols.clamp(10, 1000), rows.clamp(2, 1000));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_err)?;
        let child = pair.slave.spawn_command(shell_command()).map_err(pty_err)?;
        // The shell has its own copy of the terminal; dropping ours lets EOF arrive when it exits.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty_err)?;
        let writer = pair.master.take_writer().map_err(pty_err)?;
        let killer = child.clone_killer();

        let id = SessionId::generate();
        let (changed, _) = watch::channel(0);
        let session = Arc::new(Session {
            id: id.clone(),
            created_at_ms: now_ms(),
            started_by: started_by.to_string(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            scrollback: Mutex::new(Scrollback {
                base: 0,
                data: VecDeque::new(),
            }),
            size: Mutex::new((cols, rows)),
            attached: Mutex::new(0),
            exit: Mutex::new(None),
            exited_at: Mutex::new(None),
            changed,
        });
        spawn_reader(session.clone(), reader, child);
        lock(&self.sessions).insert(id.clone(), session.clone());
        info!(session = %id, by = started_by, "terminal opened");
        Ok(session)
    }

    pub fn get(&self, id: &SessionId) -> Option<Arc<Session>> {
        lock(&self.sessions).get(id).cloned()
    }

    /// Sessions, newest first.
    pub fn list(&self) -> Vec<TerminalInfo> {
        self.forget_old();
        let mut list: Vec<_> = lock(&self.sessions).values().map(|s| s.info()).collect();
        list.sort_by(|a, b| b.id.cmp(&a.id));
        list
    }

    /// Ends a session's shell and forgets it.
    pub fn close(&self, id: &SessionId) -> Option<TerminalInfo> {
        let session = lock(&self.sessions).remove(id)?;
        session.kill();
        info!(session = %id, "terminal closed");
        Some(session.info())
    }

    fn forget_old(&self) {
        lock(&self.sessions).retain(|_, s| {
            lock(&s.exited_at).is_none_or(|at| at.elapsed() < KEEP_EXITED || *lock(&s.attached) > 0)
        });
    }
}

/// Reads the shell's output into the scrollback until it exits.
fn spawn_reader(
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let end = {
                        let mut scrollback = lock(&session.scrollback);
                        scrollback.push(&buf[..n]);
                        scrollback.end()
                    };
                    session.changed.send_replace(end);
                }
            }
        }
        let code = child.wait().ok().map(|status| status.exit_code());
        *lock(&session.exit) = Some(code);
        *lock(&session.exited_at) = Some(std::time::Instant::now());
        let end = lock(&session.scrollback).end();
        // Different value than any output offset notification, so watchers wake up.
        session
            .changed
            .send_modify(|v| *v = end.wrapping_add(1) | (1 << 63));
        debug!(session = %session.id, ?code, "terminal shell exited");
    });
}

#[cfg(windows)]
fn shell_command() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.arg("-NoLogo");
    if let Some(home) = std::env::var_os("USERPROFILE") {
        cmd.cwd(home);
    }
    cmd
}

#[cfg(not(windows))]
fn shell_command() -> CommandBuilder {
    // The user's login shell.
    let mut cmd = CommandBuilder::new_default_prog();
    if let Some(home) = std::env::var_os("HOME") {
        cmd.cwd(home);
    }
    cmd.env("TERM", "xterm-256color");
    cmd
}

/// portable-pty reports `anyhow` errors; we only need their text.
fn pty_err(err: impl std::fmt::Display) -> Error {
    Error::Protocol(format!("couldn't start a terminal: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_drops_oldest_and_keeps_offsets() {
        let mut sb = Scrollback {
            base: 0,
            data: VecDeque::new(),
        };
        sb.push(&vec![b'a'; SCROLLBACK_BYTES]);
        sb.push(b"xyz");
        assert_eq!(sb.base, 3);
        assert_eq!(sb.end(), SCROLLBACK_BYTES as u64 + 3);
        // Asking for dropped bytes starts at the oldest kept one.
        let (start, data) = sb.read(0, 2);
        assert_eq!((start, data.as_slice()), (3, b"aa".as_slice()));
        let (start, data) = sb.read(sb.end() - 3, 10);
        assert_eq!((start, data.as_slice()), (sb.end() - 3, b"xyz".as_slice()));
        // Past the end: nothing.
        assert!(sb.read(sb.end() + 5, 10).1.is_empty());
    }

    async fn wait_for(session: &Session, text: &str) -> String {
        let mut changes = session.subscribe();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let (_, data) = session.read(0, usize::MAX);
            let out = String::from_utf8_lossy(&data).into_owned();
            if out.contains(text) {
                return out;
            }
            tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep_until(deadline) => panic!("never saw {text:?} in {out:?}"),
            }
        }
    }

    #[tokio::test]
    async fn shell_runs_commands_and_reports_exit() {
        let manager = TerminalManager::new();
        let session = manager.open("test", 80, 24).unwrap();
        session.write(b"echo yonder-$((40+2))\r").unwrap();
        wait_for(&session, "yonder-42").await;

        session.write(b"exit 3\r").unwrap();
        let mut changes = session.subscribe();
        tokio::time::timeout(Duration::from_secs(15), async {
            while !session.is_exited() {
                let _ = changes.changed().await;
            }
        })
        .await
        .expect("shell exited");
        assert_eq!(session.exit(), Some(Some(3)));
        assert!(session.info().exited);
    }

    #[tokio::test]
    async fn sessions_are_listed_resized_and_closed() {
        let manager = TerminalManager::new();
        let a = manager.open("mac", 80, 24).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let b = manager.open("phone", 80, 24).unwrap();
        b.resize(120, 40);
        let list = manager.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, *b.id(), "newest first");
        assert_eq!((list[0].cols, list[0].rows), (120, 40));

        let closed = manager.close(a.id()).unwrap();
        assert_eq!(closed.started_by, "mac");
        assert!(manager.get(a.id()).is_none());
        manager.close(b.id());
    }

    #[test]
    fn attach_guard_counts_clients() {
        let manager = TerminalManager::new();
        let s = manager.open("t", 80, 24).unwrap();
        {
            let _one = s.attach();
            let _two = s.attach();
            assert_eq!(s.info().attached, 2);
        }
        assert_eq!(s.info().attached, 0);
        manager.close(s.id());
    }
}
