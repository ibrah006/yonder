//! Messages exchanged between controllers and agents.
//!
//! Each request travels on its own QUIC stream: the controller writes one message and
//! finishes the stream, the agent answers with one message. Messages are postcard-encoded.
//!
//! The connection itself is already end-to-end encrypted and authenticated to both device
//! keys. On top of that every command is signed by the controller, so the agent's audit
//! log can prove who asked for what, and a signed command can't be reused elsewhere.

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use iroh::Signature;
use iroh::endpoint::{ReadExactError, RecvStream, SendStream};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::Os;
use crate::error::{Error, Result};
use crate::identity::{DeviceId, DeviceKey};
use crate::jobs::{JobId, JobInfo, JobSpec};
use crate::permissions::{Permission, Permissions};
use crate::terminal::{SessionId, TerminalInfo};

/// ALPN for normal use by paired controllers.
pub const CONTROL_ALPN: &[u8] = b"yonder/control/1";
/// ALPN for pairing. Only answered while pairing is open on the machine.
pub const PAIR_ALPN: &[u8] = b"yonder/pair/1";

/// Largest message either side accepts.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// How far a request's timestamp may be from the agent's clock.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(10 * 60);

/// QUIC close codes the agent uses when it drops a connection.
pub mod close_code {
    pub const NOT_PAIRED: u32 = 1;
    pub const REVOKED: u32 = 2;
    pub const SHUTTING_DOWN: u32 = 3;
}

// ---------------------------------------------------------------------------
// Commands

/// Something a controller asks a machine to do. Append-only: variants are encoded by position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    /// Check the machine is reachable.
    Ping,
    /// Describe the machine and what this controller may do there.
    Info,
    /// Start a job. Answered with [`Response::Job`].
    RunJob(JobSpec),
    /// All jobs on the machine, newest first. Answered with [`Response::Jobs`].
    ListJobs,
    /// One job's details. Answered with [`Response::Job`].
    GetJob(JobId),
    /// A job's output from byte `from`. Answered with [`Response::LogChunk`]s and a final
    /// [`Response::LogEnd`]. With `follow`, keeps streaming until the job finishes.
    JobLogs { id: JobId, from: u64, follow: bool },
    /// Stop a queued or running job. Answered with [`Response::Job`].
    CancelJob(JobId),
    /// Start a new shell (`session: None`) or attach to one. Answered with
    /// [`Response::TerminalOpened`], then [`Response::TerminalOutput`]s from byte `from`
    /// (default: all kept scrollback) and finally [`Response::TerminalExit`]. The client
    /// sends [`crate::terminal::TerminalInput`] frames on the same stream; finishing it
    /// detaches.
    OpenTerminal {
        session: Option<SessionId>,
        cols: u16,
        rows: u16,
        from: Option<u64>,
    },
    /// Terminal sessions on the machine. Answered with [`Response::Terminals`].
    ListTerminals,
    /// End a session's shell. Answered with [`Response::Terminal`].
    CloseTerminal(SessionId),
}

impl Command {
    /// The permission needed to run this command, if any beyond being paired.
    pub fn required_permission(&self) -> Option<Permission> {
        match self {
            Command::Ping | Command::Info => None,
            Command::RunJob(_)
            | Command::ListJobs
            | Command::GetJob(_)
            | Command::JobLogs { .. }
            | Command::CancelJob(_) => Some(Permission::RunJobs),
        }
    }

    /// Short name for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Ping => "ping",
            Command::Info => "info",
            Command::RunJob(_) => "run_job",
            Command::ListJobs => "list_jobs",
            Command::GetJob(_) => "get_job",
            Command::JobLogs { .. } => "job_logs",
            Command::CancelJob(_) => "cancel_job",
        }
    }

    /// Details for the audit log: the command line for new jobs, the job id otherwise.
    pub fn detail(&self) -> Option<String> {
        match self {
            Command::Ping | Command::Info | Command::ListJobs => None,
            Command::RunJob(spec) => Some(spec.summary()),
            Command::GetJob(id) | Command::CancelJob(id) | Command::JobLogs { id, .. } => {
                Some(id.to_string())
            }
        }
    }
}

/// The signed part of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBody {
    pub machine: DeviceId,
    pub controller: DeviceId,
    pub nonce: [u8; 16],
    /// Milliseconds since the Unix epoch, on the controller's clock.
    pub issued_at_ms: u64,
    pub command: Command,
}

/// A request as sent on the wire: the encoded body and the controller's signature over it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedRequest {
    pub body: Vec<u8>,
    pub signature: Signature,
}

impl SignedRequest {
    /// Builds and signs a request for `machine`.
    pub fn new(key: &DeviceKey, machine: DeviceId, command: Command) -> Self {
        let body = RequestBody {
            machine,
            controller: key.id(),
            nonce: rand::random(),
            issued_at_ms: now_ms(),
            command,
        };
        Self::sign(key, &body)
    }

    pub(crate) fn sign(key: &DeviceKey, body: &RequestBody) -> Self {
        let body = postcard::to_stdvec(body).expect("request is always serializable");
        let signature = key.sign(&signed_bytes(&body));
        SignedRequest { body, signature }
    }

