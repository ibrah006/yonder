//! The agent side: serves paired controllers and handles pairing.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::Os;
use crate::audit::{AuditEntry, AuditLog};
use crate::credentials;
use crate::error::Result;
use crate::footprint::{
    DeviceInfo, DiskEncryption, Footprint, FootprintItem, Removed, disk_encryption,
};
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
use crate::secrets::{SecretStore, UseError};
use crate::stats::StatsSampler;
use crate::store::{AgentState, PairedController, StateDir, default_device_name, now_secs};
use crate::terminal::{Session, ShellCommand, TerminalInput, TerminalManager};
use crate::ticket::{PairingTicket, pairing_proof, proof_matches};
use crate::workspace::{WorkspaceId, WorkspaceInfo, Workspaces};

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
    /// How to start shells. `None`: the machine's usual shell, as the agent's own user.
    pub shell: Option<ShellCommand>,
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
    workspaces: Arc<Workspaces>,
    secrets: SecretStore,
    stats: Arc<StatsSampler>,
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
            terminals: TerminalManager::with_shell(config.shell.clone()),
            workspaces: Arc::new(Workspaces::new(
                config.state.workspaces_dir(),
                config.state.sync_dir(),
            )),
            secrets: SecretStore::new(config.state.secrets_dir()),
            stats: Arc::new(StatsSampler::default()),
        });
        tokio::spawn(accept_loop(inner.clone()));
        tokio::spawn(cleanup_loop(Arc::downgrade(&inner)));
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
        self.inner.revoke(id, true)
    }

    /// Runs auto-cleanup as if it were `now_ms` (the agent also does this every hour).
    /// Returns how many project copies were removed.
    pub fn clean_up_unused(&self, now_ms: u64) -> usize {
        self.inner.clean_up_unused(now_ms)
    }

    /// Stops accepting connections and closes open ones.
    pub async fn shutdown(&self) {
        self.inner.endpoint.close().await;
    }
}

impl Inner {
    /// Unpairs a controller; with `disconnect`, also closes its open connections.
    fn revoke(&self, id: &DeviceId, disconnect: bool) -> Result<bool> {
        let removed = {
            let _write = lock(&self.state_write);
            let mut state = self.load_state()?;
            let removed = state.remove_controller(id);
            if removed {
                state.save(&self.state_path)?;
            }
            removed
        };
        if disconnect && let Some(conns) = lock(&self.connections).remove(id) {
            for conn in conns {
                conn.close(close_code::REVOKED.into(), b"revoked");
            }
        }
        if removed {
            info!(controller = %id, "controller revoked");
        }
        Ok(removed)
    }

    /// What `controller` has left here.
    fn footprint(&self, controller: &PairedController) -> Result<Footprint> {
        let id = &controller.id;
        let jobs: Vec<_> = self
            .jobs
            .as_ref()
            .map(|j| j.list())
            .unwrap_or_default()
            .into_iter()
            .filter(|j| &j.meta.controller == id)
            .collect();
        Ok(Footprint {
            paired_at: controller.paired_at,
            permissions: controller.permissions.clone(),
            secrets: self.secrets.list(id)?,
            workspaces: self.workspaces.owned_by(id),
            unfinished_jobs: jobs
                .iter()
                .filter(|j| !j.status.state.is_finished())
                .count(),
            job_bytes: jobs.iter().map(|j| j.log_bytes).sum(),
            jobs: jobs.len(),
            shells: self
                .terminals
                .list()
                .into_iter()
                .filter(|t| &t.owner == id)
                .collect(),
            cleanup_after_days: controller.cleanup_after_days,
            disk_encryption: DiskEncryption::Unknown,
        })
    }

    /// When a project copy was last used: synced, or a job started in it.
    fn last_used_ms(&self, workspace: &WorkspaceInfo) -> u64 {
        let dir = self.workspaces.dir(&workspace.id);
        let last_job = self
            .jobs
            .as_ref()
            .map(|j| j.list())
            .unwrap_or_default()
            .into_iter()
            .filter(|j| {
                j.meta
                    .spec
                    .cwd
                    .as_deref()
                    .is_some_and(|cwd| Path::new(cwd).starts_with(&dir))
            })
            .map(|j| j.meta.created_at_ms)
            .max();
        workspace
            .last_synced_ms
            .into_iter()
            .chain(last_job)
            .max()
            .unwrap_or(0)
    }

