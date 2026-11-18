//! Agent side of jobs: queueing, starting runners, cancelling, and picking up jobs that
//! kept running while the agent was restarted.

use std::collections::{HashSet, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tracing::{debug, info, warn};

use super::sys;
use super::{JobId, JobInfo, JobMeta, JobState, JobStatus, JobStore};
use crate::error::Result;
use crate::secrets::Resolved;

/// How long a runner gets to stop after a cancel request before it's killed.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
/// Finished jobs kept on disk.
const KEEP_JOBS: usize = 200;

/// How to start a runner: `program args… <job folder>`.
#[derive(Debug, Clone)]
pub struct RunnerCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl RunnerCommand {
    /// `<yonder-agent> job-run <job folder>`.
    pub fn agent_binary(program: impl Into<PathBuf>) -> Self {
        RunnerCommand {
            program: program.into(),
            args: vec!["job-run".into()],
        }
    }
}

/// Settings for running jobs on an agent.
#[derive(Debug, Clone)]
pub struct JobsConfig {
    pub runner: RunnerCommand,
    /// Jobs running at once; the rest wait in a queue.
    pub max_parallel: usize,
}

#[derive(Default)]
struct Slots {
    running: HashSet<JobId>,
    queue: VecDeque<JobId>,
}

/// Owns the job queue on an agent.
pub struct JobManager {
    store: JobStore,
    config: JobsConfig,
    slots: Mutex<Slots>,
}

impl std::fmt::Debug for JobManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobManager")
            .field("dir", &self.store.dir())
            .finish()
    }
}

impl JobManager {
    /// Picks up jobs left from earlier runs and starts queued ones.
    pub fn start(store: JobStore, config: JobsConfig) -> Arc<JobManager> {
        let manager = Arc::new(JobManager {
            store,
            config,
            slots: Mutex::new(Slots::default()),
        });
        manager.recover();
        manager.pump();
        manager
    }

    pub fn store(&self) -> &JobStore {
        &self.store
    }

    fn slots(&self) -> MutexGuard<'_, Slots> {
        self.slots.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queues a new job and starts it if there's a free slot. `secrets` are the values of
    /// the secrets it asked for; only its runner reads them.
    pub fn submit(self: &Arc<Self>, meta: JobMeta, secrets: &[Resolved]) -> Result<JobInfo> {
        self.store.create(&meta)?;
        if !secrets.is_empty() {
            self.store.write_secrets(&meta.id, secrets)?;
        }
        self.slots().queue.push_back(meta.id.clone());
        info!(job = %meta.id, controller = %meta.controller_name, "job queued");
        self.pump();
        let mut info = self.store.info(&meta.id)?;
        // A runner was started for it but hasn't recorded "running" yet: say so, rather
        // than make the caller think it's waiting behind other jobs.
        if info.status.state == JobState::Queued && self.slots().running.contains(&meta.id) {
            info.status.state = JobState::Running;
        }
        Ok(info)
    }

