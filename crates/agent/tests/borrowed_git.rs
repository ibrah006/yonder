//! End-to-end borrowed git logins: real git in a real job asks the device that started
//! it, through the agent, and gets an answer. Unix shells; Windows uses the same helper.
#![cfg(unix)]

use std::time::Duration;

use tempfile::TempDir;
use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::credentials::GitCredential;
use yonder_core::jobs::manager::{JobsConfig, RunnerCommand};
use yonder_core::jobs::{JobSpec, JobState};
use yonder_core::net::NetConfig;
use yonder_core::protocol::ErrorCode;
use yonder_core::store::{PairedMachine, StateDir};

const AGENT_BIN: &str = env!("CARGO_BIN_EXE_yonder-agent");

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

#[tokio::test]
async fn git_in_a_job_borrows_a_login_from_its_device() {
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
            screenshot: None,
        },
        approver_fn(|_: PairingCandidate| async { true }),
    )
    .await
    .unwrap();
    let (mac, machine, _d1) = pair(&agent, "mac").await;
    let (laptop, laptop_machine, _d2) = pair(&agent, "laptop").await;

    // Git asks its helpers, prints what it got, then would save it (our helper won't).
    let script = "printf 'protocol=https\\nhost=example.com\\n\\n' | git credential fill; \
                  printf 'protocol=https\\nhost=example.com\\nusername=me\\npassword=approve-me\\n\\n' \
                  | git credential approve";
    let mut spec = JobSpec::shell(script);
    spec.borrow_git = true;
    let job = mac.run(&machine, spec).await.unwrap();
    let id = job.meta.id.clone();

    let (requests, mut asked) = tokio::sync::mpsc::unbounded_channel();
    let mut out = Vec::new();
    let follow = mac.logs_lending(
        &machine,
        &id,
        0,
        true,
        |_, data| out.extend_from_slice(data),
        |request| {
            let _ = requests.send(request);
        },
    );
    let answer = async {
        let request = asked.recv().await.unwrap();
        assert_eq!(request.host.as_deref(), Some("example.com"));
        // Another device can't answer for this job.
        let err = laptop
            .answer_git_credential(&laptop_machine, &id, request.id.clone(), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Remote(ref e) if e.code == ErrorCode::PermissionDenied),
            "{err}"
        );
        mac.answer_git_credential(
            &machine,
            &id,
            request.id,
            Some(GitCredential {
                username: "me".into(),
                password: "pat-1".into(),
            }),
        )
        .await
        .unwrap();
        std::future::pending::<()>().await
    };
    let finished = tokio::time::timeout(Duration::from_secs(60), async {
        tokio::select! {
            job = follow => job.unwrap(),
            _ = answer => unreachable!(),
        }
    })
    .await
    .expect("job finished in time");
    let out = String::from_utf8_lossy(&out);
    assert_eq!(finished.status.state, JobState::Succeeded, "{out}");
    assert!(
        out.contains("username=me") && out.contains("password=pat-1"),
        "{out}"
    );
    assert!(out.contains("borrowed a login"), "{out}");

    // Nothing of the login stays in the job's folder or the audit log.
    let job_dir = agent_dir.path().join("jobs").join(id.as_str());
    assert!(!job_dir.join("credentials").exists());
    let audit = std::fs::read_to_string(agent_dir.path().join("audit.log")).unwrap();
    assert!(audit.contains("answer_git_credential"), "{audit}");
    assert!(!audit.contains("pat-1"), "{audit}");
}
