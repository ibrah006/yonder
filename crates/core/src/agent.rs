//! The agent side: serves paired controllers and handles pairing.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::Os;
use crate::audit::{AuditEntry, AuditLog};
use crate::error::Result;
use crate::identity::{DeviceId, DeviceKey, fingerprint};
use crate::jobs::manager::{JobManager, JobsConfig};
use crate::jobs::{JobId, JobMeta, JobStore};
use crate::net::{self, NetConfig};
use crate::permissions::Permissions;
use crate::protocol::{
    CONTROL_ALPN, Command, ErrorCode, MachineInfo, PAIR_ALPN, PairRejection, PairRequest,
    PairResponse, RemoteError, ReplayGuard, Response, SignedRequest, close_code, now_ms,
    read_frame, read_message, write_frame, write_message,
};
use crate::store::{AgentState, PairedController, StateDir, default_device_name, now_secs};
use crate::terminal::{Session, ShellCommand, TerminalInput, TerminalManager};
use crate::ticket::{PairingTicket, pairing_proof, proof_matches};

/// How long someone at the machine has to approve a pairing request.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Wrong-ticket attempts allowed before pairing closes.
const MAX_PAIRING_FAILURES: u32 = 5;
/// Longest accepted device name; longer names are cut.
const MAX_NAME_CHARS: usize = 64;
/// Largest piece of log sent in one message.
const LOG_CHUNK_BYTES: usize = 64 * 1024;
/// How often a followed log is checked for new output.
const LOG_POLL: Duration = Duration::from_millis(250);

/// Settings for starting an agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub state: StateDir,
    pub net: NetConfig,
    /// Overrides the saved machine name for this run.
    pub name: Option<String>,
    /// How to run jobs. `None` refuses job commands (useful for tests of other features).
    pub jobs: Option<JobsConfig>,
}

/// A controller asking to pair, shown to the person at the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCandidate {
    pub controller_id: DeviceId,
    pub controller_name: String,
    /// Compare with the fingerprint the controller shows.
    pub fingerprint: String,
}

/// Asks the person at the machine whether to allow a pairing.
pub trait PairingApprover: Send + Sync + 'static {
    fn approve(
        &self,
        candidate: PairingCandidate,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

struct FnApprover<F>(F);

impl<F, Fut> PairingApprover for FnApprover<F>
where
    F: Fn(PairingCandidate) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    fn approve(
        &self,
        candidate: PairingCandidate,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin((self.0)(candidate))
    }
}

/// Wraps an async function as a [`PairingApprover`].
pub fn approver_fn<F, Fut>(f: F) -> Arc<dyn PairingApprover>
where
    F: Fn(PairingCandidate) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    Arc::new(FnApprover(f))
}

/// Things that happen on the agent, for UIs and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Paired(PairedController),
    PairingDeclined(PairingCandidate),
    PairingClosed(PairingClosedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingClosedReason {
    Expired,
    TooManyWrongTickets,
}

enum PairingSlot {
    Closed,
    Open {
        secret: [u8; 32],
        expires_at: Instant,
        failures: u32,
    },
    /// A request with the right ticket is waiting for approval; the ticket is used up.
    Awaiting,
}

/// A running agent. Cheap to clone.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("id", &self.inner.id.fmt_short().to_string())
            .finish()
    }
}

