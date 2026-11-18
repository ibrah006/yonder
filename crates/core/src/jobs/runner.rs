//! The runner: a small process that owns one job from start to finish.
//!
//! Started by the agent as `yonder-agent job-run <job folder>`. It runs the command with
//! its output going straight into `output.log`, watches for a cancel request, and records
//! the result in `status.json`. It doesn't depend on the agent staying up.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::protocol::now_ms;

use super::sys::{self, KeepAwake, TreeGuard};
use super::{CANCEL, JobMeta, JobProgram, JobSpec, JobState, JobStatus, LOG, META, STATUS};
use super::{read_json, remove_secret_files, take_secrets, write_json, write_secret_files};
use crate::secrets::{Resolved, SecretTarget};

const POLL: Duration = Duration::from_millis(200);

/// Runs the job in `job_dir` to completion and returns how it ended.
pub fn run(job_dir: &Path) -> Result<JobState> {
    let meta: JobMeta = read_json(&job_dir.join(META))?;
    let status_path = job_dir.join(STATUS);
    let mut status: JobStatus = read_json(&status_path).unwrap_or_else(|_| JobStatus::queued());
    if status.state.is_finished() {
        // Cancelled while queued.
        return Ok(status.state);
    }

    sys::lower_own_priority();
    // Must happen before the command starts, so the command lands in the same Job Object.
    let tree = TreeGuard::new().ok();

    let log_path = job_dir.join(LOG);
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| Error::io(&log_path, e))?;

    status.state = JobState::Running;
    status.runner_pid = Some(std::process::id());
    status.started_at_ms = Some(now_ms());
    write_json(&status_path, &status)?;

    // Taken before anything else can fail, so the values don't stay on disk.
    let secrets = take_secrets(job_dir);
    // Secret files go when the job ends, however it ends.
    let _secret_files = SecretFiles(job_dir);
    let started = secrets.map_err(std::io::Error::other).and_then(|secrets| {
        let command = build_command(&meta.spec, &log, &secrets)?;
        if secrets
            .iter()
            .any(|s| matches!(s.target, SecretTarget::File(_)))
        {
            let cwd = job_cwd(&meta.spec).ok_or_else(|| {
                std::io::Error::other("secret files need a working folder for the job")
            })?;
            write_secret_files(job_dir, &cwd, &secrets).map_err(std::io::Error::other)?;
        }
        Ok(command)
    });
    let mut child = match started {
        Ok(mut command) => match command.spawn() {
            Ok(child) => child,
            Err(err) => return fail_to_start(&mut log, &status_path, status, err),
        },
        Err(err) => return fail_to_start(&mut log, &status_path, status, err),
    };
    let _awake = KeepAwake::while_running(child.id());

    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                let status = finished(status, exit);
                let state = status.state;
                write_json(&status_path, &status)?;
                return Ok(state);
            }
            Ok(None) => {}
            Err(err) => {
                let status = status.finish(
                    JobState::Lost,
                    Some(format!("lost track of the command: {err}")),
                );
                write_json(&status_path, &status)?;
                return Ok(JobState::Lost);
            }
        }
        if job_dir.join(CANCEL).exists() {
            let _ = writeln!(log, "\n[yonder] cancelled");
            let status = status.finish(JobState::Cancelled, None);
            // Record and clean up first: on Windows stopping the tree also ends this runner.
            write_json(&status_path, &status)?;
            remove_secret_files(job_dir);
            match &tree {
                Some(tree) => tree.stop(&mut child),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            return Ok(JobState::Cancelled);
        }
        std::thread::sleep(POLL);
    }
}

fn fail_to_start(
    log: &mut std::fs::File,
    status_path: &Path,
    status: JobStatus,
    err: std::io::Error,
) -> Result<JobState> {
    let message = format!("couldn't start the command: {err}");
    let _ = writeln!(log, "[yonder] {message}");
    write_json(status_path, &status.finish(JobState::Failed, Some(message)))?;
    Ok(JobState::Failed)
}

fn finished(status: JobStatus, exit: ExitStatus) -> JobStatus {
    match exit.code() {
        Some(0) => {
            let mut s = status.finish(JobState::Succeeded, None);
            s.exit_code = Some(0);
            s
        }
        Some(code) => {
            let mut s = status.finish(JobState::Failed, None);
            s.exit_code = Some(code);
            s
        }
        None => status.finish(JobState::Failed, Some(describe_signal(exit))),
    }
}

#[cfg(unix)]
fn describe_signal(exit: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match exit.signal() {
        Some(signal) => format!("stopped by signal {signal}"),
        None => "ended without an exit code".to_string(),
    }
}

#[cfg(not(unix))]
fn describe_signal(_exit: ExitStatus) -> String {
    "ended without an exit code".to_string()
}

/// Deletes the job's secret files when dropped.
struct SecretFiles<'a>(&'a Path);

impl Drop for SecretFiles<'_> {
    fn drop(&mut self) {
        remove_secret_files(self.0);
    }
}

/// Where the job runs: the folder it asked for, or the home folder.
fn job_cwd(spec: &JobSpec) -> Option<PathBuf> {
    match &spec.cwd {
        Some(cwd) => Some(PathBuf::from(cwd)),
        None => directories::BaseDirs::new().map(|d| d.home_dir().to_owned()),
    }
}

