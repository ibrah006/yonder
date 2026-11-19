//! End-to-end job tests: a real agent whose jobs run in real `yonder-agent job-run`
//! processes, driven by a real controller over loopback QUIC.
//!
//! The scripts below are valid in both `sh` and PowerShell (`echo`, `sleep`, `exit`), so
//! the same tests run on every OS in CI.

use std::time::{Duration, Instant};

use tempfile::TempDir;
use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::jobs::manager::{JobsConfig, RunnerCommand};
use yonder_core::jobs::{JobId, JobInfo, JobSpec, JobState};
use yonder_core::net::NetConfig;
use yonder_core::permissions::Permission;
use yonder_core::protocol::ErrorCode;
use yonder_core::store::{AgentState, PairedMachine, StateDir};

const AGENT_BIN: &str = env!("CARGO_BIN_EXE_yonder-agent");

struct Setup {
    agent: Agent,
    agent_dir: TempDir,
    controller: Controller,
    machine: PairedMachine,
    _controller_dir: TempDir,
}

async fn start_agent(dir: &TempDir, max_parallel: usize) -> Agent {
    Agent::start(
        AgentConfig {
            state: StateDir::new(dir.path()),
            net: NetConfig::local_only(),
            name: Some("build-box".into()),
            jobs: Some(JobsConfig {
                runner: RunnerCommand::agent_binary(AGENT_BIN),
                // Tests stand in for the administrator runner with the same one.
                admin_runner: Some(RunnerCommand::agent_binary(AGENT_BIN)),
                max_parallel,
            }),
            shell: None,
        },
        approver_fn(|_: PairingCandidate| async { true }),
    )
    .await
    .unwrap()
}

async fn setup_with(max_parallel: usize) -> Setup {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = start_agent(&agent_dir, max_parallel).await;
    let controller_dir = tempfile::tempdir().unwrap();
    let controller = Controller::open(ControllerConfig {
        state: StateDir::new(controller_dir.path()),
        net: NetConfig::local_only(),
        name: Some("mac".into()),
    })
    .await
    .unwrap();
    let mut ticket = agent.open_pairing(Duration::from_secs(60)).await;
    ticket.machine = agent.loopback_addr();
    let machine = controller.pair(&ticket).await.unwrap();
    Setup {
        agent,
        agent_dir,
        controller,
        machine,
        _controller_dir: controller_dir,
    }
}

async fn setup() -> Setup {
    setup_with(2).await
}

/// Follows a job's log to the end, returning the output and the final job.
async fn follow(s: &Setup, id: &JobId) -> (String, JobInfo) {
    let mut out = Vec::new();
    let job = tokio::time::timeout(
        Duration::from_secs(60),
        s.controller.logs(&s.machine, id, 0, true, |_, data| {
            out.extend_from_slice(data)
        }),
    )
    .await
    .expect("job finished in time")
    .unwrap();
    (String::from_utf8_lossy(&out).into_owned(), job)
}

