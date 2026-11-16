//! Local control channel between a running agent and tools on the same machine
//! (`yonder-agent pair`, `status`, and later the tray app).
//!
//! - Windows: a named pipe only Administrators and SYSTEM may open, so approving a new
//!   device needs an administrator.
//! - Elsewhere: a Unix socket in the agent's state folder, usable only by its owner.
//!
//! Messages are JSON, one per line.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use yonder_core::agent::{
    Agent, AgentEvent, PairingApprover, PairingCandidate, PairingClosedReason, approver_fn,
};
use yonder_core::jobs::JobState;
use yonder_core::store::StateDir;

/// Sent by a local tool.
#[derive(Debug, Serialize, Deserialize)]
pub enum ClientMsg {
    Status,
    /// Open pairing for this many minutes and route approval requests to this session.
    Pair {
        minutes: u64,
    },
    /// Answer to [`AgentMsg::Approve`].
    Decision {
        allow: bool,
    },
}

/// Sent by the agent.
#[derive(Debug, Serialize, Deserialize)]
pub enum AgentMsg {
    Status(StatusInfo),
    Ticket {
        ticket: String,
        machine_name: String,
        fingerprint: String,
        minutes: u64,
    },
    Approve {
        controller_name: String,
        fingerprint: String,
    },
    Paired {
        controller_name: String,
    },
    Declined {
        controller_name: String,
    },
    Closed {
        reason: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusInfo {
    pub name: String,
    pub id: String,
    pub fingerprint: String,
    pub paused: bool,
    pub controllers: usize,
    pub jobs_running: usize,
    pub jobs_queued: usize,
}

// ---------------------------------------------------------------------------
// Approval desk

struct ApprovalRequest {
    candidate: PairingCandidate,
    reply: oneshot::Sender<bool>,
}

/// Routes pairing approvals to whichever local session opened pairing. With no session,
/// every request is declined: nobody is at the machine to say yes.
#[derive(Default)]
pub struct Desk {
    session: Mutex<Option<mpsc::Sender<ApprovalRequest>>>,
}

impl Desk {
    pub fn new() -> Arc<Desk> {
        Arc::new(Desk::default())
    }

    /// The approver to give the agent.
    pub fn approver(self: &Arc<Self>) -> Arc<dyn PairingApprover> {
        let desk = self.clone();
        approver_fn(move |candidate| {
            let desk = desk.clone();
            async move { desk.ask(candidate).await }
        })
    }

    async fn ask(&self, candidate: PairingCandidate) -> bool {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(session) = session else { return false };
        let (reply, answer) = oneshot::channel();
        if session
            .send(ApprovalRequest { candidate, reply })
            .await
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    fn claim(&self, sender: mpsc::Sender<ApprovalRequest>) -> bool {
        let mut slot = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if slot.as_ref().is_some_and(|s| !s.is_closed()) {
            return false;
        }
        *slot = Some(sender);
        true
    }

    fn release(&self) {
        *self.session.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

// ---------------------------------------------------------------------------
// Endpoint naming

/// Where the agent for `state` listens.
pub fn endpoint(state: &StateDir) -> PathBuf {
    #[cfg(windows)]
    {
        use std::hash::{Hash, Hasher};
        // One pipe per state folder, so several agents (tests) can run side by side.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        state.path().hash(&mut hasher);
        PathBuf::from(format!(r"\\.\pipe\yonder-agent-{:016x}", hasher.finish()))
    }
    #[cfg(not(windows))]
    {
        let preferred = state.path().join("agent.sock");
        // Socket paths are limited to ~104 bytes (macOS). For long state folders, use the
        // per-user runtime folder instead (private to this user on macOS and Linux).
        if preferred.as_os_str().len() < 100 {
            return preferred;
        }
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        state.path().hash(&mut hasher);
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        runtime.join(format!("yonder-agent-{:016x}.sock", hasher.finish()))
    }
}

// ---------------------------------------------------------------------------
// Server

/// Accepts local tools until the process ends.
pub async fn serve(agent: Agent, desk: Arc<Desk>, state: StateDir) -> Result<()> {
    let path = endpoint(&state);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)
            .with_context(|| format!("couldn't listen on {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        loop {
            let (stream, _) = listener.accept().await?;
            let (agent, desk, state) = (agent.clone(), desk.clone(), state.clone());
            tokio::spawn(async move { handle(stream, agent, desk, state).await });
        }
    }
    #[cfg(windows)]
    {
        let mut first = true;
        loop {
            let server = windows_pipe::create(&path, first)?;
            first = false;
            server.connect().await?;
            let (agent, desk, state) = (agent.clone(), desk.clone(), state.clone());
            tokio::spawn(async move { handle(server, agent, desk, state).await });
        }
    }
}

async fn handle<S>(stream: S, agent: Agent, desk: Arc<Desk>, state: StateDir)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (read, mut write) = tokio::io::split(stream);
    let mut lines = BufReader::new(read).lines();
    let Ok(Some(first)) = lines.next_line().await else {
        return;
    };
    let result = match serde_json::from_str::<ClientMsg>(&first) {
        Ok(ClientMsg::Status) => send(&mut write, &AgentMsg::Status(status(&agent, &state))).await,
        Ok(ClientMsg::Pair { minutes }) => {
            pair_session(&agent, &desk, minutes, &mut lines, &mut write).await
        }
        Ok(ClientMsg::Decision { .. }) => {
            send(
                &mut write,
                &AgentMsg::Error {
                    message: "no pairing request is waiting".into(),
                },
            )
            .await
        }
        Err(err) => {
            send(
                &mut write,
                &AgentMsg::Error {
                    message: format!("bad request: {err}"),
                },
            )
            .await
        }
    };
    if let Err(err) = result {
        debug!("local session ended: {err}");
    }
}

fn status(agent: &Agent, state: &StateDir) -> StatusInfo {
    let jobs = agent.jobs().map(|j| j.list()).unwrap_or_default();
    let agent_state = yonder_core::store::AgentState::load(&state.agent_file()).unwrap_or_default();
    StatusInfo {
        name: agent.name().to_string(),
        id: agent.id().to_string(),
        fingerprint: agent.fingerprint(),
        paused: agent_state.paused,
        controllers: agent_state.controllers.len(),
        jobs_running: jobs
            .iter()
            .filter(|j| j.status.state == JobState::Running)
            .count(),
        jobs_queued: jobs
            .iter()
            .filter(|j| j.status.state == JobState::Queued)
            .count(),
    }
}

async fn pair_session<R, W>(
    agent: &Agent,
    desk: &Arc<Desk>,
    minutes: u64,
    lines: &mut tokio::io::Lines<BufReader<R>>,
    write: &mut W,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (sender, mut requests) = mpsc::channel(1);
    if !desk.claim(sender) {
        return send(
            write,
            &AgentMsg::Error {
                message: "pairing is already open in another window".into(),
            },
        )
        .await;
    }
    let result = async {
        let minutes = minutes.clamp(1, 60);
        let ttl = Duration::from_secs(minutes * 60);
        let mut events = agent.subscribe();
        let ticket = agent.open_pairing(ttl).await;
        send(write, &AgentMsg::Ticket {
            ticket: ticket.encode(),
            machine_name: agent.name().to_string(),
            fingerprint: agent.fingerprint(),
            minutes,
        })
        .await?;

        let expired = tokio::time::sleep(ttl);
        tokio::pin!(expired);
        let mut pending: Option<oneshot::Sender<bool>> = None;
        loop {
            tokio::select! {
                Some(request) = requests.recv() => {
                    send(write, &AgentMsg::Approve {
                        controller_name: request.candidate.controller_name,
                        fingerprint: request.candidate.fingerprint,
                    }).await?;
                    pending = Some(request.reply);
                }
                event = events.recv() => {
                    let msg = match event {
                        Ok(AgentEvent::Paired(c)) => AgentMsg::Paired { controller_name: c.name },
                        Ok(AgentEvent::PairingDeclined(c)) => AgentMsg::Declined { controller_name: c.controller_name },
                        Ok(AgentEvent::PairingClosed(PairingClosedReason::Expired)) => AgentMsg::Closed { reason: "expired".into() },
                        Ok(AgentEvent::PairingClosed(PairingClosedReason::TooManyWrongTickets)) => {
                            AgentMsg::Closed { reason: "too many wrong tickets".into() }
                        }
                        Err(_) => AgentMsg::Closed { reason: "the agent stopped".into() },
                    };
                    return send(write, &msg).await;
                }
                _ = &mut expired => {
                    return send(write, &AgentMsg::Closed { reason: "expired".into() }).await;
                }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => match serde_json::from_str::<ClientMsg>(&line) {
                        Ok(ClientMsg::Decision { allow }) => {
                            if let Some(reply) = pending.take() {
                                let _ = reply.send(allow);
                            }
                        }
                        _ => warn!("unexpected message during pairing: {line}"),
                    },
                    // The window was closed: stop pairing (a waiting request is declined).
                    _ => return Ok(()),
                },
            }
        }
    }
    .await;
    agent.close_pairing();
    desk.release();
    result
}

async fn send<W: AsyncWrite + Unpin>(write: &mut W, msg: &AgentMsg) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    write.write_all(&line).await?;
    write.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Client

/// A connection to the running agent.
pub struct Client {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<Box<dyn Stream>>>>,
    write: tokio::io::WriteHalf<Box<dyn Stream>>,
}

pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

impl Client {
    /// Connects to the agent for `state`. `Ok(None)` means no agent is running there.
    pub async fn connect(state: &StateDir) -> Result<Option<Client>> {
        let path = endpoint(state);
        #[cfg(unix)]
        let stream: Box<dyn Stream> = match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => Box::new(stream),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None);
            }
            Err(err) => return Err(err).context("couldn't reach the running agent"),
        };
        #[cfg(windows)]
        let stream: Box<dyn Stream> = match windows_pipe::connect(&path) {
            Ok(Some(stream)) => Box::new(stream),
            Ok(None) => return Ok(None),
            Err(err) => return Err(err),
        };
        let (read, write) = tokio::io::split(stream);
        Ok(Some(Client {
            lines: BufReader::new(read).lines(),
            write,
        }))
    }

    pub async fn send(&mut self, msg: &ClientMsg) -> Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        self.write.write_all(&line).await?;
        self.write.flush().await?;
        Ok(())
    }

    /// The next message, or `None` when the agent closed the connection.
    pub async fn recv(&mut self) -> Result<Option<AgentMsg>> {
        match self.lines.next_line().await? {
            Some(line) => Ok(Some(serde_json::from_str(&line)?)),
            None => Ok(None),
        }
    }

    pub async fn status(&mut self) -> Result<StatusInfo> {
        self.send(&ClientMsg::Status).await?;
        match self.recv().await? {
            Some(AgentMsg::Status(status)) => Ok(status),
            Some(AgentMsg::Error { message }) => bail!(message),
            other => bail!("unexpected answer from the agent: {other:?}"),
        }
    }
}

