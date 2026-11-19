//! Screenshots: the agent runs its capture command and sends the picture back in pieces.
//! A stand-in command writes a large file, so the pieces are tested too.
#![cfg(unix)]

use std::time::Duration;

use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::net::NetConfig;
use yonder_core::permissions::Permission;
use yonder_core::protocol::ErrorCode;
use yonder_core::store::{AgentState, StateDir};
use yonder_core::terminal::ShellCommand;

#[tokio::test]
async fn a_screenshot_comes_back_whole_for_admins_only() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = Agent::start(
        AgentConfig {
            state: StateDir::new(agent_dir.path()),
            net: NetConfig::local_only(),
            name: Some("box".into()),
            jobs: None,
            shell: None,
            // The agent appends the file to write: $0 for `sh -c`.
            screenshot: Some(ShellCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "head -c 1200000 /dev/zero > \"$0\"".into()],
            }),
        },
        approver_fn(|_: PairingCandidate| async { true }),
    )
    .await
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let controller = Controller::open(ControllerConfig {
        state: StateDir::new(dir.path()),
        net: NetConfig::local_only(),
        name: Some("mac".into()),
    })
    .await
    .unwrap();
    let mut ticket = agent.open_pairing(Duration::from_secs(60)).await;
    ticket.machine = agent.loopback_addr();
    let machine = controller.pair(&ticket).await.unwrap();

    let err = controller.screenshot(&machine).await.unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::PermissionDenied),
        "{err}"
    );

    let state_dir = StateDir::new(agent_dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0].permissions.insert(Permission::Admin);
    state.save(&state_dir.agent_file()).unwrap();

    let png = controller.screenshot(&machine).await.unwrap();
    assert_eq!(png.len(), 1_200_000);
    assert!(png.iter().all(|b| *b == 0));
}
