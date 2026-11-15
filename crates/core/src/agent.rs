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
    read_message, write_frame, write_message,
};
use crate::store::{AgentState, PairedController, StateDir, default_device_name, now_secs};
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

    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
        let inner = inner.clone();
        tokio::spawn(async move {
            let response = handle_request(&inner, &peer, &mut recv).await;
            if let Err(err) = write_message(&mut send, &response).await {
                debug!("could not send response: {err}");
            }
        });
    }

    if let Some(conns) = lock(&inner.connections).get_mut(&peer) {
        conns.retain(|c| c.stable_id() != conn.stable_id());
    }
}

async fn handle_request(
    inner: &Inner,
    peer: &DeviceId,
    recv: &mut iroh::endpoint::RecvStream,
) -> Response {
    match handle_request_inner(inner, peer, recv).await {
        Ok(response) => response,
        Err(err) => {
            debug!(peer = %peer.fmt_short(), "request refused: {err}");
            Response::Error(err)
        }
    }
}

async fn handle_request_inner(
    inner: &Inner,
    peer: &DeviceId,
    recv: &mut iroh::endpoint::RecvStream,
) -> std::result::Result<Response, RemoteError> {
    let request: SignedRequest = read_message(recv)
        .await
        .map_err(|e| RemoteError::new(ErrorCode::BadRequest, e.to_string()))?;
    let body = request.verify(peer, &inner.id, now_ms(), &mut lock(&inner.replay))?;

    // Re-check on every request: the controller may have been revoked mid-connection.
    let controller = inner
        .paired_controller(peer)
        .ok_or_else(|| RemoteError::new(ErrorCode::NotPaired, "this device is not paired"))?;
    if let Some(needed) = body.command.required_permission()
        && !controller.permissions.contains(needed)
    {
        return Err(RemoteError::new(
            ErrorCode::PermissionDenied,
            format!("this device doesn't have the \"{needed}\" permission here"),
        ));
    }

    info!(controller = %controller.name, command = body.command.name(), "request");
    Ok(match body.command {
        Command::Ping => Response::Pong,
        Command::Info => Response::Info(MachineInfo {
            name: inner.machine_name.clone(),
            os: Os::current(),
            agent_version: crate::VERSION.to_string(),
            your_permissions: controller.permissions,
        }),
    })
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
