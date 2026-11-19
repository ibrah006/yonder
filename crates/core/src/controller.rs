//! The controller side: pairs with machines and sends them commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use iroh::Endpoint;
use iroh::endpoint::{ApplicationClose, Connection, ConnectionError};
#[cfg(unix)]
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::Os;
use crate::agent::{APPROVAL_TIMEOUT, clean_name};
use crate::credentials::{GitCredential, GitCredentialRequest};
use crate::error::{Error, Result};
use crate::footprint::{DeviceInfo, Footprint, FootprintItem, Removed};
use crate::identity::{DeviceId, DeviceKey, fingerprint};
use crate::jobs::{JobId, JobInfo, JobSpec};
use crate::net::{self, NetConfig};
use crate::protocol::{
    CONTROL_ALPN, Command, ErrorCode, MachineInfo, PAIR_ALPN, PairRequest, PairResponse,
    RemoteError, Response, SignedRequest, close_code, read_frame, read_message, write_frame,
    write_message,
};
use crate::secrets::{SecretInfo, SecretName};
use crate::store::{ControllerState, PairedMachine, StateDir, default_device_name, now_secs};
use crate::terminal::{SessionId, TerminalInfo, TerminalInput};
use crate::ticket::{PairingTicket, pairing_proof};
use crate::workspace::{
    BlobPart, CHUNK_BYTES, ENTRIES_PER_MESSAGE, FileEntry, Hash, LocalProject, WorkspaceId,
};

/// How long to try reaching a machine before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for an answer to a request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for requests that hash or copy many files on the machine.
pub const FILE_WORK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a machine gets to answer when choosing between several.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Settings for opening a controller.
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub state: StateDir,
    pub net: NetConfig,
    /// Overrides the saved device name for this run.
    pub name: Option<String>,
}

/// The sending half of a stream to a machine: direct, or through the app.
pub type StreamSend = Box<dyn AsyncWrite + Send + Unpin>;
/// The receiving half of a stream to a machine.
pub type StreamRecv = Box<dyn AsyncRead + Send + Unpin>;

/// Set to use this device's own connection even while the app hosts one.
pub const DIRECT_ENV: &str = "YONDER_DIRECT";

/// This device acting as a controller of paired machines.
///
/// One device has one key, so only one process should hold its connections: while the
/// Yonder app is open it *hosts* them ([`Controller::host`]) on a socket in the state
/// folder, and other processes (the CLI) open their streams through it. When nothing
/// hosts, each process connects on its own.
pub struct Controller {
    key: DeviceKey,
    net: NetConfig,
    /// Bound on first use, so a CLI going through the app never goes online itself.
    endpoint: tokio::sync::OnceCell<Endpoint>,
    name: String,
    state_path: PathBuf,
    state: Mutex<ControllerState>,
    connections: tokio::sync::Mutex<HashMap<DeviceId, Connection>>,
    /// Where the app hosts connections (macOS and Linux; Windows always connects directly).
    #[cfg_attr(not(unix), allow(dead_code))]
    host_socket: PathBuf,
    /// This process is the host.
    #[cfg_attr(not(unix), allow(dead_code))]
    hosting: std::sync::atomic::AtomicBool,
    /// While hosting: why a machine closed a connection, for the process whose stream
    /// broke to ask.
    #[cfg_attr(not(unix), allow(dead_code))]
    closed: Mutex<HashMap<DeviceId, RemoteError>>,
}

/// First message on a hosted stream: which machine to reach.
#[cfg(unix)]
#[derive(Serialize, Deserialize)]
struct HostOpen {
    machine: DeviceId,
    /// Not a new stream: asks why the last one to this machine broke.
    #[serde(default)]
    why: bool,
}

