//! What the Yonder app can do, for Flutter. Wraps the core's controller.
//!
//! The app opens this device's controller and hosts its connections, so the CLI goes
//! through the app while it's open (see `yonder_core::controller::Controller::host`).
//! Everything here runs on one tokio runtime, which the network code needs.

use std::future::Future;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result, anyhow};
use flutter_rust_bridge::frb;
use tokio::sync::Mutex;
use yonder_core::controller::{
    Controller, ControllerConfig, TerminalEvent, TerminalReader, TerminalWriter,
};
use yonder_core::footprint::{DiskEncryption, FootprintItem};
use yonder_core::jobs::{JobInfo, JobId, JobSpec};
use yonder_core::net::NetConfig;
use yonder_core::secrets::SecretName;
use yonder_core::store::{PairedMachine, StateDir, default_controller_dir};
use yonder_core::terminal::SessionId;
use yonder_core::ticket::PairingTicket;
use yonder_core::workspace::WorkspaceId;

use crate::frb_generated::StreamSink;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("yonder")
        .build()
        .expect("the tokio runtime starts")
});

/// Runs `work` on the shared runtime.
async fn on_runtime<T: Send + 'static>(
    work: impl Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    RUNTIME.spawn(work).await.context("the task stopped")?
}

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// A paired machine, as the app lists it.
#[derive(Debug, Clone)]
pub struct Machine {
    pub id: String,
    pub short_id: String,
    pub name: String,
    /// `win`, `mac` or `linux`.
    pub os: String,
    pub permissions: Vec<String>,
    /// Unix seconds.
    pub paired_at: u64,
}

/// What a machine says about itself right now.
#[derive(Debug, Clone)]
pub struct MachineStatus {
    pub name: String,
    pub agent_version: String,
    pub paused: bool,
    pub running_jobs: u32,
    pub queued_jobs: u32,
}

/// One job.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub command: String,
    pub label: Option<String>,
    pub cwd: Option<String>,
    pub started_by: String,
    /// queued, running, succeeded, failed, cancelled or lost.
    pub state: String,
    pub exit_code: Option<i32>,
    pub message: Option<String>,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub log_bytes: u64,
}

/// Part of a job's log, or its end.
#[derive(Debug, Clone)]
pub enum LogEvent {
    Output { offset: u64, data: Vec<u8> },
    Finished { job: Job },
    /// Git in the job borrowed a login from this device (`--borrow-git` jobs only).
    LentGitLogin { host: String, lent: bool },
}

/// Output of an open shell.
#[derive(Debug, Clone)]
pub enum ShellEvent {
    Output { offset: u64, data: Vec<u8> },
    Exited { code: Option<u32> },
    /// The connection dropped; reopen with `from` set to the last offset received.
    Disconnected { message: String },
}

#[derive(Debug, Clone)]
pub struct ShellInfo {
    pub id: String,
    pub started_by: String,
    pub created_at_ms: u64,
    pub exited: bool,
    pub attached: u32,
}

