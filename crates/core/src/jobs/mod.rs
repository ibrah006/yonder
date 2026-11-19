//! Jobs: commands a controller asks a machine to run.
//!
//! Each job runs in its own *runner* process (`yonder-agent job-run <dir>`), separate from
//! the agent. The runner starts the command, writes its output straight to a log file and
//! records the result. Because of that split:
//!
//! - a job keeps running when the controller disconnects, and also when the agent itself
//!   restarts (for example after an update);
//! - anyone can reattach and read the log from any offset;
//! - later, the runner can be started as a separate build user (Step 2, build user).
//!
//! Layout of one job folder (`<state>/jobs/<id>/`):
//!
//! | file          | written by | contents                                  |
//! |---------------|------------|-------------------------------------------|
//! | `job.json`    | agent      | what to run, who asked, when              |
//! | `status.json` | both       | queued / running / finished, exit code    |
//! | `output.log`  | the command| stdout and stderr, interleaved            |
//! | `cancel`      | agent      | present when cancellation was requested   |
//! | `secrets.bin` | agent      | values of the secrets the job asked for; the runner reads and deletes it |
//! | `secret-files.json` | runner | secret files written for the job, deleted when it ends |

pub mod manager;
pub mod runner;
mod sys;

use std::fmt;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::DeviceId;
use crate::protocol::now_ms;
use crate::secrets::{Resolved, SecretTarget, SecretUse};
use crate::store;

pub use sys::process_alive;

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobProgram {
    /// A script for the machine's shell: PowerShell on Windows, `sh` elsewhere.
    Shell(String),
    /// A program and its arguments, run without a shell.
    Exec { program: String, args: Vec<String> },
}

/// A job as a controller asks for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub program: JobProgram,
    /// Working folder on the machine. Defaults to the build user's home.
    pub cwd: Option<String>,
    /// Extra environment variables. Only the names are written to the audit log.
    pub env: Vec<(String, String)>,
    /// Short label shown in job lists, e.g. "release windows".
    pub label: Option<String>,
    /// Saved secrets the job gets (see [`crate::secrets`]). Only names travel here.
    #[serde(default)]
    pub secrets: Vec<SecretUse>,
    /// Git in the job borrows logins from the device that started it, while that device
    /// follows the job (see [`crate::credentials`]).
    #[serde(default)]
    pub borrow_git: bool,
    /// Runs with administrator rights instead of as the build user. Needs the
    /// controller's Admin permission, which the machine's owner grants.
    #[serde(default)]
    pub admin: bool,
}

impl JobSpec {
    /// A job running `script` in the machine's shell.
    pub fn shell(script: impl Into<String>) -> Self {
        JobSpec {
            program: JobProgram::Shell(script.into()),
            cwd: None,
            env: Vec::new(),
            label: None,
            secrets: Vec::new(),
            borrow_git: false,
            admin: false,
        }
    }

    /// One line describing the command, for lists and the audit log. Never includes
    /// environment values.
    pub fn summary(&self) -> String {
        let command = match &self.program {
            JobProgram::Shell(script) => script.lines().collect::<Vec<_>>().join(" ⏎ "),
            JobProgram::Exec { program, args } => std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        };
        let mut summary: String = command.chars().take(200).collect();
        if command.chars().count() > 200 {
            summary.push('…');
        }
        if !self.env.is_empty() {
            let names: Vec<_> = self.env.iter().map(|(k, _)| k.as_str()).collect();
            summary.push_str(&format!(" [env: {}]", names.join(", ")));
        }
        if !self.secrets.is_empty() {
            let uses: Vec<_> = self.secrets.iter().map(ToString::to_string).collect();
            summary.push_str(&format!(" [secrets: {}]", uses.join(", ")));
        }
        if self.borrow_git {
            summary.push_str(" [borrows git logins]");
        }
        if self.admin {
            summary.push_str(" [as administrator]");
        }
        summary
    }
}

