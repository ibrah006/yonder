//! End-to-end footprint tests: what a device leaves on a machine, removing it, and
//! revoking a lost device. Unix shells; the Windows path shares the same code.
#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::footprint::FootprintItem;
use yonder_core::jobs::JobSpec;
use yonder_core::jobs::manager::{JobsConfig, RunnerCommand};
use yonder_core::net::NetConfig;
use yonder_core::protocol::ErrorCode;
use yonder_core::secrets::SecretName;
use yonder_core::store::{PairedMachine, StateDir};
use yonder_core::workspace::{LocalProject, WorkspaceId};

const AGENT_BIN: &str = env!("CARGO_BIN_EXE_yonder-agent");

struct Device {
    controller: Controller,
    machine: PairedMachine,
    _dir: TempDir,
}

async fn start_agent(dir: &TempDir) -> Agent {
    Agent::start(
        AgentConfig {
            state: StateDir::new(dir.path()),
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
    .unwrap()
}

async fn pair(agent: &Agent, name: &str) -> Device {
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
    Device {
        controller,
        machine,
        _dir: dir,
    }
}

fn name(text: &str) -> SecretName {
    SecretName::parse(text).unwrap()
}

/// A git project, synced to the machine; returns its workspace id.
async fn sync_project(d: &Device) -> (WorkspaceId, TempDir) {
    let project_dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(project_dir.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(project_dir.path().join("main.txt"), "code").unwrap();
    let project = LocalProject::scan(project_dir.path()).unwrap();
    let id = WorkspaceId::for_project(&project.root, &d.controller.id());
    d.controller
        .sync(&d.machine, &id, &project, |_, _| {})
        .await
        .unwrap();
    (id, project_dir)
}

async fn run_job(d: &Device) {
    let job = d
        .controller
        .run(&d.machine, JobSpec::shell("echo footprint"))
        .await
        .unwrap();
    d.controller
        .logs(&d.machine, &job.meta.id, 0, true, |_, _| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn a_device_sees_and_removes_only_its_own_footprint() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = start_agent(&agent_dir).await;
    let mac = pair(&agent, "mac").await;
    let laptop = pair(&agent, "laptop").await;

    mac.controller
        .set_secret(&mac.machine, name("TOKEN"), b"t".to_vec(), None, None)
        .await
        .unwrap();
    let (ws, _project) = sync_project(&mac).await;
    run_job(&mac).await;
    let (_shell, _w, _r) = mac
        .controller
        .open_terminal(&mac.machine, None, 80, 24, None)
        .await
        .unwrap();

    let fp = mac.controller.footprint(&mac.machine).await.unwrap();
    assert_eq!(fp.secrets.len(), 1);
    assert_eq!(fp.workspaces.len(), 1);
    assert_eq!(fp.workspaces[0].id, ws);
    assert_eq!(fp.jobs, 1);
    assert_eq!(fp.shells.len(), 1);
    assert!(fp.bytes() > 0);

    // The other device sees none of it and can't touch the project copy.
    let theirs = laptop.controller.footprint(&laptop.machine).await.unwrap();
    assert!(theirs.is_empty(), "{theirs:?}");
    let err = laptop
        .controller
        .remove_footprint(&laptop.machine, FootprintItem::Workspace(ws.clone()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::NotFound));
    let err = laptop
        .controller
        .pull(
            &laptop.machine,
            &ws,
            &["**".into()],
            tempfile::tempdir().unwrap().path(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::NotFound),
        "{err}"
    );

    let removed = mac
        .controller
        .remove_footprint(&mac.machine, FootprintItem::Everything)
        .await
        .unwrap();
    assert_eq!(
        (
            removed.secrets,
            removed.workspaces,
            removed.jobs,
            removed.shells
        ),
        (1, 1, 1, 1),
        "{removed:?}"
    );
    assert!(
        mac.controller
            .footprint(&mac.machine)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !agent_dir
            .path()
            .join("workspaces")
            .join(ws.as_str())
            .exists()
    );
}

#[tokio::test]
async fn a_lost_device_is_revoked_and_wiped_from_another() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = start_agent(&agent_dir).await;
    let mac = pair(&agent, "mac").await;
    let lost = pair(&agent, "lost-laptop").await;
    lost.controller
        .set_secret(&lost.machine, name("KEY"), b"k".to_vec(), None, None)
        .await
        .unwrap();
    let (ws, _project) = sync_project(&lost).await;

    let devices = mac.controller.devices(&mac.machine).await.unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().any(|d| d.you && d.name == "mac"));
    let lost_id = devices.iter().find(|d| !d.you).unwrap().id;

    // Not this way round: a device leaves with unpair.
    let err = mac
        .controller
        .revoke_device(&mac.machine, mac.controller.id(), true)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::BadRequest));

    let removed = mac
        .controller
        .revoke_device(&mac.machine, lost_id, true)
        .await
        .unwrap();
    assert_eq!((removed.secrets, removed.workspaces), (1, 1), "{removed:?}");
    assert!(
        !agent_dir
            .path()
            .join("workspaces")
            .join(ws.as_str())
            .exists()
    );
    assert!(
        !agent_dir
            .path()
            .join("secrets")
            .join(lost_id.to_string())
            .exists()
    );
    assert!(lost.controller.secrets(&lost.machine).await.is_err());
    assert_eq!(mac.controller.devices(&mac.machine).await.unwrap().len(), 1);
}

#[tokio::test]
async fn unpair_with_wipe_leaves_nothing() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = start_agent(&agent_dir).await;
    let mac = pair(&agent, "mac").await;
    mac.controller
        .set_secret(&mac.machine, name("TOKEN"), b"t".to_vec(), None, None)
        .await
        .unwrap();
    let removed = mac.controller.unpair(&mac.machine, true).await.unwrap();
    assert_eq!(removed.secrets, 1);
    assert!(mac.controller.machines().is_empty());
    let err = mac.controller.secrets(&mac.machine).await.unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::NotPaired),
        "{err}"
    );
}

#[tokio::test]
async fn auto_cleanup_is_off_until_a_device_turns_it_on() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = start_agent(&agent_dir).await;
    let mac = pair(&agent, "mac").await;
    let laptop = pair(&agent, "laptop").await;
    let (mine, _p1) = sync_project(&mac).await;
    let (theirs, _p2) = sync_project(&laptop).await;
    let in_three_days = yonder_core::protocol::now_ms() + 3 * 86_400_000;

    // Off by default: nothing goes, however old.
    assert_eq!(agent.clean_up_unused(in_three_days), 0);

    mac.controller
        .set_cleanup(&mac.machine, Some(2))
        .await
        .unwrap();
    let fp = mac.controller.footprint(&mac.machine).await.unwrap();
    assert_eq!(fp.cleanup_after_days, Some(2));
    // Recently used: kept.
    assert_eq!(agent.clean_up_unused(yonder_core::protocol::now_ms()), 0);
    // Unused for three days: only the mac's copy goes; the laptop kept cleanup off.
    assert_eq!(agent.clean_up_unused(in_three_days), 1);
    let workspaces = agent_dir.path().join("workspaces");
    assert!(!workspaces.join(mine.as_str()).exists());
    assert!(workspaces.join(theirs.as_str()).exists());

    mac.controller
        .set_cleanup(&mac.machine, None)
        .await
        .unwrap();
    assert_eq!(
        mac.controller
            .footprint(&mac.machine)
            .await
            .unwrap()
            .cleanup_after_days,
        None
    );
    let err = mac
        .controller
        .set_cleanup(&mac.machine, Some(0))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::BadRequest),
        "{err}"
    );
}
