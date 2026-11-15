//! The controller side: pairs with machines and sends them commands.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iroh::Endpoint;
use iroh::endpoint::{ApplicationClose, Connection, ConnectionError};

use crate::agent::{APPROVAL_TIMEOUT, clean_name};
use crate::error::{Error, Result};
use crate::identity::{DeviceId, DeviceKey, fingerprint};
use crate::jobs::{JobId, JobInfo, JobSpec};
use crate::net::{self, NetConfig};
use crate::protocol::{
    CONTROL_ALPN, Command, ErrorCode, MachineInfo, PAIR_ALPN, PairRequest, PairResponse,
    RemoteError, Response, SignedRequest, close_code, read_frame, read_message, write_message,
};
use crate::store::{ControllerState, PairedMachine, StateDir, default_device_name, now_secs};
use crate::ticket::{PairingTicket, pairing_proof};

/// How long to try reaching a machine before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for an answer to a request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Settings for opening a controller.
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub state: StateDir,
    pub net: NetConfig,
    /// Overrides the saved device name for this run.
    pub name: Option<String>,
}

/// This device acting as a controller of paired machines.
pub struct Controller {
    key: DeviceKey,
    endpoint: Endpoint,
    name: String,
    state_path: PathBuf,
    state: Mutex<ControllerState>,
    connections: tokio::sync::Mutex<HashMap<DeviceId, Connection>>,
}