/// Identifies a job: 16 lowercase hex characters, sortable by creation time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// A new id based on the current time.
    pub fn generate() -> Self {
        let suffix: u16 = rand::random();
        JobId(format!("{:012x}{suffix:04x}", now_ms() & 0xffff_ffff_ffff))
    }

    /// Accepts only well-formed ids, so an id from the network can't name another folder.
    pub fn parse(text: &str) -> Option<Self> {
        let valid = text.len() == 16
            && text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| JobId(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for JobId {
    type Error = String;
    fn try_from(value: String) -> std::result::Result<Self, String> {
        JobId::parse(&value).ok_or_else(|| format!("\"{value}\" is not a job id"))
    }
}

impl From<JobId> for String {
    fn from(id: JobId) -> String {
        id.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    /// The runner disappeared without recording a result (crash, power cut).
    Lost,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        !matches!(self, JobState::Queued | JobState::Running)
    }

    pub fn name(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Succeeded => "succeeded",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
            JobState::Lost => "lost",
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `job.json`: what to run and who asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobMeta {
    pub id: JobId,
    pub spec: JobSpec,
    pub controller: DeviceId,
    pub controller_name: String,
    pub created_at_ms: u64,
}

/// `status.json`: progress and result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub state: JobState,
    pub runner_pid: Option<u32>,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub exit_code: Option<i32>,
    /// Explanation when there's no exit code to tell the story ("program not found").
    pub message: Option<String>,
}

impl JobStatus {
    pub fn queued() -> Self {
        JobStatus {
            state: JobState::Queued,
            runner_pid: None,
            started_at_ms: None,
            finished_at_ms: None,
            exit_code: None,
            message: None,
        }
    }

    /// This status, ended in `state` now, with an optional explanation.
    pub fn finish(mut self, state: JobState, message: Option<String>) -> Self {
        self.state = state;
        self.finished_at_ms = Some(now_ms());
        if message.is_some() {
            self.message = message;
        }
        self
    }
}

/// Everything about a job, as sent to controllers. Environment values are left out: any
/// paired device may list jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobInfo {
    pub meta: JobMeta,
    pub status: JobStatus,
    pub log_bytes: u64,
}

/// The jobs folder.
#[derive(Debug, Clone)]
pub struct JobStore {
    dir: PathBuf,
}

const META: &str = "job.json";
const STATUS: &str = "status.json";
const LOG: &str = "output.log";
const CANCEL: &str = "cancel";
const SECRETS: &str = "secrets.bin";
const SECRET_FILES: &str = "secret-files.json";

impl JobStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        JobStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn job_dir(&self, id: &JobId) -> PathBuf {
        self.dir.join(id.as_str())
    }

    /// Creates the folder for a new job, queued.
    pub fn create(&self, meta: &JobMeta) -> Result<PathBuf> {
        let dir = self.job_dir(&meta.id);
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        write_json(&dir.join(META), meta)?;
        write_json(&dir.join(STATUS), &JobStatus::queued())?;
        let log = dir.join(LOG);
        std::fs::File::create(&log).map_err(|e| Error::io(&log, e))?;
        Ok(dir)
    }

    pub fn exists(&self, id: &JobId) -> bool {
        self.job_dir(id).join(META).is_file()
    }

    pub fn info(&self, id: &JobId) -> Result<JobInfo> {
        info_in(&self.job_dir(id))
    }

    pub fn write_status(&self, id: &JobId, status: &JobStatus) -> Result<()> {
        write_json(&self.job_dir(id).join(STATUS), status)
    }

    /// All jobs, newest first. Folders that can't be read are skipped.
    pub fn list(&self) -> Vec<JobInfo> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut jobs: Vec<JobInfo> = entries
            .flatten()
            .filter(|e| e.file_name().to_str().and_then(JobId::parse).is_some())
            .filter_map(|e| info_in(&e.path()).ok())
            .collect();
        jobs.sort_by(|a, b| b.meta.id.cmp(&a.meta.id));
        jobs
    }

    /// Reads up to `max` bytes of a job's output starting at `offset`.
    pub fn read_log(&self, id: &JobId, offset: u64, max: usize) -> Result<Vec<u8>> {
        let path = self.job_dir(id).join(LOG);
        let mut file = std::fs::File::open(&path).map_err(|e| Error::io(&path, e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Error::io(&path, e))?;
        let mut buf = Vec::with_capacity(max.min(64 * 1024));
        file.take(max as u64)
            .read_to_end(&mut buf)
            .map_err(|e| Error::io(&path, e))?;
        Ok(buf)
    }

    /// Hands secret values to the job's runner, which deletes them once read.
    pub fn write_secrets(&self, id: &JobId, secrets: &[Resolved]) -> Result<()> {
        let bytes = postcard::to_stdvec(secrets).expect("secrets are always serializable");
        store::write_secret_file(&self.job_dir(id).join(SECRETS), &bytes)
    }

    /// Removes whatever secrets a job left behind: values its runner never read (it was
    /// cancelled while queued) and files a runner that died didn't delete.
    pub fn scrub(&self, id: &JobId) {
        let dir = self.job_dir(id);
        let _ = std::fs::remove_file(dir.join(SECRETS));
        remove_secret_files(&dir);
        crate::credentials::clear(&dir);
    }

    /// Deletes a finished job's folder, log included.
    pub fn remove(&self, id: &JobId) -> Result<()> {
        self.scrub(id);
        let dir = self.job_dir(id);
        std::fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))
    }

    /// Asks the job's runner to stop.
    pub fn request_cancel(&self, id: &JobId) -> Result<()> {
        let path = self.job_dir(id).join(CANCEL);
        std::fs::write(&path, b"").map_err(|e| Error::io(&path, e))
    }

    /// Deletes the oldest finished jobs beyond `keep`.
    pub fn prune(&self, keep: usize) {
        for job in self.list().into_iter().skip(keep) {
            if job.status.state.is_finished() {
                let _ = std::fs::remove_dir_all(self.job_dir(&job.meta.id));
            }
        }
    }
}