    /// Checks the signature, that it came from `peer` over this connection, that it's
    /// addressed to `machine`, that it's fresh, and that it wasn't seen before.
    pub fn verify(
        &self,
        peer: &DeviceId,
        machine: &DeviceId,
        now_ms: u64,
        seen: &mut ReplayGuard,
    ) -> std::result::Result<RequestBody, RemoteError> {
        peer.verify(&signed_bytes(&self.body), &self.signature)
            .map_err(|_| RemoteError::new(ErrorCode::BadSignature, "signature check failed"))?;
        let body: RequestBody = postcard::from_bytes(&self.body)
            .map_err(|e| RemoteError::new(ErrorCode::BadRequest, e.to_string()))?;
        if &body.controller != peer {
            return Err(RemoteError::new(
                ErrorCode::BadSignature,
                "request was signed for a different connection",
            ));
        }
        if &body.machine != machine {
            return Err(RemoteError::new(
                ErrorCode::BadRequest,
                "request was addressed to a different machine",
            ));
        }
        let skew = now_ms.abs_diff(body.issued_at_ms);
        if skew > MAX_CLOCK_SKEW.as_millis() as u64 {
            return Err(RemoteError::new(
                ErrorCode::ClockSkew,
                format!(
                    "clocks differ by {} minutes; check the date and time on both devices",
                    skew / 60_000
                ),
            ));
        }
        if !seen.check_and_remember(body.nonce) {
            return Err(RemoteError::new(
                ErrorCode::Replay,
                "request was already used",
            ));
        }
        Ok(body)
    }
}

/// Domain-separates request signatures from any other use of the device key.
fn signed_bytes(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 32);
    out.extend_from_slice(b"yonder request v1\0");
    out.extend_from_slice(body);
    out
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Remembers recent request nonces so a request can't be accepted twice.
#[derive(Debug)]
pub struct ReplayGuard {
    seen: HashMap<[u8; 16], Instant>,
    keep_for: Duration,
    limit: usize,
}

impl Default for ReplayGuard {
    fn default() -> Self {
        // Anything older than the clock-skew window is rejected anyway.
        Self::new(MAX_CLOCK_SKEW * 2, 100_000)
    }
}

impl ReplayGuard {
    pub fn new(keep_for: Duration, limit: usize) -> Self {
        ReplayGuard {
            seen: HashMap::new(),
            keep_for,
            limit,
        }
    }

    /// Returns false if the nonce was seen recently; otherwise remembers it.
    pub fn check_and_remember(&mut self, nonce: [u8; 16]) -> bool {
        let now = Instant::now();
        if self.seen.len() >= self.limit {
            let keep_for = self.keep_for;
            self.seen.retain(|_, at| now.duration_since(*at) < keep_for);
        }
        if self.seen.len() >= self.limit {
            // Still full of fresh nonces: refuse rather than forget, which would allow replays.
            return false;
        }
        match self.seen.get(&nonce) {
            Some(at) if now.duration_since(*at) < self.keep_for => false,
            _ => {
                self.seen.insert(nonce, now);
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Responses

/// What the agent sends back for a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Info(MachineInfo),
    Error(RemoteError),
    Job(JobInfo),
    Jobs(Vec<JobInfo>),
    /// Part of a job's output, starting at byte `offset`.
    LogChunk {
        offset: u64,
        data: Vec<u8>,
    },
    /// End of a log stream, with the job's state at that moment.
    LogEnd {
        job: JobInfo,
    },
}

/// Description of a machine, as seen by one controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub name: String,
    pub os: Os,
    pub agent_version: String,
    /// What the asking controller may do on this machine.
    pub your_permissions: Permissions,
    /// The owner paused new jobs on this machine.
    pub paused: bool,
}

/// Why the agent refused or failed a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    NotPaired,
    PermissionDenied,
    BadSignature,
    Replay,
    ClockSkew,
    BadRequest,
    Internal,
    /// The owner paused new jobs on this machine.
    Paused,
    /// No such job.
    NotFound,
    /// This agent can't do that (for example, it was started without job support).
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteError {
    pub code: ErrorCode,
    pub message: String,
}

impl RemoteError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        RemoteError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "machine refused: {} ({:?})", self.message, self.code)
    }
}

// ---------------------------------------------------------------------------
// Pairing

/// Sent by a controller on the pairing ALPN.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    pub controller_name: String,
    /// See [`crate::ticket`]: proves the controller holds the ticket secret.
    pub proof: [u8; 32],
}

/// The machine's answer to a pairing request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairResponse {
    Accepted {
        machine_name: String,
        os: Os,
        permissions: Permissions,
    },
    Rejected(PairRejection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairRejection {
    /// Pairing isn't open on the machine (never opened, expired, or already used).
    NotOpen,
    /// The ticket's secret didn't match.
    WrongTicket,
    /// Someone at the machine said no, or nobody answered in time.
    Declined,
    /// Another pairing request is waiting for approval.
    Busy,
    /// The machine approved but couldn't save the pairing.
    Failed,
}

impl fmt::Display for PairRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PairRejection::NotOpen => {
                "pairing isn't open on that machine (it may have expired); open it again there"
            }
            PairRejection::WrongTicket => "the ticket doesn't match; copy it again",
            PairRejection::Declined => "the request was declined on the machine",
            PairRejection::Busy => "another pairing request is waiting on that machine",
            PairRejection::Failed => "the machine couldn't save the pairing; check its logs",
        })
    }
}