async fn wait_for_state(s: &Setup, id: &JobId, state: JobState) -> JobInfo {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let job = s.controller.job(&s.machine, id).await.unwrap();
        if job.status.state == state {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "job stayed {} instead of becoming {state}",
            job.status.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn remote_code(err: Error) -> ErrorCode {
    match err {
        Error::Remote(remote) => remote.code,
        other => panic!("expected a refusal from the machine, got {other}"),
    }
}

#[tokio::test]
async fn run_a_job_and_follow_its_output() {
    let s = setup().await;
    let job = s
        .controller
        .run(&s.machine, JobSpec::shell("echo one; sleep 1; echo two"))
        .await
        .unwrap();
    assert_eq!(job.meta.controller_name, "mac");

    let (out, done) = follow(&s, &job.meta.id).await;
    assert!(out.contains("one") && out.contains("two"), "{out}");
    assert_eq!(done.status.state, JobState::Succeeded);
    assert_eq!(done.status.exit_code, Some(0));
    assert_eq!(done.log_bytes as usize, out.len());
}

#[tokio::test]
async fn failing_job_reports_its_exit_code() {
    let s = setup().await;
    let job = s
        .controller
        .run(&s.machine, JobSpec::shell("echo broken; exit 7"))
        .await
        .unwrap();
    let (out, done) = follow(&s, &job.meta.id).await;
    assert!(out.contains("broken"));
    assert_eq!(done.status.state, JobState::Failed);
    assert_eq!(done.status.exit_code, Some(7));
}

#[tokio::test]
async fn logs_resume_from_any_offset() {
    let s = setup().await;
    let job = s
        .controller
        .run(&s.machine, JobSpec::shell("echo abcdefghij"))
        .await
        .unwrap();
    let (full, _) = follow(&s, &job.meta.id).await;

    let mut tail = Vec::new();
    let mut first_offset = None;
    s.controller
        .logs(&s.machine, &job.meta.id, 3, false, |offset, data| {
            first_offset.get_or_insert(offset);
            tail.extend_from_slice(data);
        })
        .await
        .unwrap();
    assert_eq!(first_offset, Some(3));
    assert_eq!(String::from_utf8_lossy(&tail), full[3..]);
}

#[tokio::test]
async fn cancel_stops_a_running_job() {
    let s = setup().await;
    let job = s
        .controller
        .run(&s.machine, JobSpec::shell("echo started; sleep 60"))
        .await
        .unwrap();
    let id = job.meta.id;
    wait_for_state(&s, &id, JobState::Running).await;

    let started = Instant::now();
    s.controller.cancel(&s.machine, &id).await.unwrap();
    let job = wait_for_state(&s, &id, JobState::Cancelled).await;
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "cancelled promptly"
    );
    assert!(job.status.finished_at_ms.is_some());
}

#[tokio::test]
async fn queued_job_can_be_cancelled_before_it_starts() {
    let s = setup_with(1).await;
    let first = s
        .controller
        .run(&s.machine, JobSpec::shell("sleep 60"))
        .await
        .unwrap();
    let second = s
        .controller
        .run(&s.machine, JobSpec::shell("echo should-not-run"))
        .await
        .unwrap();
    assert_eq!(second.status.state, JobState::Queued);

    let cancelled = s
        .controller
        .cancel(&s.machine, &second.meta.id)
        .await
        .unwrap();
    assert_eq!(cancelled.status.state, JobState::Cancelled);
    s.controller
        .cancel(&s.machine, &first.meta.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn jobs_wait_for_a_free_slot() {
    let s = setup_with(1).await;
    let first = s
        .controller
        .run(&s.machine, JobSpec::shell("sleep 1; echo first"))
        .await
        .unwrap();
    let second = s
        .controller
        .run(&s.machine, JobSpec::shell("echo second"))
        .await
        .unwrap();
    assert_eq!(second.status.state, JobState::Queued);

    let (_, a) = follow(&s, &first.meta.id).await;
    let (out, b) = follow(&s, &second.meta.id).await;
    assert_eq!(a.status.state, JobState::Succeeded);
    assert_eq!(b.status.state, JobState::Succeeded);
    assert!(out.contains("second"));
    assert!(
        b.status.started_at_ms >= a.status.finished_at_ms,
        "ran one after the other"
    );

    let listed = s.controller.jobs(&s.machine).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].meta.id, second.meta.id, "newest first");
}

#[tokio::test]
async fn a_job_keeps_running_while_the_agent_restarts() {
    let mut s = setup().await;
    let job = s
        .controller
        .run(
            &s.machine,
            JobSpec::shell("echo before; sleep 3; echo after"),
        )
        .await
        .unwrap();
    let id = job.meta.id;
    wait_for_state(&s, &id, JobState::Running).await;

    // Stop the agent completely, then start it again on the same folder.
    s.agent.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.agent = start_agent(&s.agent_dir, 2).await;
    s.machine.addr = s.agent.loopback_addr();

    let (out, done) = follow(&s, &id).await;
    assert_eq!(done.status.state, JobState::Succeeded, "{out}");
    assert!(out.contains("before") && out.contains("after"), "{out}");
}

#[tokio::test]
async fn paused_machine_refuses_new_jobs() {
    let s = setup().await;
    s.agent.set_paused(true).unwrap();
    assert!(s.controller.info(&s.machine).await.unwrap().paused);

    let err = s
        .controller
        .run(&s.machine, JobSpec::shell("echo hi"))
        .await
        .unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::Paused);

    s.agent.set_paused(false).unwrap();
    s.controller
        .run(&s.machine, JobSpec::shell("echo hi"))
        .await
        .unwrap();
}