fn build_command(
    spec: &JobSpec,
    log: &std::fs::File,
    secrets: &[Resolved],
) -> std::io::Result<Command> {
    let mut command = match &spec.program {
        JobProgram::Shell(script) => shell_command(script),
        JobProgram::Exec { program, args } => {
            let mut c = Command::new(program);
            c.args(args);
            c
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .envs(spec.env.iter().map(|(k, v)| (k, v)));
    for secret in secrets {
        if let SecretTarget::Env(name) = &secret.target {
            let value = std::str::from_utf8(&secret.value).map_err(|_| {
                std::io::Error::other(format!(
                    "the secret for ${name} isn't text; use it as a file instead"
                ))
            })?;
            command.env(name, value);
        }
    }
    if let Some(cwd) = job_cwd(spec) {
        command.current_dir(cwd);
    }
    sys::prepare_command(&mut command);
    Ok(command)
}

#[cfg(windows)]
fn shell_command(script: &str) -> Command {
    let mut c = Command::new("powershell.exe");
    // UTF-8 output and no progress bars: logs are files, not a console.
    let prelude = "$ProgressPreference='SilentlyContinue'; \
                   [Console]::OutputEncoding=[System.Text.Encoding]::UTF8; ";
    c.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
    ])
    .arg(format!("{prelude}{script}"));
    c
}

#[cfg(not(windows))]
fn shell_command(script: &str) -> Command {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(script);
    c
}

#[cfg(test)]
mod tests {
    use super::super::tests::meta;
    use super::super::{JobSpec, JobStore};
    use super::*;

    fn run_spec(spec: JobSpec) -> (JobStore, crate::jobs::JobId, JobState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        let m = meta(spec);
        let job_dir = store.create(&m).unwrap();
        let state = run(&job_dir).unwrap();
        (store, m.id, state, dir)
    }

    fn log(store: &JobStore, id: &crate::jobs::JobId) -> String {
        String::from_utf8(store.read_log(id, 0, 1 << 20).unwrap()).unwrap()
    }

    #[test]
    fn successful_command_records_output_and_exit_code() {
        let (store, id, state, _dir) = run_spec(JobSpec::shell("echo hello"));
        assert_eq!(state, JobState::Succeeded);
        let info = store.info(&id).unwrap();
        assert_eq!(info.status.exit_code, Some(0));
        assert!(info.status.finished_at_ms.is_some());
        assert!(log(&store, &id).contains("hello"));
    }

    #[test]
    fn failing_command_records_exit_code() {
        let (store, id, state, _dir) = run_spec(JobSpec::shell("echo oops; exit 3"));
        assert_eq!(state, JobState::Failed);
        assert_eq!(store.info(&id).unwrap().status.exit_code, Some(3));
        assert!(log(&store, &id).contains("oops"));
    }

    #[test]
    fn stderr_goes_to_the_same_log() {
        let script = if cfg!(windows) {
            "[Console]::Error.WriteLine('to-stderr')"
        } else {
            "echo to-stderr >&2"
        };
        let (store, id, _, _dir) = run_spec(JobSpec::shell(script));
        assert!(log(&store, &id).contains("to-stderr"));
    }

    #[test]
    fn env_and_cwd_are_applied() {
        let cwd = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) {
            "Write-Output \"$env:YONDER_TEST|$(Get-Location)\""
        } else {
            "echo \"$YONDER_TEST|$(pwd)\""
        };
        let mut spec = JobSpec::shell(script);
        spec.env.push(("YONDER_TEST".into(), "it-works".into()));
        spec.cwd = Some(cwd.path().to_string_lossy().into_owned());
        let (store, id, _, _dir) = run_spec(spec);
        let out = log(&store, &id);
        assert!(out.contains("it-works|"), "{out}");
        let dir_name = cwd.path().file_name().unwrap().to_string_lossy();
        assert!(out.contains(dir_name.as_ref()), "{out}");
    }

    #[test]
    fn missing_program_fails_with_a_message() {
        let spec = JobSpec {
            program: JobProgram::Exec {
                program: "yonder-no-such-program".into(),
                args: vec![],
            },
            cwd: None,
            env: vec![],
            label: None,
            secrets: vec![],
        };
        let (store, id, state, _dir) = run_spec(spec);
        assert_eq!(state, JobState::Failed);
        let info = store.info(&id).unwrap();
        assert!(info.status.message.unwrap().contains("couldn't start"));
        assert!(log(&store, &id).contains("couldn't start"));
    }

    #[test]
    fn job_cancelled_while_queued_does_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        let m = meta(JobSpec::shell("echo should-not-run"));
        let job_dir = store.create(&m).unwrap();
        store
            .write_status(
                &m.id,
                &JobStatus::queued().finish(JobState::Cancelled, None),
            )
            .unwrap();
        assert_eq!(run(&job_dir).unwrap(), JobState::Cancelled);
        assert!(!log(&store, &m.id).contains("should-not-run"));
    }

    // On Windows, cancelling ends the runner's own process (Job Object), so this is
    // tested end to end in the agent crate instead.
    #[cfg(unix)]
    #[test]
    fn cancel_stops_the_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        // A child that starts its own background grandchild, then waits.
        let m = meta(JobSpec::shell("sleep 30 & echo started; wait"));
        let job_dir = store.create(&m).unwrap();
        let runner = std::thread::spawn({
            let job_dir = job_dir.clone();
            move || run(&job_dir).unwrap()
        });
        while !log(&store, &m.id).contains("started") {
            std::thread::sleep(Duration::from_millis(50));
        }
        let started = std::time::Instant::now();
        store.request_cancel(&m.id).unwrap();
        assert_eq!(runner.join().unwrap(), JobState::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(store.info(&m.id).unwrap().status.state, JobState::Cancelled);
        assert!(log(&store, &m.id).contains("[yonder] cancelled"));
    }
}