// ---------------------------------------------------------------------------
// Framing

//
// Every message is a 4-byte little-endian length followed by the postcard bytes, so one
// stream can carry several messages (a log stream) as well as just one.

/// Writes one message without finishing the stream.
pub async fn write_frame<T: Serialize>(send: &mut SendStream, message: &T) -> Result<()> {
    let bytes = postcard::to_stdvec(message).expect("messages are always serializable");
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Protocol("message too large".into()));
    }
    let len = (bytes.len() as u32).to_le_bytes();
    send.write_all(&len).await.map_err(Error::network)?;
    send.write_all(&bytes).await.map_err(Error::network)?;
    Ok(())
}

/// Reads one message. `None` means the other side finished the stream cleanly.
pub async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(err) => return Err(Error::network(err)),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE_BYTES {
        return Err(Error::Protocol(format!(
            "message of {len} bytes is too large"
        )));
    }
    let mut bytes = vec![0u8; len];
    recv.read_exact(&mut bytes).await.map_err(Error::network)?;
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(Error::protocol)
}

/// Writes one message and finishes the stream.
pub async fn write_message<T: Serialize>(send: &mut SendStream, message: &T) -> Result<()> {
    write_frame(send, message).await?;
    send.finish().map_err(Error::network)?;
    Ok(())
}

/// Reads exactly one message.
pub async fn read_message<T: DeserializeOwned>(recv: &mut RecvStream) -> Result<T> {
    read_frame(recv)
        .await?
        .ok_or_else(|| Error::Protocol("the stream ended before a message arrived".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (DeviceKey, DeviceId) {
        (DeviceKey::generate(), DeviceKey::generate().id())
    }

    #[test]
    fn valid_request_verifies() {
        let (controller, machine) = setup();
        let req = SignedRequest::new(&controller, machine, Command::Ping);
        let body = req
            .verify(
                &controller.id(),
                &machine,
                now_ms(),
                &mut ReplayGuard::default(),
            )
            .unwrap();
        assert_eq!(body.command, Command::Ping);
    }

    #[test]
    fn tampered_body_is_rejected() {
        let (controller, machine) = setup();
        let mut req = SignedRequest::new(&controller, machine, Command::Ping);
        let last = req.body.len() - 1;
        req.body[last] ^= 1;
        let err = req
            .verify(
                &controller.id(),
                &machine,
                now_ms(),
                &mut ReplayGuard::default(),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadSignature);
    }

    #[test]
    fn request_from_another_key_is_rejected() {
        let (controller, machine) = setup();
        let other = DeviceKey::generate();
        let req = SignedRequest::new(&other, machine, Command::Ping);
        let err = req
            .verify(
                &controller.id(),
                &machine,
                now_ms(),
                &mut ReplayGuard::default(),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadSignature);
    }

    #[test]
    fn request_for_another_machine_is_rejected() {
        let (controller, machine) = setup();
        let req = SignedRequest::new(&controller, DeviceKey::generate().id(), Command::Ping);
        let err = req
            .verify(
                &controller.id(),
                &machine,
                now_ms(),
                &mut ReplayGuard::default(),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::BadRequest);
    }

    #[test]
    fn stale_request_is_rejected() {
        let (controller, machine) = setup();
        let req = SignedRequest::new(&controller, machine, Command::Ping);
        let later = now_ms() + MAX_CLOCK_SKEW.as_millis() as u64 + 60_000;
        let err = req
            .verify(
                &controller.id(),
                &machine,
                later,
                &mut ReplayGuard::default(),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ClockSkew);
    }

    #[test]
    fn replayed_request_is_rejected() {
        let (controller, machine) = setup();
        let req = SignedRequest::new(&controller, machine, Command::Ping);
        let mut guard = ReplayGuard::default();
        req.verify(&controller.id(), &machine, now_ms(), &mut guard)
            .unwrap();
        let err = req
            .verify(&controller.id(), &machine, now_ms(), &mut guard)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Replay);
    }

    #[test]
    fn full_replay_guard_refuses_instead_of_forgetting() {
        let mut guard = ReplayGuard::new(Duration::from_secs(60), 2);
        assert!(guard.check_and_remember([1; 16]));
        assert!(guard.check_and_remember([2; 16]));
        assert!(!guard.check_and_remember([3; 16]));
        assert!(!guard.check_and_remember([1; 16]));
    }

    #[test]
    fn expired_nonces_are_forgotten() {
        let mut guard = ReplayGuard::new(Duration::ZERO, 1);
        assert!(guard.check_and_remember([1; 16]));
        assert!(guard.check_and_remember([1; 16]));
    }
}