struct Inner {
    id: DeviceId,
    endpoint: Endpoint,
    uses_relays: bool,
    machine_name: String,
    /// The controller list lives on disk and is re-read for every check, so a revoke made
    /// by another process (the agent CLI, later the tray app) takes effect immediately.
    state_path: PathBuf,
    /// Serializes read-modify-write of the state file within this process.
    state_write: Mutex<()>,
    pairing: Mutex<PairingSlot>,
    replay: Mutex<ReplayGuard>,
    connections: Mutex<HashMap<DeviceId, Vec<Connection>>>,
    approver: Arc<dyn PairingApprover>,
    events: broadcast::Sender<AgentEvent>,
    jobs: Option<Arc<JobManager>>,
    audit: AuditLog,
    terminals: Arc<TerminalManager>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding one of these locks can't leave the data half-updated in a
    // way that matters more than stopping the agent would, so keep going.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Agent {
    /// Loads (or creates) the machine's key and state and starts accepting connections.
    pub async fn start(config: AgentConfig, approver: Arc<dyn PairingApprover>) -> Result<Agent> {
        let key = DeviceKey::load_or_create(&config.state.key_file())?;
        let state_path = config.state.agent_file();
        let state = AgentState::load(&state_path)?;
        let machine_name = config
            .name
            .clone()
            .or_else(|| (!state.machine_name.is_empty()).then(|| state.machine_name.clone()))
            .unwrap_or_else(default_device_name);
        let endpoint = net::bind(
            &key,
            vec![CONTROL_ALPN.to_vec(), PAIR_ALPN.to_vec()],
            &config.net,
        )
        .await?;
        let (events, _) = broadcast::channel(32);
        let jobs = config
            .jobs
            .clone()
            .map(|jobs| JobManager::start(JobStore::new(config.state.jobs_dir()), jobs));
        let inner = Arc::new(Inner {
            id: key.id(),
            endpoint,
            uses_relays: config.net.uses_relays(),
            machine_name,
            state_path,
            state_write: Mutex::new(()),
            pairing: Mutex::new(PairingSlot::Closed),
            replay: Mutex::new(ReplayGuard::default()),
            connections: Mutex::new(HashMap::new()),
            approver,
            events,
            jobs,
            audit: AuditLog::new(config.state.audit_file()),
            terminals: TerminalManager::new(),
        });
        tokio::spawn(accept_loop(inner.clone()));
        info!(id = %inner.id, name = %inner.machine_name, "agent started");
        Ok(Agent { inner })
    }

    pub fn id(&self) -> DeviceId {
        self.inner.id
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.inner.id)
    }

    pub fn name(&self) -> &str {
        &self.inner.machine_name
    }

    /// Current addresses (relay and direct) others can reach this machine on.
    pub fn addr(&self) -> EndpointAddr {
        self.inner.endpoint.addr()
    }

    /// This machine's address on its own loopback interface.
    pub fn loopback_addr(&self) -> EndpointAddr {
        net::loopback_addr(&self.inner.endpoint)
    }

    /// Paired controllers, read from disk.
    pub fn controllers(&self) -> Result<Vec<PairedController>> {
        Ok(self.inner.load_state()?.controllers)
    }

    /// The job manager, if this agent runs jobs.
    pub fn jobs(&self) -> Option<&Arc<JobManager>> {
        self.inner.jobs.as_ref()
    }

    pub fn audit_log(&self) -> &AuditLog {
        &self.inner.audit
    }

    /// Pauses or resumes new jobs. Running jobs carry on.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        let _write = lock(&self.inner.state_write);
        let mut state = self.inner.load_state()?;
        state.paused = paused;
        state.save(&self.inner.state_path)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.inner.events.subscribe()
    }

    /// Opens pairing for `ttl` and returns the ticket to show. One controller can pair with
    /// it; opening again replaces any earlier ticket.
    pub async fn open_pairing(&self, ttl: Duration) -> PairingTicket {
        if self.inner.uses_relays {
            // Wait briefly for a relay so the ticket works from other networks too.
            let _ =
                tokio::time::timeout(Duration::from_secs(5), self.inner.endpoint.online()).await;
        }
        let secret: [u8; 32] = rand::random();
        *lock(&self.inner.pairing) = PairingSlot::Open {
            secret,
            expires_at: Instant::now() + ttl,
            failures: 0,
        };
        PairingTicket {
            machine: self.addr(),
            secret,
        }
    }

    pub fn close_pairing(&self) {
        *lock(&self.inner.pairing) = PairingSlot::Closed;
    }

    /// Removes a controller and drops its open connections. Returns whether it was paired.
    pub fn revoke(&self, id: &DeviceId) -> Result<bool> {
        let removed = {
            let _write = lock(&self.inner.state_write);
            let mut state = self.inner.load_state()?;
            let removed = state.remove_controller(id);
            if removed {
                state.save(&self.inner.state_path)?;
            }
            removed
        };
        if let Some(conns) = lock(&self.inner.connections).remove(id) {
            for conn in conns {
                conn.close(close_code::REVOKED.into(), b"revoked");
            }
        }
        if removed {
            info!(controller = %id, "controller revoked");
        }
        Ok(removed)
    }

    /// Stops accepting connections and closes open ones.
    pub async fn shutdown(&self) {
        self.inner.endpoint.close().await;
    }
}