/// Reads and deletes the secret values the agent left for the job in `dir`.
pub(crate) fn take_secrets(dir: &Path) -> Result<Vec<Resolved>> {
    let path = dir.join(SECRETS);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&path, e)),
    };
    std::fs::remove_file(&path).map_err(|e| Error::io(&path, e))?;
    postcard::from_bytes(&bytes).map_err(|e| Error::CorruptFile {
        path,
        message: e.to_string(),
    })
}

/// Writes the job's secret files into `cwd`, recording them first so they can be removed
/// even if the runner dies.
pub(crate) fn write_secret_files(dir: &Path, cwd: &Path, secrets: &[Resolved]) -> Result<()> {
    let paths: Vec<PathBuf> = secrets
        .iter()
        .filter_map(|s| match &s.target {
            SecretTarget::File(relative) => Some(cwd.join(relative)),
            SecretTarget::Env(_) => None,
        })
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    write_json(&dir.join(SECRET_FILES), &paths)?;
    for (secret, path) in secrets
        .iter()
        .filter(|s| matches!(s.target, SecretTarget::File(_)))
        .zip(&paths)
    {
        write_owner_only(path, &secret.value).map_err(|e| Error::io(path, e))?;
    }
    Ok(())
}

/// Writes a file only its owner can read, leaving the folder's permissions alone (it's
/// the job's folder, not ours).
fn write_owner_only(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents)
}

/// Deletes the secret files recorded for the job in `dir`.
pub(crate) fn remove_secret_files(dir: &Path) {
    let list = dir.join(SECRET_FILES);
    if let Ok(paths) = read_json::<Vec<PathBuf>>(&list) {
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = std::fs::remove_file(list);
}

fn info_in(dir: &Path) -> Result<JobInfo> {
    let mut meta: JobMeta = read_json(&dir.join(META))?;
    for (_, value) in &mut meta.spec.env {
        value.clear();
    }
    let status: JobStatus = read_json(&dir.join(STATUS))?;
    let log_bytes = std::fs::metadata(dir.join(LOG))
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(JobInfo {
        meta,
        status,
        log_bytes,
    })
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::CorruptFile {
        path: path.to_owned(),
        message: e.to_string(),
    })
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).expect("job files are always serializable");
    store::write_secret_file(path, &bytes)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::identity::DeviceKey;

    pub(crate) fn meta(spec: JobSpec) -> JobMeta {
        JobMeta {
            id: JobId::generate(),
            spec,
            controller: DeviceKey::generate().id(),
            controller_name: "mac".into(),
            created_at_ms: now_ms(),
        }
    }

    #[test]
    fn job_ids_are_strict() {
        let id = JobId::generate();
        assert_eq!(JobId::parse(id.as_str()), Some(id));
        for bad in [
            "",
            "../../etc/passwd",
            "0123456789abcdeF",
            "0123456789abcde",
            "0123456789abcdefg",
        ] {
            assert_eq!(JobId::parse(bad), None, "{bad}");
        }
        assert!(serde_json::from_str::<JobId>("\"../x\"").is_err());
    }

    #[test]
    fn job_ids_sort_by_creation() {
        let a = JobId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = JobId::generate();
        assert!(a < b);
    }

    #[test]
    fn summary_hides_env_values_and_joins_lines() {
        let mut spec = JobSpec::shell("cd app\nflutter build windows");
        spec.env
            .push(("SHOREBIRD_TOKEN".into(), "secret-value".into()));
        let s = spec.summary();
        assert!(s.contains("cd app ⏎ flutter build windows"));
        assert!(s.contains("SHOREBIRD_TOKEN"));
        assert!(!s.contains("secret-value"));
    }

    #[test]
    fn store_creates_lists_and_reads_logs() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        let m = meta(JobSpec::shell("echo hi"));
        store.create(&m).unwrap();
        std::fs::write(store.job_dir(&m.id).join(LOG), b"hello world").unwrap();

        let info = store.info(&m.id).unwrap();
        assert_eq!(info.status.state, JobState::Queued);
        assert_eq!(info.log_bytes, 11);
        assert_eq!(store.read_log(&m.id, 6, 100).unwrap(), b"world");
        assert_eq!(store.read_log(&m.id, 0, 5).unwrap(), b"hello");
        assert_eq!(store.read_log(&m.id, 50, 5).unwrap(), b"");
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn prune_keeps_newest_and_unfinished() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        let mut ids = Vec::new();
        for _ in 0..4 {
            let m = meta(JobSpec::shell("x"));
            store.create(&m).unwrap();
            ids.push(m.id);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Oldest stays queued (unfinished); the rest are finished.
        for id in &ids[1..] {
            store
                .write_status(id, &JobStatus::queued().finish(JobState::Succeeded, None))
                .unwrap();
        }
        store.prune(1);
        let left: Vec<_> = store.list().into_iter().map(|j| j.meta.id).collect();
        assert_eq!(left, vec![ids[3].clone(), ids[0].clone()]);
    }
}