#[cfg(windows)]
mod windows_pipe {
    use std::path::Path;

    use anyhow::{Context, Result, bail};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

    /// Full access for SYSTEM and Administrators only; nobody else may open the pipe.
    const SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)";

    pub fn create(path: &Path, first: bool) -> Result<NamedPipeServer> {
        let sddl: Vec<u16> = SDDL.encode_utf16().chain(Some(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid null-terminated SDDL; the descriptor is freed below.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("couldn't build the pipe's access rules");
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: attributes point at a valid descriptor for the duration of the call.
        let server = unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    path,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                )
        };
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            LocalFree(descriptor as _);
        }
        server.with_context(|| format!("couldn't open {}", path.display()))
    }

    pub fn connect(path: &Path) -> Result<Option<NamedPipeClient>> {
        for _ in 0..50 {
            match ClientOptions::new().open(path) {
                Ok(client) => return Ok(Some(client)),
                Err(err) if err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                    return Ok(None);
                }
                Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                    bail!(
                        "the running agent only accepts administrators; run this from an administrator terminal"
                    )
                }
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(err) => return Err(err).context("couldn't reach the running agent"),
            }
        }
        bail!("the running agent is busy; try again")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yonder_core::agent::AgentConfig;
    use yonder_core::controller::{Controller, ControllerConfig};
    use yonder_core::net::NetConfig;
    use yonder_core::ticket::PairingTicket;

    async fn start(dir: &tempfile::TempDir) -> (Agent, StateDir) {
        let state = StateDir::new(dir.path());
        let desk = Desk::new();
        let agent = Agent::start(
            AgentConfig {
                state: state.clone(),
                net: NetConfig::local_only(),
                name: Some("box".into()),
                jobs: None,
                shell: None,
            },
            desk.approver(),
        )
        .await
        .unwrap();
        tokio::spawn(serve(agent.clone(), desk, state.clone()));
        (agent, state)
    }

    async fn connect(state: &StateDir) -> Client {
        for _ in 0..50 {
            if let Some(client) = Client::connect(state).await.unwrap() {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("local channel never came up");
    }

    #[cfg(unix)]
    #[test]
    fn long_state_folders_get_a_short_socket_path() {
        let long = StateDir::new(std::env::temp_dir().join("x".repeat(120)));
        let path = endpoint(&long);
        assert!(path.as_os_str().len() < 104, "{}", path.display());
        assert_eq!(path, endpoint(&long), "stable for the same folder");
        let short = StateDir::new("/tmp/y");
        assert_eq!(endpoint(&short), PathBuf::from("/tmp/y/agent.sock"));
    }

    #[tokio::test]
    async fn no_agent_means_no_connection() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            Client::connect(&StateDir::new(dir.path()))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn status_reports_the_running_agent() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, state) = start(&dir).await;
        let status = connect(&state).await.status().await.unwrap();
        assert_eq!(status.name, "box");
        assert_eq!(status.fingerprint, agent.fingerprint());
        assert_eq!(status.controllers, 0);
    }

    /// Pairs through the local channel, answering the approval with `allow`.
    async fn pair_through_channel(
        allow: bool,
    ) -> (
        Result<(), yonder_core::Error>,
        Option<AgentMsg>,
        Agent,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (agent, state) = start(&dir).await;
        let mut local = connect(&state).await;
        local.send(&ClientMsg::Pair { minutes: 5 }).await.unwrap();
        let Some(AgentMsg::Ticket { ticket, .. }) = local.recv().await.unwrap() else {
            panic!("expected a ticket");
        };
        let mut ticket = PairingTicket::decode(&ticket).unwrap();
        ticket.machine = agent.loopback_addr();

        let cdir = tempfile::tempdir().unwrap();
        let controller = Controller::open(ControllerConfig {
            state: StateDir::new(cdir.path()),
            net: NetConfig::local_only(),
            name: Some("mac".into()),
        })
        .await
        .unwrap();
        let pairing = tokio::spawn(async move { controller.pair(&ticket).await.map(|_| ()) });

        let Some(AgentMsg::Approve {
            controller_name, ..
        }) = local.recv().await.unwrap()
        else {
            panic!("expected an approval request");
        };
        assert_eq!(controller_name, "mac");
        local.send(&ClientMsg::Decision { allow }).await.unwrap();
        let outcome = local.recv().await.unwrap();
        (pairing.await.unwrap(), outcome, agent, dir)
    }

    #[tokio::test]
    async fn approving_through_the_channel_pairs() {
        let (result, outcome, agent, _dir) = pair_through_channel(true).await;
        result.unwrap();
        assert!(
            matches!(outcome, Some(AgentMsg::Paired { .. })),
            "{outcome:?}"
        );
        assert_eq!(agent.controllers().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn declining_through_the_channel_refuses() {
        let (result, outcome, agent, _dir) = pair_through_channel(false).await;
        assert!(result.is_err());
        assert!(
            matches!(outcome, Some(AgentMsg::Declined { .. })),
            "{outcome:?}"
        );
        assert!(agent.controllers().unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_one_pairing_window_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let (_agent, state) = start(&dir).await;
        let mut first = connect(&state).await;
        first.send(&ClientMsg::Pair { minutes: 5 }).await.unwrap();
        assert!(matches!(
            first.recv().await.unwrap(),
            Some(AgentMsg::Ticket { .. })
        ));

        let mut second = connect(&state).await;
        second.send(&ClientMsg::Pair { minutes: 5 }).await.unwrap();
        assert!(matches!(
            second.recv().await.unwrap(),
            Some(AgentMsg::Error { .. })
        ));
    }

    #[tokio::test]
    async fn closing_the_window_stops_pairing() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, state) = start(&dir).await;
        let mut local = connect(&state).await;
        local.send(&ClientMsg::Pair { minutes: 5 }).await.unwrap();
        let Some(AgentMsg::Ticket { ticket, .. }) = local.recv().await.unwrap() else {
            panic!("expected a ticket");
        };
        drop(local);
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut ticket = PairingTicket::decode(&ticket).unwrap();
        ticket.machine = agent.loopback_addr();
        let cdir = tempfile::tempdir().unwrap();
        let controller = Controller::open(ControllerConfig {
            state: StateDir::new(cdir.path()),
            net: NetConfig::local_only(),
            name: Some("mac".into()),
        })
        .await
        .unwrap();
        assert!(controller.pair(&ticket).await.is_err());
    }
}