/// The host's answer before the stream carries the request itself.
#[cfg(unix)]
type HostReply = std::result::Result<(), RemoteError>;

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
        Ok(Controller {
            key,
            net: config.net,
            endpoint: tokio::sync::OnceCell::new(),
            name: clean_name(&name),
            state_path,
            state: Mutex::new(state),
            connections: tokio::sync::Mutex::new(HashMap::new()),
            host_socket: config.state.host_socket(),
            hosting: std::sync::atomic::AtomicBool::new(false),
            closed: Mutex::new(HashMap::new()),
        })
    }

    /// This device's network endpoint, started on first use.
    async fn endpoint(&self) -> Result<&Endpoint> {
        // Controllers only dial out; they accept no protocols.
        self.endpoint
            .get_or_try_init(|| net::bind(&self.key, Vec::new(), &self.net))
            .await
    }

    /// Whether this process has its own network connection (it doesn't while going
    /// through the app).
    pub fn is_online(&self) -> bool {
        self.endpoint.get().is_some()
    }

    /// Reads the machine list again (another process may have paired or forgotten one).
    pub fn reload(&self) -> Result<()> {
        let state = ControllerState::load(&self.state_path)?;
        *lock(&self.state) = state;
        Ok(())
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

    /// Resolves what the user typed as a machine: a name or id prefix first; otherwise an
    /// OS (`win`, `mac`, `linux`) or `any`, choosing the reachable, unpaused machine with
    /// the fewest jobs.
    pub async fn pick_machine(&self, query: &str) -> Result<PairedMachine> {
        match self.find_machine(query) {
            Err(Error::UnknownMachine(_)) => {}
            other => return other,
        }
        let os = if query.trim().eq_ignore_ascii_case("any") {
            None
        } else {
            Some(Os::parse(query.trim()).ok_or_else(|| Error::UnknownMachine(query.to_string()))?)
        };
        let what = os.map_or("".to_string(), |os| format!("{os} "));
        let candidates: Vec<PairedMachine> = self
            .machines()
            .into_iter()
            .filter(|m| os.is_none_or(|os| m.os == os))
            .collect();
        match candidates.as_slice() {
            [] => {
                return Err(Error::UnknownMachine(format!(
                    "{query} (no paired {what}machine)"
                )));
            }
            [only] => return Ok(only.clone()),
            _ => {}
        }
        let probes = candidates.iter().map(|machine| async move {
            let probe = async {
                let info = self.info(machine).await?;
                let jobs = self.jobs(machine).await.unwrap_or_default();
                let busy = jobs
                    .iter()
                    .filter(|j| !j.status.state.is_finished())
                    .count();
                Ok::<_, Error>((info.paused, busy))
            };
            tokio::time::timeout(PROBE_TIMEOUT, probe)
                .await
                .ok()
                .and_then(|r| r.ok())
                .map(|score| (score, machine.clone()))
        });
        futures_util::future::join_all(probes)
            .await
            .into_iter()
            .flatten()
            .min_by_key(|(score, _)| *score)
            .map(|(_, machine)| machine)
            .ok_or(Error::Timeout(
                "none of those machines answered; are they on and online?",
            ))
    }

    /// Pairs with the machine that showed `ticket`. Waits while the person at the machine
    /// approves, so this can take up to a couple of minutes.
    pub async fn pair(&self, ticket: &PairingTicket) -> Result<PairedMachine> {
        let machine_id = ticket.machine_id();
        let conn = tokio::time::timeout(
            CONNECT_TIMEOUT,
            self.endpoint()
                .await?
                .connect(ticket.machine.clone(), PAIR_ALPN),
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
        self.request_within(machine, command, REQUEST_TIMEOUT).await
    }

    /// [`Controller::request`] with its own time limit.
    pub async fn request_within(
        &self,
        machine: &PairedMachine,
        command: Command,
        timeout: Duration,
    ) -> Result<Response> {
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let result = tokio::time::timeout(timeout, async {
            let (mut send, mut recv) = self.open_stream(machine).await?;
            write_message(&mut send, &signed).await?;
            read_message::<Response>(&mut recv).await
        })
        .await
        .map_err(|_| Error::Timeout("no answer from the machine"))
        .and_then(|r| r);

        match result {
            Ok(Response::Error(err)) => Err(Error::Remote(err)),
            Ok(response) => Ok(response),
            Err(err @ Error::Remote(_)) => Err(err),
            Err(err) => Err(self.failed(machine, err).await),
        }
    }

    /// Brings the machine's copy of `project` up to date and returns where it is there.
    /// Only contents the machine doesn't already have are sent; `progress(sent, total)`
    /// reports bytes as they go.
    pub async fn sync(
        &self,
        machine: &PairedMachine,
        workspace: &WorkspaceId,
        project: &LocalProject,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<SyncResult> {
        let mut needed = std::collections::HashSet::new();
        let empty: [FileEntry; 0] = [];
        let mut chunks: Vec<&[FileEntry]> = project.files.chunks(ENTRIES_PER_MESSAGE).collect();
        if chunks.is_empty() {
            chunks.push(&empty);
        }
        for (i, chunk) in chunks.into_iter().enumerate() {
            let command = Command::SyncPlan {
                workspace: workspace.clone(),
                files: chunk.to_vec(),
                first: i == 0,
            };
            match self
                .request_within(machine, command, FILE_WORK_TIMEOUT)
                .await?
            {
                Response::SyncNeeded(hashes) => needed.extend(hashes),
                other => return Err(unexpected("the contents needed", &other)),
            }
        }

        let mut by_hash: HashMap<Hash, &FileEntry> = HashMap::new();
        for file in &project.files {
            by_hash.entry(file.hash).or_insert(file);
        }
        let uploads: Vec<&FileEntry> = needed
            .iter()
            .filter_map(|h| by_hash.get(h).copied())
            .collect();
        let total: u64 = uploads.iter().map(|f| f.size).sum();
        let mut sent = 0u64;
        let mut batch: Vec<BlobPart> = Vec::new();
        let mut batch_bytes = 0usize;
        for file in &uploads {
            let full = project.root.join(&file.path);
            let mut reader = std::fs::File::open(&full).map_err(|e| Error::io(&full, e))?;
            let mut offset = 0u64;
            loop {
                let mut data = Vec::with_capacity(CHUNK_BYTES.min(file.size as usize));
                std::io::Read::read_to_end(
                    &mut std::io::Read::take(&mut reader, CHUNK_BYTES as u64),
                    &mut data,
                )
                .map_err(|e| Error::io(&full, e))?;
                let len = data.len();
                if batch_bytes + len > CHUNK_BYTES && !batch.is_empty() {
                    self.put(machine, workspace, std::mem::take(&mut batch))
                        .await?;
                    sent += batch_bytes as u64;
                    batch_bytes = 0;
                    progress(sent, total);
                }
                batch.push(BlobPart {
                    hash: file.hash,
                    offset,
                    total: file.size,
                    data,
                });
                batch_bytes += len;
                offset += len as u64;
                if len < CHUNK_BYTES || offset >= file.size {
                    break;
                }
            }
        }
        if !batch.is_empty() {
            self.put(machine, workspace, batch).await?;
            sent += batch_bytes as u64;
            progress(sent, total);
        }

        let apply = Command::SyncApply {
            workspace: workspace.clone(),
        };
        match self
            .request_within(machine, apply, FILE_WORK_TIMEOUT)
            .await?
        {
            Response::Workspace { path } => Ok(SyncResult {
                path,
                sent_files: uploads.len(),
                sent_bytes: sent,
            }),
            other => Err(unexpected("the workspace", &other)),
        }
    }

    async fn put(
        &self,
        machine: &PairedMachine,
        workspace: &WorkspaceId,
        parts: Vec<BlobPart>,
    ) -> Result<()> {
        let command = Command::SyncPut {
            workspace: workspace.clone(),
            parts,
        };
        match self.request(machine, command).await? {
            Response::Done => Ok(()),
            other => Err(unexpected("done", &other)),
        }
    }

    /// Copies files matching `patterns` from the machine's workspace into `into`, skipping
    /// ones that are already the same here. Returns the files written.
    pub async fn pull(
        &self,
        machine: &PairedMachine,
        workspace: &WorkspaceId,
        patterns: &[String],
        into: &Path,
    ) -> Result<Vec<FileEntry>> {
        let list = Command::ListFiles {
            workspace: workspace.clone(),
            patterns: patterns.to_vec(),
        };
        let files = match self
            .request_within(machine, list, FILE_WORK_TIMEOUT)
            .await?
        {
            Response::Files(files) => files,
            other => return Err(unexpected("files", &other)),
        };
        let mut written = Vec::new();
        for entry in files {
            if crate::workspace::local_matches(into, &entry) {
                continue;
            }
            let mut data = Vec::with_capacity(entry.size as usize);
            while (data.len() as u64) < entry.size {
                let read = Command::ReadFile {
                    workspace: workspace.clone(),
                    path: entry.path.clone(),
                    offset: data.len() as u64,
                };
                match self.request(machine, read).await? {
                    Response::FileData(chunk) if !chunk.is_empty() => data.extend(chunk),
                    Response::FileData(_) => break,
                    other => return Err(unexpected("file data", &other)),
                }
            }
            if *blake3::hash(&data).as_bytes() != entry.hash {
                return Err(Error::Protocol(format!(
                    "{} changed on the machine while it was being copied; try again",
                    entry.path
                )));
            }
            crate::workspace::write_local(into, &entry, &data)?;
            written.push(entry);
        }
        Ok(written)
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
        on_chunk: impl FnMut(u64, &[u8]),
    ) -> Result<JobInfo> {
        self.logs_lending(machine, id, from, follow, on_chunk, |_| {})
            .await
    }

    /// Like [`Controller::logs`], also passing on the job's borrowed-git requests
    /// (answer them with [`Controller::answer_git_credential`]).
    pub async fn logs_lending(
        &self,
        machine: &PairedMachine,
        id: &JobId,
        from: u64,
        follow: bool,
        mut on_chunk: impl FnMut(u64, &[u8]),
        mut on_git_request: impl FnMut(GitCredentialRequest),
    ) -> Result<JobInfo> {
        let command = Command::JobLogs {
            id: id.clone(),
            from,
            follow,
        };
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let result: Result<JobInfo> = async {
            let (mut send, mut recv) = self.open_stream(machine).await?;
            write_message(&mut send, &signed).await?;
            loop {
                match read_frame::<Response>(&mut recv).await? {
                    Some(Response::LogChunk { offset, data }) => on_chunk(offset, &data),
                    Some(Response::GitCredentialRequest(request)) => on_git_request(request),
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
        match result {
            Err(err @ Error::Remote(_)) => Err(err),
            Err(err) => Err(self.failed(machine, err).await),
            ok => ok,
        }
    }

    /// Starts a shell (`session: None`) or attaches to one, replaying output from byte
    /// `from` (default: all scrollback the machine kept).
    pub async fn open_terminal(
        &self,
        machine: &PairedMachine,
        session: Option<SessionId>,
        cols: u16,
        rows: u16,
        from: Option<u64>,
    ) -> Result<(TerminalInfo, TerminalWriter, TerminalReader)> {
        let command = Command::OpenTerminal {
            session,
            cols,
            rows,
            from,
        };
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let opened = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let (mut send, mut recv) = self.open_stream(machine).await?;
            // Not finished: keystrokes follow on the same stream.
            write_frame(&mut send, &signed).await?;
            match read_frame::<Response>(&mut recv).await? {
                Some(Response::TerminalOpened(info)) => Ok((info, send, recv)),
                Some(Response::Error(err)) => Err(Error::Remote(err)),
                Some(other) => Err(unexpected("an open terminal", &other)),
                None => Err(Error::Protocol(
                    "the machine closed the terminal stream".into(),
                )),
            }
        })
        .await
        .map_err(|_| Error::Timeout("no answer from the machine"))
        .and_then(|r| r);
        match opened {
            Ok((info, send, recv)) => Ok((info, TerminalWriter(send), TerminalReader(recv))),
            Err(err @ Error::Remote(_)) => Err(err),
            Err(err) => Err(self.failed(machine, err).await),
        }
    }

    /// Opens a connection to `host:port` as the machine sees it (a port forward). Bytes
    /// written to the first half reach that port; its answers come out of the second.
    pub async fn forward(
        &self,
        machine: &PairedMachine,
        host: &str,
        port: u16,
    ) -> Result<(StreamSend, StreamRecv)> {
        let command = Command::Forward {
            host: host.to_string(),
            port,
        };
        let signed = SignedRequest::new(&self.key, machine.id, command);
        let opened = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let (mut send, mut recv) = self.open_stream(machine).await?;
            // Not finished: the connection's bytes follow on the same stream.
            write_frame(&mut send, &signed).await?;
            match read_frame::<Response>(&mut recv).await? {
                Some(Response::Forwarding) => Ok((send, recv)),
                Some(Response::Error(err)) => Err(Error::Remote(err)),
                Some(other) => Err(unexpected("a forwarded connection", &other)),
                None => Err(Error::Protocol("the machine closed the stream".into())),
            }
        })
        .await
        .map_err(|_| Error::Timeout("no answer from the machine"))
        .and_then(|r| r);
        match opened {
            Err(err @ Error::Remote(_)) => Err(err),
            Err(err) => Err(self.failed(machine, err).await),
            ok => ok,
        }
    }

    /// Terminal sessions on the machine, newest first.
    pub async fn terminals(&self, machine: &PairedMachine) -> Result<Vec<TerminalInfo>> {
        match self.request(machine, Command::ListTerminals).await? {
            Response::Terminals(list) => Ok(list),
            other => Err(unexpected("terminals", &other)),
        }
    }

    /// Saves (or replaces) a secret on the machine for this device's jobs.
    pub async fn set_secret(
        &self,
        machine: &PairedMachine,
        name: SecretName,
        value: Vec<u8>,
        expires_at_ms: Option<u64>,
        note: Option<String>,
    ) -> Result<SecretInfo> {
        let command = Command::SetSecret {
            name,
            value,
            expires_at_ms,
            note,
        };
        match self.request(machine, command).await? {
            Response::Secret(info) => Ok(info),
            other => Err(unexpected("a secret", &other)),
        }
    }

    /// This device's secrets on the machine, without values.
    pub async fn secrets(&self, machine: &PairedMachine) -> Result<Vec<SecretInfo>> {
        match self.request(machine, Command::ListSecrets).await? {
            Response::Secrets(list) => Ok(list),
            other => Err(unexpected("secrets", &other)),
        }
    }

    /// Deletes one of this device's secrets on the machine.
    pub async fn remove_secret(&self, machine: &PairedMachine, name: &SecretName) -> Result<()> {
        match self
            .request(machine, Command::RemoveSecret(name.clone()))
            .await?
        {
            Response::Done => Ok(()),
            other => Err(unexpected("done", &other)),
        }
    }

    /// Lends (or, with `None`, declines) a git login a job of this device asked for.
    pub async fn answer_git_credential(
        &self,
        machine: &PairedMachine,
        job: &JobId,
        request: String,
        credential: Option<GitCredential>,
    ) -> Result<()> {
        let command = Command::AnswerGitCredential {
            job: job.clone(),
            request,
            credential,
        };
        match self.request(machine, command).await? {
            Response::Done => Ok(()),
            other => Err(unexpected("done", &other)),
        }
    }

    /// Turns this device's auto-cleanup on the machine on (`Some(days)`) or off.
    pub async fn set_cleanup(
        &self,
        machine: &PairedMachine,
        after_days: Option<u32>,
    ) -> Result<()> {
        match self
            .request(machine, Command::SetCleanup { after_days })
            .await?
        {
            Response::Done => Ok(()),
            other => Err(unexpected("done", &other)),
        }
    }

    /// What this device has left on the machine.
    pub async fn footprint(&self, machine: &PairedMachine) -> Result<Footprint> {
        match self.request(machine, Command::Footprint).await? {
            Response::Footprint(footprint) => Ok(footprint),
            other => Err(unexpected("a footprint", &other)),
        }
    }

    /// Removes part or all of this device's footprint on the machine.
    pub async fn remove_footprint(
        &self,
        machine: &PairedMachine,
        item: FootprintItem,
    ) -> Result<Removed> {
        match self
            .request_within(machine, Command::RemoveFootprint(item), FILE_WORK_TIMEOUT)
            .await?
        {
            Response::Removed(removed) => Ok(removed),
            other => Err(unexpected("removed", &other)),
        }
    }

    /// Unpairs this device from the machine (after removing its footprint with `wipe`)
    /// and forgets the machine here.
    pub async fn unpair(&self, machine: &PairedMachine, wipe: bool) -> Result<Removed> {
        let removed = match self
            .request_within(machine, Command::Unpair { wipe }, FILE_WORK_TIMEOUT)
            .await?
        {
            Response::Removed(removed) => removed,
            other => return Err(unexpected("removed", &other)),
        };
        self.forget(&machine.id)?;
        Ok(removed)
    }

    /// Devices paired with the machine.
    pub async fn devices(&self, machine: &PairedMachine) -> Result<Vec<DeviceInfo>> {
        match self.request(machine, Command::ListDevices).await? {
            Response::Devices(list) => Ok(list),
            other => Err(unexpected("devices", &other)),
        }
    }

    /// Unpairs another device from the machine, removing its footprint with `wipe`.
    pub async fn revoke_device(
        &self,
        machine: &PairedMachine,
        id: DeviceId,
        wipe: bool,
    ) -> Result<Removed> {
        match self
            .request_within(
                machine,
                Command::RevokeDevice { id, wipe },
                FILE_WORK_TIMEOUT,
            )
            .await?
        {
            Response::Removed(removed) => Ok(removed),
            other => Err(unexpected("removed", &other)),
        }
    }

    /// Ends a session's shell.
    pub async fn close_terminal(
        &self,
        machine: &PairedMachine,
        id: &SessionId,
    ) -> Result<TerminalInfo> {
        match self
            .request(machine, Command::CloseTerminal(id.clone()))
            .await?
        {
            Response::Terminal(info) => Ok(info),
            other => Err(unexpected("a terminal", &other)),
        }
    }

    /// Closes all connections.
    pub async fn close(&self) {
        self.connections.lock().await.clear();
        if let Some(endpoint) = self.endpoint.get() {
            endpoint.close().await;
        }
    }

    /// A new stream to `machine`: through the app when it hosts this device's
    /// connections, otherwise over this process's own connection.
    async fn open_stream(&self, machine: &PairedMachine) -> Result<(StreamSend, StreamRecv)> {
        #[cfg(unix)]
        if !self.hosting.load(std::sync::atomic::Ordering::Relaxed)
            && std::env::var_os(DIRECT_ENV).is_none()
            && let Some(stream) = self.hosted_stream(machine).await?
        {
            return Ok(stream);
        }
        let conn = self.connection(machine).await?;
        match conn.open_bi().await {
            Ok((send, recv)) => Ok((Box::new(send), Box::new(recv))),
            Err(err) => Err(self.failed(machine, Error::network(err)).await),
        }
    }

    /// A stream through the app, or `None` if no app is hosting.
    #[cfg(unix)]
    async fn hosted_stream(
        &self,
        machine: &PairedMachine,
    ) -> Result<Option<(StreamSend, StreamRecv)>> {
        let Ok(stream) = tokio::net::UnixStream::connect(&self.host_socket).await else {
            return Ok(None);
        };
        let (mut recv, mut send) = stream.into_split();
        let open = HostOpen {
            machine: machine.id,
            why: false,
        };
        write_frame(&mut send, &open).await?;
        match read_frame::<HostReply>(&mut recv).await? {
            Some(Ok(())) => Ok(Some((Box::new(send), Box::new(recv)))),
            Some(Err(err)) => Err(Error::Remote(err)),
            None => Err(Error::Protocol(
                "the Yonder app closed the connection; try again".into(),
            )),
        }
    }

    /// After a stream failed: drop the connection and explain why it closed, if it did.
    async fn failed(&self, machine: &PairedMachine, err: Error) -> Error {
        if let Some(conn) = self.connections.lock().await.remove(&machine.id) {
            return explain_close(&conn).unwrap_or(err);
        }
        #[cfg(unix)]
        if !self.hosting.load(std::sync::atomic::Ordering::Relaxed)
            && let Some(why) = self.hosted_why(machine).await
        {
            return why;
        }
        err
    }

    /// Asks the app why its connection to `machine` closed, if it knows.
    #[cfg(unix)]
    async fn hosted_why(&self, machine: &PairedMachine) -> Option<Error> {
        let stream = tokio::net::UnixStream::connect(&self.host_socket)
            .await
            .ok()?;
        let (mut recv, mut send) = stream.into_split();
        let ask = HostOpen {
            machine: machine.id,
            why: true,
        };
        write_frame(&mut send, &ask).await.ok()?;
        match read_frame::<HostReply>(&mut recv).await.ok()?? {
            Err(err) => Some(Error::Remote(err)),
            Ok(()) => None,
        }
    }

    /// Hosts this device's connections for other processes (the CLI) until the returned
    /// task is aborted. Fails if another process already hosts.
    #[cfg(unix)]
    pub fn host(self: &std::sync::Arc<Self>) -> Result<tokio::task::JoinHandle<()>> {
        let path = &self.host_socket;
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(Error::Protocol(
                "another Yonder app already hosts this device's connections".into(),
            ));
        }
        // Left by a host that stopped without cleaning up.
        let _ = std::fs::remove_file(path);
        let listener = tokio::net::UnixListener::bind(path).map_err(|e| Error::io(path, e))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Error::io(path, e))?;
        }
        self.hosting
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let controller = self.clone();
        Ok(tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let controller = controller.clone();
                tokio::spawn(async move { controller.serve_hosted(stream).await });
            }
        }))
    }

    /// Stops hosting: removes the socket so other processes connect on their own.
    #[cfg(unix)]
    pub fn stop_hosting(&self, task: tokio::task::JoinHandle<()>) {
        task.abort();
        let _ = std::fs::remove_file(&self.host_socket);
        self.hosting
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// One hosted stream: open the same to the machine, then pass bytes both ways.
    #[cfg(unix)]
    async fn serve_hosted(&self, stream: tokio::net::UnixStream) {
        let (mut local_recv, mut local_send) = stream.into_split();
        let Ok(Some(open)) = read_frame::<HostOpen>(&mut local_recv).await else {
            return;
        };
        if open.why {
            let reply: HostReply = match lock(&self.closed).remove(&open.machine) {
                Some(err) => Err(err),
                None => Ok(()),
            };
            let _ = write_frame(&mut local_send, &reply).await;
            return;
        }
        let opened = async {
            let mut machine = self.machines().into_iter().find(|m| m.id == open.machine);
            if machine.is_none() {
                // Paired by the CLI since this process loaded its list.
                self.reload()?;
                machine = self.machines().into_iter().find(|m| m.id == open.machine);
            }
            let machine = machine.ok_or_else(|| Error::UnknownMachine(open.machine.to_string()))?;
            let conn = self.connection(&machine).await?;
            match conn.open_bi().await {
                Ok((send, recv)) => Ok((send, recv, conn)),
                Err(err) => Err(self.failed(&machine, Error::network(err)).await),
            }
        }
        .await;
        let (mut send, mut recv, conn) = match opened {
            Ok(pair) => pair,
            Err(err) => {
                let reply: HostReply = Err(match err {
                    Error::Remote(remote) => remote,
                    other => RemoteError::new(ErrorCode::Internal, other.to_string()),
                });
                let _ = write_frame(&mut local_send, &reply).await;
                return;
            }
        };
        if write_frame(&mut local_send, &HostReply::Ok(()))
            .await
            .is_err()
        {
            return;
        }
        let up = async {
            let _ = tokio::io::copy(&mut local_recv, &mut send).await;
            let _ = send.shutdown().await;
        };
        let down = async {
            let _ = tokio::io::copy(&mut recv, &mut local_send).await;
            // Recorded before the other process sees the stream end and asks why.
            if let Some(Error::Remote(err)) = explain_close(&conn) {
                lock(&self.closed).insert(open.machine, err);
                self.connections.lock().await.remove(&open.machine);
            }
            let _ = local_send.shutdown().await;
        };
        tokio::join!(up, down);
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
            self.endpoint()
                .await?
                .connect(machine.addr.clone(), CONTROL_ALPN),
        )
        .await
        .map_err(|_| Error::Timeout("couldn't reach the machine; is it on and online?"))?
        .map_err(Error::network)?;
        connections.insert(machine.id, conn.clone());
        Ok(conn)
    }
}