    /// Removes project copies unused for longer than their owner's auto-cleanup allows.
    /// Returns how many went.
    fn clean_up_unused(&self, now_ms: u64) -> usize {
        let Ok(state) = self.load_state() else {
            return 0;
        };
        let mut removed = 0;
        for controller in &state.controllers {
            let Some(days) = controller.cleanup_after_days else {
                continue;
            };
            let limit = u64::from(days) * 86_400_000;
            for workspace in self.workspaces.owned_by(&controller.id) {
                let idle = now_ms.saturating_sub(self.last_used_ms(&workspace));
                if idle <= limit || self.job_running_in(&self.workspaces.dir(&workspace.id)) {
                    continue;
                }
                match self.workspaces.remove(&workspace.id) {
                    Ok(bytes) => {
                        removed += 1;
                        info!(
                            controller = %controller.name,
                            workspace = %workspace.id,
                            bytes,
                            "auto-cleanup removed an unused project copy"
                        );
                    }
                    Err(err) => warn!(workspace = %workspace.id, "auto-cleanup failed: {err}"),
                }
            }
        }
        removed
    }

    /// Whether an unfinished job (anyone's) runs inside `dir`.
    fn job_running_in(&self, dir: &Path) -> bool {
        self.jobs.as_ref().is_some_and(|jobs| {
            jobs.list().iter().any(|job| {
                !job.status.state.is_finished()
                    && job
                        .meta
                        .spec
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| Path::new(cwd).starts_with(dir))
            })
        })
    }

    /// Removes part or all of `id`'s footprint.
    fn remove_footprint(&self, id: &DeviceId, item: &FootprintItem) -> Result<Removed> {
        let mut removed = Removed::default();
        let everything = matches!(item, FootprintItem::Everything);
        if everything || matches!(item, FootprintItem::Shells) {
            for shell in self.terminals.list().into_iter().filter(|t| &t.owner == id) {
                if self.terminals.close(&shell.id).is_some() {
                    removed.shells += 1;
                }
            }
        }
        if (everything || matches!(item, FootprintItem::Jobs))
            && let Some(jobs) = &self.jobs
        {
            let mut cancelled = 0;
            for job in jobs.list().into_iter().filter(|j| &j.meta.controller == id) {
                if job.status.state.is_finished() {
                    if jobs.store().remove(&job.meta.id).is_ok() {
                        removed.jobs += 1;
                        removed.bytes += job.log_bytes;
                    }
                } else {
                    let _ = jobs.cancel(&job.meta.id);
                    cancelled += 1;
                }
            }
            if cancelled > 0 {
                removed.kept.push(format!(
                    "{cancelled} unfinished jobs were cancelled; remove them again once they stop"
                ));
            }
        }
        match item {
            FootprintItem::Secret(name) => {
                if self.secrets.remove(id, name)? {
                    removed.secrets += 1;
                }
            }
            FootprintItem::Everything => removed.secrets += self.secrets.remove_all(id)?,
            _ => {}
        }
        let workspaces: Vec<WorkspaceId> = match item {
            FootprintItem::Workspace(ws) => self
                .workspaces
                .owned_by(id)
                .into_iter()
                .filter(|w| &w.id == ws)
                .map(|w| w.id)
                .collect(),
            FootprintItem::Everything => self
                .workspaces
                .owned_by(id)
                .into_iter()
                .map(|w| w.id)
                .collect(),
            _ => Vec::new(),
        };
        for ws in workspaces {
            if self.job_running_in(&self.workspaces.dir(&ws)) {
                removed
                    .kept
                    .push(format!("{ws}: a job is still running in it"));
                continue;
            }
            removed.bytes += self.workspaces.remove(&ws)?;
            removed.workspaces += 1;
        }
        info!(controller = %id, ?removed, "footprint removed");
        Ok(removed)
    }

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
        Reply::Logs { id, from, follow } => {
            stream_logs(inner, &peer, &id, from, follow, &mut send).await
        }
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
    // A project copy belongs to the device that first synced it.
    if let Some(ws) = body.command.workspace()
        && !inner.workspaces.claim(ws, peer).map_err(internal)?
    {
        return Err(RemoteError::new(
            ErrorCode::NotFound,
            format!("no project copy {ws} from this device here"),
        ));
    }
    Ok(match body.command {
        Command::Ping => Reply::One(Box::new(Response::Pong)),
        Command::Info => {
            let (sampler, dir) = (inner.stats.clone(), inner.workspaces.dir_root());
            let stats = tokio::task::spawn_blocking(move || sampler.sample(&dir))
                .await
                .ok()
                .flatten();
            Reply::One(Box::new(Response::Info(MachineInfo {
                name: inner.machine_name.clone(),
                os: Os::current(),
                agent_version: crate::VERSION.to_string(),
                your_permissions: controller.permissions,
                paused: inner.paused(),
                stats,
            })))
        }
        Command::RunJob(spec) => {
            let jobs = inner.jobs()?;
            if inner.paused() {
                return Err(RemoteError::new(
                    ErrorCode::Paused,
                    "the owner paused new jobs on this machine",
                ));
            }
            // Environment values don't belong in the audit log.
            if !spec.env.is_empty() {
                audit.request = None;
            }
            let secrets = inner
                .secrets
                .resolve(peer, &spec.secrets)
                .map_err(internal)?
                .map_err(|e| {
                    let code = match e {
                        UseError::BadTarget(_) => ErrorCode::BadRequest,
                        UseError::Missing(_) | UseError::Expired(_) => ErrorCode::NotFound,
                    };
                    RemoteError::new(code, e.to_string())
                })?;
            let meta = JobMeta {
                id: JobId::generate(),
                spec,
                controller: *peer,
                controller_name: controller.name,
                created_at_ms: now_ms(),
            };
            audit.detail = Some(format!("{}: {}", meta.id, meta.spec.summary()));
            Reply::One(Box::new(Response::Job(
                jobs.submit(meta, &secrets).map_err(internal)?,
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
                        .open(*peer, &controller.name, cols, rows)
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
        Command::SyncPlan {
            workspace,
            files,
            first,
        } => {
            let needed =
                blocking(&inner.workspaces, move |w| w.plan(&workspace, files, first)).await?;
            Reply::One(Box::new(Response::SyncNeeded(needed)))
        }
        Command::SyncPut { workspace, parts } => {
            // File contents don't belong in the audit log.
            audit.request = None;
            blocking(&inner.workspaces, move |w| w.put(&workspace, parts)).await?;
            Reply::One(Box::new(Response::Done))
        }
        Command::SyncApply { workspace } => {
            if inner.job_running_in(&inner.workspaces.dir(&workspace)) {
                return Err(RemoteError::new(
                    ErrorCode::Busy,
                    "a job is still running in this workspace; wait for it or cancel it",
                ));
            }
            let dir = blocking(&inner.workspaces, move |w| w.apply(&workspace)).await?;
            Reply::One(Box::new(Response::Workspace {
                path: dir.to_string_lossy().into_owned(),
            }))
        }
        Command::ListFiles {
            workspace,
            patterns,
        } => {
            let files = blocking(&inner.workspaces, move |w| w.list(&workspace, &patterns)).await?;
            Reply::One(Box::new(Response::Files(files)))
        }
        Command::ReadFile {
            workspace,
            path,
            offset,
        } => {
            audit.request = None;
            let data = blocking(&inner.workspaces, move |w| {
                w.read(&workspace, &path, offset)
            })
            .await?;
            Reply::One(Box::new(Response::FileData(data)))
        }
        Command::SetSecret {
            name,
            value,
            expires_at_ms,
            note,
        } => {
            // The value must never reach the audit log.
            audit.request = None;
            let info = inner
                .secrets
                .set(peer, name, value, expires_at_ms, note)
                .map_err(|e| RemoteError::new(ErrorCode::BadRequest, e.to_string()))?;
            Reply::One(Box::new(Response::Secret(info)))
        }
        Command::ListSecrets => Reply::One(Box::new(Response::Secrets(
            inner.secrets.list(peer).map_err(internal)?,
        ))),
        Command::Footprint => {
            let mut footprint = inner.footprint(&controller).map_err(internal)?;
            footprint.disk_encryption = tokio::task::spawn_blocking(disk_encryption)
                .await
                .unwrap_or(DiskEncryption::Unknown);
            Reply::One(Box::new(Response::Footprint(footprint)))
        }
        Command::SetCleanup { after_days } => {
            if after_days == Some(0) {
                return Err(RemoteError::new(
                    ErrorCode::BadRequest,
                    "auto-cleanup needs at least one day",
                ));
            }
            {
                let _write = lock(&inner.state_write);
                let mut state = inner.load_state().map_err(internal)?;
                if let Some(c) = state.controllers.iter_mut().find(|c| &c.id == peer) {
                    c.cleanup_after_days = after_days;
                }
                state.save(&inner.state_path).map_err(internal)?;
            }
            Reply::One(Box::new(Response::Done))
        }
        Command::RemoveFootprint(item) => {
            if let FootprintItem::Workspace(ws) = &item
                && !inner.workspaces.owned_by(peer).iter().any(|w| &w.id == ws)
            {
                return Err(RemoteError::new(
                    ErrorCode::NotFound,
                    format!("no project copy {ws} from this device here"),
                ));
            }
            Reply::One(Box::new(Response::Removed(
                inner.remove_footprint(peer, &item).map_err(internal)?,
            )))
        }
        Command::Unpair { wipe } => {
            let removed = if wipe {
                inner
                    .remove_footprint(peer, &FootprintItem::Everything)
                    .map_err(internal)?
            } else {
                Removed::default()
            };
            // This connection stays open long enough to answer; requests after this
            // are refused.
            inner.revoke(peer, false).map_err(internal)?;
            Reply::One(Box::new(Response::Removed(removed)))
        }
        Command::ListDevices => {
            let state = inner.load_state().map_err(internal)?;
            let devices = state
                .controllers
                .iter()
                .map(|c| DeviceInfo {
                    id: c.id,
                    name: c.name.clone(),
                    permissions: c.permissions.clone(),
                    paired_at: c.paired_at,
                    you: &c.id == peer,
                })
                .collect();
            Reply::One(Box::new(Response::Devices(devices)))
        }
        Command::RevokeDevice { id, wipe } => {
            if &id == peer {
                return Err(RemoteError::new(
                    ErrorCode::BadRequest,
                    "that's this device; use `yonder unpair` instead",
                ));
            }
            if inner.paired_controller(&id).is_none() {
                return Err(RemoteError::new(
                    ErrorCode::NotFound,
                    format!("no device {} is paired here", id.fmt_short()),
                ));
            }
            // Unpair first, so the lost device can't add anything while it's wiped.
            inner.revoke(&id, true).map_err(internal)?;
            let removed = if wipe {
                inner
                    .remove_footprint(&id, &FootprintItem::Everything)
                    .map_err(internal)?
            } else {
                Removed::default()
            };
            Reply::One(Box::new(Response::Removed(removed)))
        }
        Command::AnswerGitCredential {
            job,
            request,
            credential,
        } => {
            // The login must never reach the audit log.
            audit.request = None;
            let jobs = inner.job(&job)?;
            let info = jobs.store().info(&job).map_err(internal)?;
            if &info.meta.controller != peer || !info.meta.spec.borrow_git {
                return Err(RemoteError::new(
                    ErrorCode::PermissionDenied,
                    "only the device that started this job can lend it git logins",
                ));
            }
            let dir = jobs.store().job_dir(&job);
            if !credentials::answer(&dir, &request, credential).map_err(internal)? {
                return Err(RemoteError::new(
                    ErrorCode::NotFound,
                    "that git request is no longer waiting",
                ));
            }
            Reply::One(Box::new(Response::Done))
        }
        Command::RemoveSecret(name) => {
            if !inner.secrets.remove(peer, &name).map_err(internal)? {
                return Err(RemoteError::new(
                    ErrorCode::NotFound,
                    format!("no secret {name} from this device here"),
                ));
            }
            Reply::One(Box::new(Response::Done))
        }
    })
}

/// Runs file work off the async threads.
async fn blocking<T: Send + 'static>(
    workspaces: &Arc<Workspaces>,
    work: impl FnOnce(&Workspaces) -> Result<T> + Send + 'static,
) -> std::result::Result<T, RemoteError> {
    let workspaces = workspaces.clone();
    tokio::task::spawn_blocking(move || work(&workspaces))
        .await
        .map_err(|e| RemoteError::new(ErrorCode::Internal, e.to_string()))?
        .map_err(|e| RemoteError::new(ErrorCode::Internal, e.to_string()))
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
    peer: &DeviceId,
    id: &JobId,
    mut from: u64,
    follow: bool,
    send: &mut SendStream,
) -> Result<()> {
    let store = match inner.jobs() {
        Ok(jobs) => jobs.store().clone(),
        Err(err) => return write_message(send, &Response::Error(err)).await,
    };
    // Borrowed-git requests already passed on in this stream.
    let mut asked = std::collections::HashSet::new();
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
        // Only the device that started the job lends it logins.
        if job.meta.spec.borrow_git && &job.meta.controller == peer {
            for request in credentials::pending(&store.job_dir(id)) {
                if asked.insert(request.id.clone()) {
                    write_frame(send, &Response::GitCredentialRequest(request)).await?;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(LOG_POLL) => {}
            // The controller went away; it can reattach from where it got to.
            _ = send.stopped() => return Ok(()),
        }
    }
}

/// Auto-cleanup: shortly after starting, then hourly, while the agent runs.
async fn cleanup_loop(inner: std::sync::Weak<Inner>) {
    let mut wait = Duration::from_secs(60);
    loop {
        tokio::time::sleep(wait).await;
        wait = Duration::from_secs(3600);
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || inner.clean_up_unused(now_ms())).await;
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
        cleanup_after_days: None,
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