    /// Cancels a queued or running job. Finished jobs are left as they are.
    pub fn cancel(self: &Arc<Self>, id: &JobId) -> Result<JobInfo> {
        let info = self.store.info(id)?;
        match info.status.state {
            JobState::Queued => {
                self.slots().queue.retain(|q| q != id);
                self.store
                    .write_status(id, &info.status.finish(JobState::Cancelled, None))?;
                self.store.scrub(id);
            }
            JobState::Running => {
                self.store.request_cancel(id)?;
                let manager = self.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(CANCEL_GRACE).await;
                    manager.force_cancel(&id);
                });
            }
            _ => {}
        }
        self.store.info(id)
    }

    /// The runner ignored the cancel request: kill it and record the cancellation.
    fn force_cancel(&self, id: &JobId) {
        let Ok(info) = self.store.info(id) else {
            return;
        };
        if info.status.state.is_finished() {
            return;
        }
        if let Some(pid) = info.status.runner_pid {
            warn!(job = %id, "runner didn't stop in time; killing it");
            sys::kill_process(pid);
        }
        let _ = self
            .store
            .write_status(id, &info.status.finish(JobState::Cancelled, None));
    }

    /// Starts queued jobs while there are free slots.
    fn pump(self: &Arc<Self>) {
        loop {
            let next = {
                let mut slots = self.slots();
                if slots.running.len() >= self.config.max_parallel.max(1) {
                    return;
                }
                let Some(id) = slots.queue.pop_front() else {
                    return;
                };
                slots.running.insert(id.clone());
                id
            };
            self.launch(next);
        }
    }

    fn launch(self: &Arc<Self>, id: JobId) {
        let job_dir = self.store.job_dir(&id);
        let mut command = tokio::process::Command::new(&self.config.runner.program);
        command
            .args(&self.config.runner.args)
            .arg(&job_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        detach(&mut command);

        match command.spawn() {
            Ok(mut child) => {
                debug!(job = %id, pid = ?child.id(), "runner started");
                let manager = self.clone();
                tokio::spawn(async move {
                    let exit = child.wait().await;
                    manager.runner_exited(&id, exit.ok().and_then(|s| s.code()));
                });
            }
            Err(err) => {
                warn!(job = %id, "couldn't start runner: {err}");
                if let Ok(info) = self.store.info(&id) {
                    let message = format!("couldn't start the job runner: {err}");
                    let _ = self
                        .store
                        .write_status(&id, &info.status.finish(JobState::Failed, Some(message)));
                }
                self.slots().running.remove(&id);
            }
        }
    }

    /// Called when a runner ends. If it didn't record a result, the job is lost.
    fn runner_exited(self: &Arc<Self>, id: &JobId, code: Option<i32>) {
        if let Ok(info) = self.store.info(id)
            && !info.status.state.is_finished()
        {
            let message = format!("the job runner exited unexpectedly (code {code:?})");
            warn!(job = %id, "{message}");
            let _ = self
                .store
                .write_status(id, &info.status.finish(JobState::Lost, Some(message)));
        }
        // Whatever the runner left, including after a crash.
        self.store.scrub(id);
        if let Ok(info) = self.store.info(id) {
            info!(job = %id, state = %info.status.state, "job finished");
        }
        self.slots().running.remove(id);
        self.store.prune(KEEP_JOBS);
        self.pump();
    }

    /// Looks at jobs left by an earlier agent run.
    fn recover(self: &Arc<Self>) {
        let boot = sys::boot_time_ms();
        let mut queued = Vec::new();
        for job in self.store.list() {
            let id = job.meta.id.clone();
            match job.status.state {
                JobState::Queued => queued.push(id),
                JobState::Running => {
                    let started_before_boot = matches!(
                        (boot, job.status.started_at_ms),
                        (Some(boot), Some(started)) if started < boot
                    );
                    let alive = !started_before_boot
                        && job.status.runner_pid.is_some_and(sys::process_alive);
                    if alive {
                        info!(job = %id, "job still running from before the agent restarted");
                        self.slots().running.insert(id.clone());
                        self.watch_adopted(id, job.status.runner_pid.unwrap_or_default());
                    } else {
                        let message = if started_before_boot {
                            "the machine restarted while the job was running"
                        } else {
                            "the job runner stopped without recording a result"
                        };
                        let _ = self.store.write_status(
                            &id,
                            &job.status.finish(JobState::Lost, Some(message.into())),
                        );
                        self.store.scrub(&id);
                    }
                }
                _ => {}
            }
        }
        // list() is newest first; queue oldest first.
        queued.reverse();
        self.slots().queue.extend(queued);
    }

    /// Waits for a runner this agent didn't start.
    fn watch_adopted(self: &Arc<Self>, id: JobId, pid: u32) {
        let manager = self.clone();
        tokio::spawn(async move {
            while sys::process_alive(pid) {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            manager.runner_exited(&id, None);
        });
    }

    /// Current state of all jobs, newest first.
    pub fn list(&self) -> Vec<JobInfo> {
        self.store.list()
    }

    /// Whether any job is queued or running.
    pub fn busy(&self) -> bool {
        let slots = self.slots();
        !slots.running.is_empty() || !slots.queue.is_empty()
    }
}

/// Keeps a runner out of the agent's console group, so Ctrl-C on the agent's terminal or
/// the agent stopping doesn't stop running jobs.
fn detach(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
}

impl JobStatus {
    /// Convenience for tests and tooling.
    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }
}