/// Sends keystrokes and size changes to an open terminal. Dropping it (or [`detach`])
/// leaves the shell running on the machine.
///
/// [`detach`]: TerminalWriter::detach
pub struct TerminalWriter(StreamSend);

impl std::fmt::Debug for TerminalWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TerminalWriter")
    }
}

impl TerminalWriter {
    pub async fn send(&mut self, data: &[u8]) -> Result<()> {
        write_frame(&mut self.0, &TerminalInput::Data(data.to_vec())).await
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        write_frame(&mut self.0, &TerminalInput::Resize { cols, rows }).await
    }

    /// Detaches; the shell keeps running.
    pub fn detach(mut self) {
        tokio::spawn(async move {
            let _ = self.0.shutdown().await;
        });
    }
}

/// What a terminal sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Output starting at byte `offset` of the session.
    Output { offset: u64, data: Vec<u8> },
    /// The shell exited.
    Exit(Option<u32>),
}

/// Receives a terminal's output.
pub struct TerminalReader(StreamRecv);

impl std::fmt::Debug for TerminalReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TerminalReader")
    }
}

impl TerminalReader {
    /// The next output or the exit. An error means the connection dropped: reattach with
    /// `from` set to the end of the last output received.
    pub async fn next(&mut self) -> Result<TerminalEvent> {
        match read_frame::<Response>(&mut self.0).await? {
            Some(Response::TerminalOutput { offset, data }) => {
                Ok(TerminalEvent::Output { offset, data })
            }
            Some(Response::TerminalExit { code }) => Ok(TerminalEvent::Exit(code)),
            Some(Response::Error(err)) => Err(Error::Remote(err)),
            Some(other) => Err(unexpected("terminal output", &other)),
            None => Err(Error::Protocol("the terminal stream ended".into())),
        }
    }
}

fn unexpected(wanted: &str, got: &Response) -> Error {
    Error::Protocol(format!("expected {wanted}, got {got:?}"))
}

/// What a sync did.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// The workspace's folder on the machine.
    pub path: String,
    pub sent_files: usize,
    pub sent_bytes: u64,
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
