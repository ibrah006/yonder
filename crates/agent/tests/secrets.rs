//! End-to-end secret tests: saved on a real agent, handed to real job runners.
//! Unix shell scripts; the Windows path shares the same code.
#![cfg(unix)]

use std::time::Duration;

use tempfile::TempDir;
use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::jobs::manager::{JobsConfig, RunnerCommand};
use yonder_core::jobs::{JobId, JobInfo, JobSpec, JobState};
use yonder_core::net::NetConfig;
use yonder_core::protocol::{ErrorCode, now_ms};
use yonder_core::secrets::{SecretName, SecretTarget, SecretUse};
use yonder_core::store::{PairedMachine, StateDir};

const AGENT_BIN: &str = env!("CARGO_BIN_EXE_yonder-agent");

struct Setup {
    agent: Agent,
    agent_dir: TempDir,
    controller: Controller,
    machine: PairedMachine,
    _controller_dir: TempDir,
}

async fn pair(agent: &Agent, name: &str) -> (Controller, PairedMachine, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let controller = Controller::open(ControllerConfig {
        state: StateDir::new(dir.path()),
        net: NetConfig::local_only(),
        name: Some(name.into()),
    })
    .await
    .unwrap();
    let mut ticket = agent.open_pairing(Duration::from_secs(60)).await;
    ticket.machine = agent.loopback_addr();
    let machine = controller.pair(&ticket).await.unwrap();
    (controller, machine, dir)
}

async fn setup() -> Setup {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = Agent::start(
        AgentConfig {
            state: StateDir::new(agent_dir.path()),
            net: NetConfig::local_only(),
            name: Some("build-box".into()),
            jobs: Some(JobsConfig {
                runner: RunnerCommand::agent_binary(AGENT_BIN),
                admin_runner: None,
                max_parallel: 2,
            }),
            shell: None,
        },
        approver_fn(|_: PairingCandidate| async { true }),
    )
    .await
    .unwrap();
    let (controller, machine, controller_dir) = pair(&agent, "mac").await;
    Setup {
        agent,
        agent_dir,
        controller,
        machine,
        _controller_dir: controller_dir,
    }
}

fn name(text: &str) -> SecretName {
    SecretName::parse(text).unwrap()
}

fn uses(name_: &str, target: SecretTarget) -> SecretUse {
    SecretUse {
        name: name(name_),
        target,
    }
}

async fn run(s: &Setup, spec: JobSpec) -> (String, JobInfo) {
    let job = s.controller.run(&s.machine, spec).await.unwrap();
    follow(&s.controller, &s.machine, &job.meta.id).await
}

async fn follow(c: &Controller, m: &PairedMachine, id: &JobId) -> (String, JobInfo) {
    let mut out = Vec::new();
    let job = tokio::time::timeout(
        Duration::from_secs(60),
        c.logs(m, id, 0, true, |_, data| out.extend_from_slice(data)),
    )
    .await
    .expect("job finished in time")
    .unwrap();
    (String::from_utf8_lossy(&out).into_owned(), job)
}