impl std::fmt::Debug for Controller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller")
            .field("id", &self.key.id().fmt_short().to_string())
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Controller {
    /// Loads (or creates) this device's key and machine list, and starts networking.
    pub async fn open(config: ControllerConfig) -> Result<Controller> {
        let key = DeviceKey::load_or_create(&config.state.key_file())?;
        let state_path = config.state.controller_file();
        let state = ControllerState::load(&state_path)?;
        let name = config
            .name
            .clone()
            .or_else(|| (!state.device_name.is_empty()).then(|| state.device_name.clone()))
            .unwrap_or_else(default_device_name);
        // Controllers only dial out; they accept no protocols.
        let endpoint = net::bind(&key, Vec::new(), &config.net).await?;
        Ok(Controller {
            key,
            endpoint,
            name: clean_name(&name),
            state_path,
            state: Mutex::new(state),
            connections: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn id(&self) -> DeviceId {
        self.key.id()
    }

    /// Shown here while pairing; the machine shows the same code to confirm.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key.id())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn machines(&self) -> Vec<PairedMachine> {
        lock(&self.state).machines.clone()
    }

    /// Finds a paired machine by name or id prefix.
    pub fn find_machine(&self, query: &str) -> Result<PairedMachine> {
        lock(&self.state).find_machine(query).cloned()
    }

    /// Pairs with the machine that showed `ticket`. Waits while the person at the machine
    /// approves, so this can take up to a couple of minutes.
    pub async fn pair(&self, ticket: &PairingTicket) -> Result<PairedMachine> {
        let machine_id = ticket.machine_id();
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(ticket.machine.clone(), PAIR_ALPN),
        )
        .await
        .map_err(|_| Error::Timeout("couldn't reach the machine"))?
        .map_err(Error::network)?;

        let request = PairRequest {
            controller_name: self.name.clone(),
            proof: pairing_proof(&ticket.secret, &machine_id, &self.key.id()),
        };
        let (mut send, mut recv) = conn.open_bi().await.map_err(Error::network)?;
        write_message(&mut send, &request).await?;
        let response: PairResponse = tokio::time::timeout(
            APPROVAL_TIMEOUT + Duration::from_secs(30),
            read_message(&mut recv),
        )
        .await
        .map_err(|_| Error::Timeout("no answer from the machine"))??;
        conn.close(0u32.into(), b"done");

        match response {
            PairResponse::Accepted {
                machine_name,
                os,
                permissions,
            } => {
                let machine = PairedMachine {
                    id: machine_id,
                    name: clean_name(&machine_name),
                    os,
                    addr: ticket.machine.clone(),
                    permissions,
                    paired_at: now_secs(),
                };
                let mut state = lock(&self.state);
                state.upsert_machine(machine.clone());
                state.save(&self.state_path)?;
                Ok(machine)
            }
            PairResponse::Rejected(reason) => Err(Error::PairingRejected(reason)),
        }
    }

    /// Removes a machine from this device's list. The machine still lists this device until
    /// it's revoked there.
    pub fn forget(&self, id: &DeviceId) -> Result<bool> {
        let mut state = lock(&self.state);
        let removed = state.remove_machine(id);
        if removed {
            state.save(&self.state_path)?;
        }
        Ok(removed)
    }

    /// Sends a command and returns the machine's answer. Refusals become [`Error::Remote`].
    pub async fn request(&self, machine: &PairedMachine, command: Command) -> Result<Response> {
        let conn = self.connection(machine).await?;
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let (mut send, mut recv) = conn.open_bi().await.map_err(Error::network)?;
            write_message(&mut send, &signed).await?;
            read_message::<Response>(&mut recv).await
        })
        .await
        .map_err(|_| Error::Timeout("no answer from the machine"))
        .and_then(|r| r);

        match result {
            Ok(Response::Error(err)) => Err(Error::Remote(err)),
            Ok(response) => Ok(response),
            Err(err) => {
                self.connections.lock().await.remove(&machine.id);
                Err(explain_close(&conn).unwrap_or(err))
            }
        }
    }

    /// Round-trip time to the machine.
    pub async fn ping(&self, machine: &PairedMachine) -> Result<Duration> {
        let started = Instant::now();
        match self.request(machine, Command::Ping).await? {
            Response::Pong => Ok(started.elapsed()),
            other => Err(unexpected("pong", &other)),
        }
    }

    pub async fn info(&self, machine: &PairedMachine) -> Result<MachineInfo> {
        match self.request(machine, Command::Info).await? {
            Response::Info(info) => Ok(info),
            other => Err(unexpected("info", &other)),
        }
    }

    /// Starts a job. It runs on the machine whether or not this device stays connected.
    pub async fn run(&self, machine: &PairedMachine, spec: JobSpec) -> Result<JobInfo> {
        expect_job(self.request(machine, Command::RunJob(spec)).await?)
    }

    /// All jobs on the machine, newest first.
    pub async fn jobs(&self, machine: &PairedMachine) -> Result<Vec<JobInfo>> {
        match self.request(machine, Command::ListJobs).await? {
            Response::Jobs(jobs) => Ok(jobs),
            other => Err(unexpected("jobs", &other)),
        }
    }

    pub async fn job(&self, machine: &PairedMachine, id: &JobId) -> Result<JobInfo> {
        expect_job(self.request(machine, Command::GetJob(id.clone())).await?)
    }

    /// Stops a queued or running job.
    pub async fn cancel(&self, machine: &PairedMachine, id: &JobId) -> Result<JobInfo> {
        expect_job(
            self.request(machine, Command::CancelJob(id.clone()))
                .await?,
        )
    }

    /// Reads a job's output from byte `from`, calling `on_chunk(offset, bytes)` for each
    /// piece. With `follow`, keeps going until the job finishes. Returns the job as it was
    /// when the stream ended.
    ///
    /// If the connection drops, the error comes back and the caller can call again with
    /// `from` set to the end of the last chunk it received: nothing is lost or repeated.
    pub async fn logs(
        &self,
        machine: &PairedMachine,
        id: &JobId,
        from: u64,
        follow: bool,
        mut on_chunk: impl FnMut(u64, &[u8]),
    ) -> Result<JobInfo> {
        let conn = self.connection(machine).await?;
        let command = Command::JobLogs {
            id: id.clone(),
            from,
            follow,
        };
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let result: Result<JobInfo> = async {
            let (mut send, mut recv) = conn.open_bi().await.map_err(Error::network)?;
            write_message(&mut send, &signed).await?;
            loop {
                match read_frame::<Response>(&mut recv).await? {
                    Some(Response::LogChunk { offset, data }) => on_chunk(offset, &data),
                    Some(Response::LogEnd { job }) => return Ok(job),
                    Some(Response::Error(err)) => return Err(Error::Remote(err)),
                    Some(other) => return Err(unexpected("log output", &other)),
                    None => {
                        return Err(Error::Protocol(
                            "the log stream ended early; try again to resume".into(),
                        ));
                    }
                }
            }
        }
        .await;
        if result.is_err() {
            self.connections.lock().await.remove(&machine.id);
            if let Some(err) = explain_close(&conn) {
                return Err(err);
            }
        }
        result
    }

    /// Closes all connections.
    pub async fn close(&self) {
        self.connections.lock().await.clear();
        self.endpoint.close().await;
    }

    async fn connection(&self, machine: &PairedMachine) -> Result<Connection> {
        let mut connections = self.connections.lock().await;
        if let Some(conn) = connections.get(&machine.id)
            && conn.close_reason().is_none()
        {
            return Ok(conn.clone());
        }
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint.connect(machine.addr.clone(), CONTROL_ALPN),
        )
        .await
        .map_err(|_| Error::Timeout("couldn't reach the machine; is it on and online?"))?
        .map_err(Error::network)?;
        connections.insert(machine.id, conn.clone());
        Ok(conn)
    }
}

fn unexpected(wanted: &str, got: &Response) -> Error {
    Error::Protocol(format!("expected {wanted}, got {got:?}"))
}

fn expect_job(response: Response) -> Result<JobInfo> {
    match response {
        Response::Job(job) => Ok(job),
        other => Err(unexpected("a job", &other)),
    }
}

/// Turns the agent closing the connection on us into a clear error.
fn explain_close(conn: &Connection) -> Option<Error> {
    let Some(ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. })) =
        conn.close_reason()
    else {
        return None;
    };
    let (code, message) = match u32::try_from(error_code.into_inner()).ok()? {
        close_code::NOT_PAIRED => (
            ErrorCode::NotPaired,
            "this device isn't paired with that machine (it was revoked or the machine was reset); pair again",
        ),
        close_code::REVOKED => (
            ErrorCode::NotPaired,
            "this device was revoked on that machine",
        ),
        close_code::SHUTTING_DOWN => (
            ErrorCode::Internal,
            "the agent on that machine is shutting down",
        ),
        _ => return None,
    };
    Some(Error::Remote(RemoteError::new(code, message)))
}