#[derive(Debug, Clone)]
pub struct Secret {
    pub name: String,
    pub size: u64,
    pub created_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub last_used_ms: Option<u64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectCopy {
    pub id: String,
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub last_synced_ms: Option<u64>,
}

/// What this device has left on a machine.
#[derive(Debug, Clone)]
pub struct Footprint {
    pub secrets: Vec<Secret>,
    pub projects: Vec<ProjectCopy>,
    pub jobs: u32,
    pub unfinished_jobs: u32,
    pub job_bytes: u64,
    pub shells: Vec<ShellInfo>,
    pub cleanup_after_days: Option<u32>,
    /// `on`, `off` or `unknown`.
    pub disk_encryption: String,
}

#[derive(Debug, Clone)]
pub struct Removed {
    pub secrets: u32,
    pub projects: u32,
    pub jobs: u32,
    pub shells: u32,
    pub bytes: u64,
    pub kept: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub permissions: Vec<String>,
    pub paired_at: u64,
    pub is_this_device: bool,
}

fn machine(m: &PairedMachine) -> Machine {
    Machine {
        id: m.id.to_string(),
        short_id: m.id.fmt_short().to_string(),
        name: m.name.clone(),
        os: m.os.short_name().to_string(),
        permissions: m.permissions.iter().map(|p| p.name().to_string()).collect(),
        paired_at: m.paired_at,
    }
}

fn job(j: JobInfo) -> Job {
    Job {
        id: j.meta.id.to_string(),
        command: j.meta.spec.summary(),
        label: j.meta.spec.label.clone(),
        cwd: j.meta.spec.cwd.clone(),
        started_by: j.meta.controller_name.clone(),
        state: j.status.state.name().to_string(),
        exit_code: j.status.exit_code,
        message: j.status.message.clone(),
        created_at_ms: j.meta.created_at_ms,
        started_at_ms: j.status.started_at_ms,
        finished_at_ms: j.status.finished_at_ms,
        log_bytes: j.log_bytes,
    }
}

fn removed(r: yonder_core::footprint::Removed) -> Removed {
    Removed {
        secrets: r.secrets as u32,
        projects: r.workspaces as u32,
        jobs: r.jobs as u32,
        shells: r.shells as u32,
        bytes: r.bytes,
        kept: r.kept,
    }
}

fn job_id(text: &str) -> Result<JobId> {
    JobId::parse(text).ok_or_else(|| anyhow!("\"{text}\" isn't a job id"))
}

/// This device as a controller, opened by the app.
#[frb(opaque)]
pub struct YonderApp {
    controller: Arc<Controller>,
    hosting: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// An open shell; dropping it detaches (the shell keeps running).
#[frb(opaque)]
pub struct Shell {
    id: String,
    writer: Arc<Mutex<Option<TerminalWriter>>>,
    reader: Arc<Mutex<Option<TerminalReader>>>,
}

impl YonderApp {
    /// Opens this device's controller (the CLI's folder unless `state_dir` says
    /// otherwise) and hosts its connections for the CLI.
    pub async fn open(state_dir: Option<String>) -> Result<YonderApp> {
        on_runtime(async move {
            let dir = match state_dir {
                Some(dir) => dir.into(),
                None => default_controller_dir().context("no home folder")?,
            };
            let controller = Arc::new(
                Controller::open(ControllerConfig {
                    state: StateDir::new(dir),
                    net: NetConfig::default(),
                    name: None,
                })
                .await?,
            );
            // Another app window or process may host already; then this one goes
            // through it like the CLI does.
            #[cfg(unix)]
            let hosting = controller.host().ok();
            #[cfg(not(unix))]
            let hosting = None;
            Ok(YonderApp {
                controller,
                hosting: Mutex::new(hosting),
            })
        })
        .await
    }

    #[frb(sync)]
    pub fn device_name(&self) -> String {
        self.controller.name().to_string()
    }

    #[frb(sync)]
    pub fn fingerprint(&self) -> String {
        self.controller.fingerprint()
    }

    /// Whether this app holds the device's connections (false if another one does).
    pub async fn is_hosting(&self) -> bool {
        self.hosting.lock().await.is_some()
    }

    /// Paired machines, read fresh (the CLI may have paired or forgotten some).
    #[frb(sync)]
    pub fn machines(&self) -> Vec<Machine> {
        let _ = self.controller.reload();
        self.controller.machines().iter().map(machine).collect()
    }

    fn find(&self, id: &str) -> Result<PairedMachine> {
        Ok(self.controller.find_machine(id)?)
    }

    /// Asks a machine how it is; fails if it can't be reached.
    pub async fn status(&self, machine_id: String) -> Result<MachineStatus> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move {
            let info = c.info(&m).await?;
            let jobs = c.jobs(&m).await.unwrap_or_default();
            let count = |s: &str| jobs.iter().filter(|j| j.status.state.name() == s).count() as u32;
            Ok(MachineStatus {
                name: info.name,
                agent_version: info.agent_version,
                paused: info.paused,
                running_jobs: count("running"),
                queued_jobs: count("queued"),
            })
        })
        .await
    }