impl Inner {
    fn load_state(&self) -> Result<AgentState> {
        AgentState::load(&self.state_path)
    }

    /// Whether the owner paused new jobs. Unreadable state counts as paused.
    fn paused(&self) -> bool {
        self.load_state().map(|s| s.paused).unwrap_or(true)
    }

    fn jobs(&self) -> std::result::Result<&Arc<JobManager>, RemoteError> {
        self.jobs
            .as_ref()
            .ok_or_else(|| RemoteError::new(ErrorCode::Unsupported, "this agent doesn't run jobs"))
    }

    /// The job manager, if the job exists.
    fn job(&self, id: &JobId) -> std::result::Result<&Arc<JobManager>, RemoteError> {
        let jobs = self.jobs()?;
        if jobs.store().exists(id) {
            Ok(jobs)
        } else {
            Err(RemoteError::new(
                ErrorCode::NotFound,
                format!("no job {id}"),
            ))
        }
    }

    /// The paired controller with this id. Fails closed: if the state can't be read,
    /// nobody is treated as paired.
    fn paired_controller(&self, id: &DeviceId) -> Option<PairedController> {
        match self.load_state() {
            Ok(state) => state.controller(id).cloned(),
            Err(err) => {
                warn!("could not read agent state, refusing all controllers: {err}");
                None
            }
        }
    }
}

async fn accept_loop(inner: Arc<Inner>) {
    while let Some(incoming) = inner.endpoint.accept().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(err) => {
                    debug!("incoming connection failed: {err}");
                    return;
                }
            };
            match conn.alpn() {
                CONTROL_ALPN => serve_controller(inner, conn).await,
                PAIR_ALPN => serve_pairing(inner, conn).await,
                other => {
                    warn!(alpn = %String::from_utf8_lossy(other), "unexpected protocol");
                    conn.close(0u32.into(), b"unsupported protocol");
                }
            }
        });
    }
}

async fn serve_controller(inner: Arc<Inner>, conn: Connection) {
    let peer = conn.remote_id();
    if inner.paired_controller(&peer).is_none() {
        debug!(peer = %peer.fmt_short(), "refusing unpaired device");
        conn.close(close_code::NOT_PAIRED.into(), b"not paired");
        return;
    }
    lock(&inner.connections)
        .entry(peer)
        .or_default()
        .push(conn.clone());

    while let Ok((send, recv)) = conn.accept_bi().await {
        let inner = inner.clone();
        tokio::spawn(async move { handle_stream(&inner, peer, send, recv).await });
    }

    if let Some(conns) = lock(&inner.connections).get_mut(&peer) {
        conns.retain(|c| c.stable_id() != conn.stable_id());
    }
}

/// What a request turned into.
enum Reply {
    One(Box<Response>),
    Logs { id: JobId, from: u64, follow: bool },
    Terminal { session: Arc<Session>, from: u64 },
}