#[tokio::test]
async fn device_without_run_jobs_permission_is_refused() {
    let s = setup().await;
    let state_dir = StateDir::new(s.agent_dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0].permissions.remove(Permission::RunJobs);
    state.save(&state_dir.agent_file()).unwrap();

    let err = s
        .controller
        .run(&s.machine, JobSpec::shell("echo hi"))
        .await
        .unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::PermissionDenied);
    // Ping needs no extra permission.
    s.controller.ping(&s.machine).await.unwrap();
}

#[tokio::test]
async fn unknown_job_is_not_found() {
    let s = setup().await;
    let err = s
        .controller
        .job(&s.machine, &JobId::generate())
        .await
        .unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::NotFound);
}

#[tokio::test]
async fn audit_log_records_who_ran_what() {
    let s = setup().await;
    let mut spec = JobSpec::shell("echo audited");
    spec.env
        .push(("SECRET_TOKEN".into(), "do-not-log-me".into()));
    let job = s.controller.run(&s.machine, spec).await.unwrap();
    s.agent.set_paused(true).unwrap();
    let _ = s
        .controller
        .run(&s.machine, JobSpec::shell("echo refused"))
        .await;

    let entries = s.agent.audit_log().recent(10).unwrap();
    let ran = entries
        .iter()
        .find(|e| e.command == "run_job" && e.outcome == "ok")
        .expect("accepted run recorded");
    assert_eq!(ran.controller, s.controller.id());
    assert_eq!(ran.controller_name.as_deref(), Some("mac"));
    let detail = ran.detail.as_deref().unwrap();
    assert!(detail.contains(job.meta.id.as_str()) && detail.contains("echo audited"));
    // The raw request would carry the env value, so only the signature is kept.
    assert!(ran.request.is_none() && ran.signature.is_some());

    let refused = entries
        .iter()
        .find(|e| e.command == "run_job" && e.outcome == "Paused")
        .expect("refused run recorded");
    assert!(refused.detail.as_deref().unwrap().contains("echo refused"));

    let raw = std::fs::read_to_string(s.agent.audit_log().path()).unwrap();
    assert!(raw.contains("SECRET_TOKEN"));
    assert!(
        !raw.contains("do-not-log-me"),
        "env values never reach the audit log"
    );
}

#[tokio::test]
async fn admin_jobs_need_the_admin_permission() {
    let s = setup().await;
    let mut spec = JobSpec::shell("echo as-admin");
    spec.admin = true;

    // New devices don't get it.
    let err = s
        .controller
        .run(&s.machine, spec.clone())
        .await
        .unwrap_err();
    let message = err.to_string();
    assert_eq!(remote_code(err), ErrorCode::PermissionDenied);
    assert!(message.contains("yonder-agent allow"), "{message}");

    // The owner grants it on the machine.
    let state_dir = StateDir::new(s.agent_dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0].permissions.insert(Permission::Admin);
    state.save(&state_dir.agent_file()).unwrap();

    let job = s.controller.run(&s.machine, spec).await.unwrap();
    let (out, job) = follow(&s, &job.meta.id).await;
    assert_eq!(job.status.state, JobState::Succeeded, "{out}");
    assert!(out.contains("as-admin"));
    assert!(job.meta.spec.summary().contains("[as administrator]"));
}