    pub async fn jobs(&self, machine_id: String) -> Result<Vec<Job>> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move { Ok(c.jobs(&m).await?.into_iter().map(job).collect()) }).await
    }

    /// Starts `command` in the machine's shell.
    pub async fn run_job(
        &self,
        machine_id: String,
        command: String,
        cwd: Option<String>,
        label: Option<String>,
    ) -> Result<Job> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move {
            let mut spec = JobSpec::shell(command);
            spec.cwd = cwd.filter(|c| !c.trim().is_empty());
            spec.label = label.filter(|l| !l.trim().is_empty());
            Ok(job(c.run(&m, spec).await?))
        })
        .await
    }

    pub async fn cancel_job(&self, machine_id: String, job_id: String) -> Result<Job> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        let id = self::job_id(&job_id)?;
        on_runtime(async move { Ok(job(c.cancel(&m, &id).await?)) }).await
    }

    /// Streams a job's log from byte `from` until it finishes.
    pub async fn follow_job(
        &self,
        machine_id: String,
        job_id: String,
        from: u64,
        sink: StreamSink<LogEvent>,
    ) -> Result<()> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        let id = self::job_id(&job_id)?;
        on_runtime(async move {
            let (requests, mut asked) = tokio::sync::mpsc::unbounded_channel();
            let lend = {
                let (c, m, id, sink) = (c.clone(), m.clone(), id.clone(), sink.clone());
                async move {
                    while let Some(request) =
                        asked.recv().await as Option<yonder_core::credentials::GitCredentialRequest>
                    {
                        let host = request.describe();
                        let credential = git_fill(&request).await;
                        let lent = credential.is_some();
                        let _ = c
                            .answer_git_credential(&m, &id, request.id, credential)
                            .await;
                        let _ = sink.add(LogEvent::LentGitLogin { host, lent });
                    }
                }
            };
            let lending = tokio::spawn(lend);
            let result = c
                .logs_lending(
                    &m,
                    &id,
                    from,
                    true,
                    |offset, data| {
                        let _ = sink.add(LogEvent::Output {
                            offset,
                            data: data.to_vec(),
                        });
                    },
                    |request| {
                        let _ = requests.send(request);
                    },
                )
                .await;
            lending.abort();
            let finished = result?;
            let _ = sink.add(LogEvent::Finished { job: job(finished) });
            Ok(())
        })
        .await
    }

    /// Opens a new shell (`session` empty) or attaches to one, replaying output from
    /// byte `from` (default: all it kept). Read its output with [`Shell::events`].
    pub async fn open_shell(
        &self,
        machine_id: String,
        session: Option<String>,
        cols: u16,
        rows: u16,
        from: Option<u64>,
    ) -> Result<Shell> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        let session = match session {
            Some(text) => {
                Some(SessionId::parse(&text).ok_or_else(|| anyhow!("\"{text}\" isn't a shell id"))?)
            }
            None => None,
        };
        on_runtime(async move {
            let (info, writer, reader) = c.open_terminal(&m, session, cols, rows, from).await?;
            Ok(Shell {
                id: info.id.to_string(),
                writer: Arc::new(Mutex::new(Some(writer))),
                reader: Arc::new(Mutex::new(Some(reader))),
            })
        })
        .await
    }

    pub async fn shells(&self, machine_id: String) -> Result<Vec<ShellInfo>> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move {
            Ok(c.terminals(&m)
                .await?
                .into_iter()
                .map(|t| ShellInfo {
                    id: t.id.to_string(),
                    started_by: t.started_by,
                    created_at_ms: t.created_at_ms,
                    exited: t.exited,
                    attached: t.attached as u32,
                })
                .collect())
        })
        .await
    }

    pub async fn footprint(&self, machine_id: String) -> Result<Footprint> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move {
            let fp = c.footprint(&m).await?;
            Ok(Footprint {
                secrets: fp
                    .secrets
                    .into_iter()
                    .map(|s| Secret {
                        name: s.name.to_string(),
                        size: s.size,
                        created_at_ms: s.created_at_ms,
                        expires_at_ms: s.expires_at_ms,
                        last_used_ms: s.last_used_ms,
                        note: s.note,
                    })
                    .collect(),
                projects: fp
                    .workspaces
                    .into_iter()
                    .map(|w| ProjectCopy {
                        id: w.id.to_string(),
                        path: w.path,
                        files: w.files,
                        bytes: w.bytes,
                        last_synced_ms: w.last_synced_ms,
                    })
                    .collect(),
                jobs: fp.jobs as u32,
                unfinished_jobs: fp.unfinished_jobs as u32,
                job_bytes: fp.job_bytes,
                shells: fp
                    .shells
                    .into_iter()
                    .map(|t| ShellInfo {
                        id: t.id.to_string(),
                        started_by: t.started_by,
                        created_at_ms: t.created_at_ms,
                        exited: t.exited,
                        attached: t.attached as u32,
                    })
                    .collect(),
                cleanup_after_days: fp.cleanup_after_days,
                disk_encryption: match fp.disk_encryption {
                    DiskEncryption::On => "on",
                    DiskEncryption::Off => "off",
                    DiskEncryption::Unknown => "unknown",
                }
                .into(),
            })
        })
        .await
    }

    /// Removes part of this device's footprint: `everything`, `jobs`, `shells`,
    /// `secret:NAME` or `project:ID`.
    pub async fn clean(&self, machine_id: String, what: String) -> Result<Removed> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        let item = match what.as_str() {
            "everything" => FootprintItem::Everything,
            "jobs" => FootprintItem::Jobs,
            "shells" => FootprintItem::Shells,
            other => match other.split_once(':') {
                Some(("secret", name)) => FootprintItem::Secret(
                    SecretName::parse(name).ok_or_else(|| anyhow!("bad secret name"))?,
                ),
                Some(("project", id)) => FootprintItem::Workspace(
                    WorkspaceId::parse(id).ok_or_else(|| anyhow!("bad project id"))?,
                ),
                _ => return Err(anyhow!("don't know how to remove \"{what}\"")),
            },
        };
        on_runtime(async move { Ok(removed(c.remove_footprint(&m, item).await?)) }).await
    }

    /// Turns auto-cleanup on (`Some(days)`) or off.
    pub async fn set_cleanup(&self, machine_id: String, after_days: Option<u32>) -> Result<()> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move { Ok(c.set_cleanup(&m, after_days).await?) }).await
    }

    /// Pairs with a machine using the ticket `yonder-agent pair` showed; the machine's
    /// owner approves it there after comparing [`YonderApp::fingerprint`].
    pub async fn pair(&self, ticket: String) -> Result<Machine> {
        let c = self.controller.clone();
        on_runtime(async move {
            let ticket = PairingTicket::decode(ticket.trim())?;
            Ok(machine(&c.pair(&ticket).await?))
        })
        .await
    }

    /// Leaves a machine, removing everything of this device's there unless `keep`.
    pub async fn unpair(&self, machine_id: String, keep: bool) -> Result<Removed> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move { Ok(removed(c.unpair(&m, !keep).await?)) }).await
    }

    pub async fn devices(&self, machine_id: String) -> Result<Vec<Device>> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        on_runtime(async move {
            Ok(c.devices(&m)
                .await?
                .into_iter()
                .map(|d| Device {
                    id: d.id.to_string(),
                    short_id: d.id.fmt_short().to_string(),
                    name: d.name,
                    permissions: d.permissions.iter().map(|p| p.name().to_string()).collect(),
                    paired_at: d.paired_at,
                    is_this_device: d.you,
                })
                .collect())
        })
        .await
    }

    /// Cuts another device off a machine, removing what it left unless `keep`.
    pub async fn revoke(&self, machine_id: String, device_id: String, keep: bool) -> Result<Removed> {
        let (c, m) = (self.controller.clone(), self.find(&machine_id)?);
        let device = device_id
            .parse()
            .map_err(|_| anyhow!("\"{device_id}\" isn't a device id"))?;
        on_runtime(async move { Ok(removed(c.revoke_device(&m, device, !keep).await?)) }).await
    }
}