/// Handles one request stream: decide, record in the audit log, answer.
async fn handle_stream(inner: &Inner, peer: DeviceId, mut send: SendStream, mut recv: RecvStream) {
    let mut audit = AuditEntry::new(peer, "?", "ok");
    let reply = match decide(inner, &peer, &mut recv, &mut audit).await {
        Ok(reply) => reply,
        Err(err) => {
            debug!(peer = %peer.fmt_short(), "request refused: {err}");
            audit.outcome = format!("{:?}", err.code);
            Reply::One(Box::new(Response::Error(err)))
        }
    };
    if let Err(err) = inner.audit.record(&audit) {
        warn!("could not write the audit log: {err}");
    }
    let sent = match reply {
        Reply::One(response) => write_message(&mut send, &*response).await,
        Reply::Logs { id, from, follow } => stream_logs(inner, &id, from, follow, &mut send).await,
        Reply::Terminal { session, from } => serve_terminal(session, from, send, recv).await,
    };
    if let Err(err) = sent {
        debug!("could not send response: {err}");
    }
}

async fn decide(
    inner: &Inner,
    peer: &DeviceId,
    recv: &mut RecvStream,
    audit: &mut AuditEntry,
) -> std::result::Result<Reply, RemoteError> {
    let request: SignedRequest = read_message(recv)
        .await
        .map_err(|e| RemoteError::new(ErrorCode::BadRequest, e.to_string()))?;
    let body = request.verify(peer, &inner.id, now_ms(), &mut lock(&inner.replay))?;
    audit.command = body.command.name().to_string();
    audit.detail = body.command.detail();
    audit.request = Some(data_encoding::HEXLOWER.encode(&request.body));
    audit.signature = Some(data_encoding::HEXLOWER.encode(&request.signature.to_bytes()));

    // Re-check on every request: the controller may have been revoked mid-connection.
    let controller = inner
        .paired_controller(peer)
        .ok_or_else(|| RemoteError::new(ErrorCode::NotPaired, "this device is not paired"))?;
    audit.controller_name = Some(controller.name.clone());
    if let Some(needed) = body.command.required_permission()
        && !controller.permissions.contains(needed)
    {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            format!("this device doesn't have the \"{needed}\" permission here"),
        ));
    }

    info!(controller = %controller.name, command = body.command.name(), "request");
    let internal = |e: crate::Error| RemoteError::new(ErrorCode::Internal, e.to_string());
    Ok(match body.command {
        Command::Ping => Reply::One(Box::new(Response::Pong)),
        Command::Info => Reply::One(Box::new(Response::Info(MachineInfo {
            name: inner.machine_name.clone(),
            os: Os::current(),
            agent_version: crate::VERSION.to_string(),
            your_permissions: controller.permissions,
            paused: inner.paused(),
        }))),
        Command::RunJob(spec) => {
            let jobs = inner.jobs()?;
            if inner.paused() {
                return Err(RemoteError::new(
                    ErrorCode::Paused,
                    "the owner paused new jobs on this machine",
                ));
            }
            let meta = JobMeta {
                id: JobId::generate(),
                spec,
                controller: *peer,
                controller_name: controller.name,
                created_at_ms: now_ms(),
            };
            audit.detail = Some(format!("{}: {}", meta.id, meta.spec.summary()));
            Reply::One(Box::new(Response::Job(
                jobs.submit(meta).map_err(internal)?,
            )))
        }
        Command::ListJobs => Reply::One(Box::new(Response::Jobs(inner.jobs()?.list()))),
        Command::GetJob(id) => Reply::One(Box::new(Response::Job(
            inner.job(&id)?.store().info(&id).map_err(internal)?,
        ))),
        Command::CancelJob(id) => Reply::One(Box::new(Response::Job(
            inner.job(&id)?.cancel(&id).map_err(internal)?,
        ))),
        Command::JobLogs { id, from, follow } => {
            inner.job(&id)?;
            Reply::Logs { id, from, follow }
        }
        Command::OpenTerminal {
            session,
            cols,
            rows,
            from,
        } => {
            let session = match session {
                None => {
                    let session = inner
                        .terminals
                        .open(&controller.name, cols, rows)
                        .map_err(internal)?;
                    audit.detail = Some(session.id().to_string());
                    session
                }
                Some(id) => {
                    let session = inner.terminals.get(&id).ok_or_else(|| {
                        RemoteError::new(ErrorCode::NotFound, format!("no terminal session {id}"))
                    })?;
                    session.resize(cols, rows);
                    session
                }
            };
            Reply::Terminal {
                session,
                from: from.unwrap_or(0),
            }
        }
        Command::ListTerminals => Reply::One(Box::new(Response::Terminals(inner.terminals.list()))),
        Command::CloseTerminal(id) => {
            let info = inner.terminals.close(&id).ok_or_else(|| {
                RemoteError::new(ErrorCode::NotFound, format!("no terminal session {id}"))
            })?;
            Reply::One(Box::new(Response::Terminal(info)))
        }
    })
}