#[tokio::test]
async fn env_and_file_secrets_reach_the_job_and_files_are_removed() {
    let s = setup().await;
    s.controller
        .set_secret(
            &s.machine,
            name("TOKEN"),
            b"tok-123".to_vec(),
            None,
            Some("CI".into()),
        )
        .await
        .unwrap();
    s.controller
        .set_secret(&s.machine, name("key.pem"), b"PRIVATE".to_vec(), None, None)
        .await
        .unwrap();

    let listed = s.controller.secrets(&s.machine).await.unwrap();
    assert_eq!(listed.len(), 2);

    let cwd = tempfile::tempdir().unwrap();
    let mut spec = JobSpec::shell("echo \"env=$MY_TOKEN\"; echo \"file=$(cat keys/k.pem)\"");
    spec.cwd = Some(cwd.path().to_string_lossy().into_owned());
    spec.secrets = vec![
        uses("TOKEN", SecretTarget::Env("MY_TOKEN".into())),
        uses("key.pem", SecretTarget::File("keys/k.pem".into())),
    ];
    let (out, job) = run(&s, spec).await;
    assert_eq!(job.status.state, JobState::Succeeded, "{out}");
    assert!(out.contains("env=tok-123"), "{out}");
    assert!(out.contains("file=PRIVATE"), "{out}");

    // The file is gone once the job ends, and nothing in the job folder holds a value.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!cwd.path().join("keys/k.pem").exists());
    let job_dir = s.agent_dir.path().join("jobs").join(job.meta.id.as_str());
    for entry in std::fs::read_dir(&job_dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "output.log" {
            continue;
        }
        let text = String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap()).into_owned();
        assert!(
            !text.contains("tok-123") && !text.contains("PRIVATE"),
            "{name}: {text}"
        );
    }
    // Job lists show names, never values.
    assert!(job.meta.spec.summary().contains("TOKEN"));

    let audit = std::fs::read_to_string(s.agent_dir.path().join("audit.log")).unwrap();
    assert!(audit.contains("set_secret"));
    assert!(
        !audit.contains("tok-123") && !audit.contains(&hex(b"tok-123")),
        "{audit}"
    );
    drop(s.agent);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn other_devices_cant_use_or_see_my_secrets() {
    let s = setup().await;
    s.controller
        .set_secret(&s.machine, name("TOKEN"), b"mine".to_vec(), None, None)
        .await
        .unwrap();
    let (laptop, machine, _dir) = pair(&s.agent, "laptop").await;
    assert!(laptop.secrets(&machine).await.unwrap().is_empty());

    let mut spec = JobSpec::shell("echo $TOKEN");
    spec.secrets = vec![uses("TOKEN", SecretTarget::Env("TOKEN".into()))];
    let err = laptop.run(&machine, spec).await.unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::NotFound));
    let err = laptop
        .remove_secret(&machine, &name("TOKEN"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::NotFound));
}

#[tokio::test]
async fn expired_and_removed_secrets_stop_jobs() {
    let s = setup().await;
    s.controller
        .set_secret(
            &s.machine,
            name("OLD"),
            b"x".to_vec(),
            Some(now_ms() - 1),
            None,
        )
        .await
        .unwrap();
    let mut spec = JobSpec::shell("echo hi");
    spec.secrets = vec![uses("OLD", SecretTarget::Env("OLD".into()))];
    let err = s
        .controller
        .run(&s.machine, spec.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Remote(e) if e.message.contains("expired")),
        "{err}"
    );

    s.controller
        .remove_secret(&s.machine, &name("OLD"))
        .await
        .unwrap();
    let err = s.controller.run(&s.machine, spec).await.unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::NotFound));
}

#[tokio::test]
async fn env_values_stay_out_of_job_lists() {
    let s = setup().await;
    let mut spec = JobSpec::shell("echo done");
    spec.env.push(("PLAIN".into(), "visible-before".into()));
    let (_, job) = run(&s, spec).await;
    assert_eq!(job.meta.spec.env, vec![("PLAIN".into(), String::new())]);
    let listed = s.controller.jobs(&s.machine).await.unwrap();
    assert!(!format!("{listed:?}").contains("visible-before"));
}

#[tokio::test]
async fn machines_without_an_admin_runner_refuse_admin_jobs() {
    let s = setup().await;
    let state_dir = StateDir::new(s.agent_dir.path());
    let mut state = yonder_core::store::AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0]
        .permissions
        .insert(yonder_core::permissions::Permission::Admin);
    state.save(&state_dir.agent_file()).unwrap();
    let mut spec = JobSpec::shell("echo hi");
    spec.admin = true;
    let err = s.controller.run(&s.machine, spec).await.unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::Unsupported),
        "{err}"
    );
}