impl Shell {
    #[frb(sync)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// The shell's output until it exits or the connection drops. Call once.
    pub async fn events(&self, sink: StreamSink<ShellEvent>) -> Result<()> {
        let reader = self.reader.clone();
        on_runtime(async move {
            let mut reader = reader
                .lock()
                .await
                .take()
                .ok_or_else(|| anyhow!("the shell's output is already being read"))?;
            loop {
                let event = match reader.next().await {
                    Ok(TerminalEvent::Output { offset, data }) => ShellEvent::Output { offset, data },
                    Ok(TerminalEvent::Exit(code)) => {
                        let _ = sink.add(ShellEvent::Exited { code });
                        return Ok(());
                    }
                    Err(err) => {
                        let _ = sink.add(ShellEvent::Disconnected {
                            message: err.to_string(),
                        });
                        return Ok(());
                    }
                };
                if sink.add(event).is_err() {
                    return Ok(());
                }
            }
        })
        .await
    }

    pub async fn send(&self, data: Vec<u8>) -> Result<()> {
        let writer = self.writer.clone();
        on_runtime(async move {
            match writer.lock().await.as_mut() {
                Some(w) => Ok(w.send(&data).await?),
                None => Err(anyhow!("the shell was detached")),
            }
        })
        .await
    }

    pub async fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let writer = self.writer.clone();
        on_runtime(async move {
            if let Some(w) = writer.lock().await.as_mut() {
                w.resize(cols, rows).await?;
            }
            Ok(())
        })
        .await
    }

    /// Leaves the shell running on the machine.
    pub async fn detach(&self) -> Result<()> {
        let writer = self.writer.clone();
        on_runtime(async move {
            if let Some(w) = writer.lock().await.take() {
                w.detach();
            }
            Ok(())
        })
        .await
    }
}

/// This computer's own git login for a job's request (keychain), without prompting.
async fn git_fill(
    request: &yonder_core::credentials::GitCredentialRequest,
) -> Option<yonder_core::credentials::GitCredential> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("git")
        .args(["credential", "fill"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    stdin.write_all(request.to_git_input().as_bytes()).await.ok()?;
    drop(stdin);
    let output = child.wait_with_output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    yonder_core::credentials::GitCredential::from_git_output(&String::from_utf8_lossy(
        &output.stdout,
    ))
}