/// Connects a client to a terminal session until it detaches or the shell exits.
///
/// Keystrokes are read in their own task: reading a frame isn't safe to interrupt, so it
/// can't sit in the same `select!` as output.
async fn serve_terminal(
    session: Arc<Session>,
    from: u64,
    mut send: SendStream,
    mut recv: RecvStream,
) -> Result<()> {
    let _attached = session.attach();
    write_frame(&mut send, &Response::TerminalOpened(session.info())).await?;

    let (detached_tx, mut detached) = tokio::sync::oneshot::channel::<()>();
    let input_session = session.clone();
    let input = tokio::spawn(async move {
        loop {
            match read_frame::<TerminalInput>(&mut recv).await {
                Ok(Some(TerminalInput::Data(data))) => {
                    let s = input_session.clone();
                    // Writing to a PTY can block briefly when the shell is busy.
                    if tokio::task::spawn_blocking(move || s.write(&data))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Some(TerminalInput::Resize { cols, rows })) => input_session.resize(cols, rows),
                Ok(None) | Err(_) => break,
            }
        }
        let _ = detached_tx.send(());
    });

    let mut changes = session.subscribe();
    let mut offset = from;
    let result = loop {
        loop {
            let (start, data) = session.read(offset, LOG_CHUNK_BYTES);
            if data.is_empty() {
                break;
            }
            offset = start + data.len() as u64;
            write_frame(
                &mut send,
                &Response::TerminalOutput {
                    offset: start,
                    data,
                },
            )
            .await?;
        }
        if let Some(code) = session.exit()
            && offset >= session.output_end()
        {
            break write_message(&mut send, &Response::TerminalExit { code }).await;
        }
        tokio::select! {
            _ = changes.changed() => {}
            _ = &mut detached => break Ok(()),
            _ = send.stopped() => break Ok(()),
        }
    };
    input.abort();
    result
}

/// Sends a job's output from `from`, then (with `follow`) new output as it's written,
/// ending with [`Response::LogEnd`] once the job has finished and everything was sent.
async fn stream_logs(
    inner: &Inner,
    id: &JobId,
    mut from: u64,
    follow: bool,
    send: &mut SendStream,
) -> Result<()> {
    let store = match inner.jobs() {
        Ok(jobs) => jobs.store().clone(),
        Err(err) => return write_message(send, &Response::Error(err)).await,
    };
    loop {
        let chunk = {
            let (store, id) = (store.clone(), id.clone());
            tokio::task::spawn_blocking(move || store.read_log(&id, from, LOG_CHUNK_BYTES))
                .await
                .map_err(crate::Error::network)??
        };
        if !chunk.is_empty() {
            let len = chunk.len() as u64;
            write_frame(
                send,
                &Response::LogChunk {
                    offset: from,
                    data: chunk,
                },
            )
            .await?;
            from += len;
            continue;
        }
        let job = store.info(id)?;
        if job.log_bytes > from {
            // Output arrived between the read and the status check.
            continue;
        }
        if job.status.state.is_finished() || !follow {
            return write_message(send, &Response::LogEnd { job }).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(LOG_POLL) => {}
            // The controller went away; it can reattach from where it got to.
            _ = send.stopped() => return Ok(()),
        }
    }
}

async fn serve_pairing(inner: Arc<Inner>, conn: Connection) {
    let peer = conn.remote_id();
    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
        return;
    };
    let response = match read_message::<PairRequest>(&mut recv).await {
        Ok(request) => pair(&inner, peer, request).await,
        Err(err) => {
            debug!("bad pairing request: {err}");
            return;
        }
    };
    if write_message(&mut send, &response).await.is_ok() {
        // Let the controller read the answer before the connection goes away.
        let _ = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    }
}

async fn pair(inner: &Inner, peer: DeviceId, request: PairRequest) -> PairResponse {
    {
        let mut slot = lock(&inner.pairing);
        match &mut *slot {
            PairingSlot::Closed => return PairResponse::Rejected(PairRejection::NotOpen),
            PairingSlot::Awaiting => return PairResponse::Rejected(PairRejection::Busy),
            PairingSlot::Open {
                secret,
                expires_at,
                failures,
            } => {
                if Instant::now() >= *expires_at {
                    *slot = PairingSlot::Closed;
                    let _ = inner
                        .events
                        .send(AgentEvent::PairingClosed(PairingClosedReason::Expired));
                    return PairResponse::Rejected(PairRejection::NotOpen);
                }
                let expected = pairing_proof(secret, &inner.id, &peer);
                if !proof_matches(&expected, &request.proof) {
                    *failures += 1;
                    warn!(peer = %peer.fmt_short(), "pairing attempt with a wrong ticket");
                    if *failures >= MAX_PAIRING_FAILURES {
                        *slot = PairingSlot::Closed;
                        let _ = inner.events.send(AgentEvent::PairingClosed(
                            PairingClosedReason::TooManyWrongTickets,
                        ));
                    }
                    return PairResponse::Rejected(PairRejection::WrongTicket);
                }
                *slot = PairingSlot::Awaiting;
            }
        }
    }

    let candidate = PairingCandidate {
        controller_id: peer,
        controller_name: clean_name(&request.controller_name),
        fingerprint: fingerprint(&peer),
    };
    let approved =
        tokio::time::timeout(APPROVAL_TIMEOUT, inner.approver.approve(candidate.clone()))
            .await
            .unwrap_or(false);
    *lock(&inner.pairing) = PairingSlot::Closed;

    if !approved {
        info!(controller = %candidate.controller_name, "pairing declined");
        let _ = inner.events.send(AgentEvent::PairingDeclined(candidate));
        return PairResponse::Rejected(PairRejection::Declined);
    }

    // Re-pairing resets permissions: anything extra (like admin) must be granted again.
    let controller = PairedController {
        id: peer,
        name: candidate.controller_name,
        permissions: Permissions::for_new_controller(),
        paired_at: now_secs(),
    };
    let saved = {
        let _write = lock(&inner.state_write);
        inner.load_state().and_then(|mut state| {
            state.upsert_controller(controller.clone());
            state.save(&inner.state_path)
        })
    };
    if let Err(err) = saved {
        warn!("could not save pairing: {err}");
        return PairResponse::Rejected(PairRejection::Failed);
    }
    info!(controller = %controller.name, id = %peer, "paired");
    let _ = inner.events.send(AgentEvent::Paired(controller.clone()));
    PairResponse::Accepted {
        machine_name: inner.machine_name.clone(),
        os: Os::current(),
        permissions: controller.permissions,
    }
}

/// Makes a name from another device safe to show: no control characters (which could
/// rewrite the terminal), trimmed, and limited in length.
pub(crate) fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Unnamed device".to_string()
    } else {
        cleaned.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_other_devices_are_made_safe() {
        assert_eq!(clean_name("  Mac\u{1b}[2J Book \n"), "Mac[2J Book");
        assert_eq!(clean_name("\u{7}\u{7}"), "Unnamed device");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }
}
