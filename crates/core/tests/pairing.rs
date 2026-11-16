//! End-to-end tests: a real agent and real controllers talking over loopback QUIC, with no
//! relays and no internet.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use yonder_core::agent::{Agent, AgentConfig, AgentEvent, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::net::NetConfig;
use yonder_core::permissions::Permission;
use yonder_core::protocol::{ErrorCode, PairRejection};
use yonder_core::store::{AgentState, StateDir};
use yonder_core::ticket::PairingTicket;
use yonder_core::{Error, Os};

const TTL: Duration = Duration::from_secs(60);

struct TestAgent {
    agent: Agent,
    approvals: Arc<AtomicUsize>,
    _dir: TempDir,
}

async fn start_agent_in(dir: TempDir, approve: bool) -> TestAgent {
    let approvals = Arc::new(AtomicUsize::new(0));
    let counter = approvals.clone();
    let agent = Agent::start(
        AgentConfig {
            state: StateDir::new(dir.path()),
            net: NetConfig::local_only(),
            name: Some("build-box".into()),
            jobs: None,
            shell: None,
        },
        approver_fn(move |_candidate: PairingCandidate| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { approve }
        }),
    )
    .await
    .unwrap();
    TestAgent {
        agent,
        approvals,
        _dir: dir,
    }
}

async fn start_agent(approve: bool) -> TestAgent {
    start_agent_in(tempfile::tempdir().unwrap(), approve).await
}

async fn open_controller(dir: &TempDir, name: &str) -> Controller {
    Controller::open(ControllerConfig {
        state: StateDir::new(dir.path()),
        net: NetConfig::local_only(),
        name: Some(name.into()),
    })
    .await
    .unwrap()
}

async fn new_controller(name: &str) -> (Controller, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (open_controller(&dir, name).await, dir)
}

/// A ticket pointing at the agent's loopback address.
async fn ticket(agent: &Agent) -> PairingTicket {
    let mut ticket = agent.open_pairing(TTL).await;
    ticket.machine = agent.loopback_addr();
    ticket
}

fn rejection(err: Error) -> PairRejection {
    match err {
        Error::PairingRejected(reason) => reason,
        other => panic!("expected a pairing rejection, got {other}"),
    }
}

fn remote_code(err: Error) -> ErrorCode {
    match err {
        Error::Remote(remote) => remote.code,
        other => panic!("expected a refusal from the machine, got {other}"),
    }
}

#[tokio::test]
async fn pair_then_ping_and_info() {
    let t = start_agent(true).await;
    let mut events = t.agent.subscribe();
    let (controller, _dir) = new_controller("Ibrahim's Mac").await;

    let machine = controller.pair(&ticket(&t.agent).await).await.unwrap();
    assert_eq!(machine.name, "build-box");
    assert_eq!(machine.os, Os::current());
    assert!(!machine.permissions.contains(Permission::Admin));
    assert_eq!(t.approvals.load(Ordering::SeqCst), 1);

    match events.recv().await.unwrap() {
        AgentEvent::Paired(c) => {
            assert_eq!(c.id, controller.id());
            assert_eq!(c.name, "Ibrahim's Mac");
        }
        other => panic!("unexpected event {other:?}"),
    }
    assert_eq!(t.agent.controllers().unwrap().len(), 1);

    controller.ping(&machine).await.unwrap();
    let info = controller.info(&machine).await.unwrap();
    assert_eq!(info.name, "build-box");
    assert_eq!(info.your_permissions, machine.permissions);

    // The machine list was saved and can be found by name.
    assert_eq!(
        controller.find_machine("BUILD-BOX").unwrap().id,
        t.agent.id()
    );
}

#[tokio::test]
async fn controller_shows_matching_fingerprint_to_the_machine() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen_in_approver = seen.clone();
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::start(
        AgentConfig {
            state: StateDir::new(dir.path()),
            net: NetConfig::local_only(),
            name: None,
            jobs: None,
            shell: None,
        },
        approver_fn(move |candidate: PairingCandidate| {
            *seen_in_approver.lock().unwrap() = Some(candidate.fingerprint);
            async { true }
        }),
    )
    .await
    .unwrap();
    let (controller, _cdir) = new_controller("mac").await;
    controller.pair(&ticket(&agent).await).await.unwrap();
    assert_eq!(seen.lock().unwrap().clone(), Some(controller.fingerprint()));
}

#[tokio::test]
async fn wrong_ticket_is_rejected_without_asking() {
    let t = start_agent(true).await;
    let (controller, _dir) = new_controller("mac").await;
    let mut bad = ticket(&t.agent).await;
    bad.secret[0] ^= 1;

    let err = controller.pair(&bad).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::WrongTicket);
    assert_eq!(t.approvals.load(Ordering::SeqCst), 0);
    assert!(t.agent.controllers().unwrap().is_empty());
}

#[tokio::test]
async fn repeated_wrong_tickets_close_pairing() {
    let t = start_agent(true).await;
    let (controller, _dir) = new_controller("mac").await;
    let good = ticket(&t.agent).await;
    let mut bad = good.clone();
    bad.secret[0] ^= 1;

    for _ in 0..5 {
        controller.pair(&bad).await.unwrap_err();
    }
    let err = controller.pair(&good).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::NotOpen);
}

#[tokio::test]
async fn declined_pairing_saves_nothing() {
    let t = start_agent(false).await;
    let (controller, _dir) = new_controller("mac").await;

    let err = controller.pair(&ticket(&t.agent).await).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::Declined);
    assert!(t.agent.controllers().unwrap().is_empty());
    assert!(controller.machines().is_empty());
}

#[tokio::test]
async fn ticket_works_only_once() {
    let t = start_agent(true).await;
    let ticket = ticket(&t.agent).await;
    let (first, _d1) = new_controller("first").await;
    let (second, _d2) = new_controller("second").await;

    first.pair(&ticket).await.unwrap();
    let err = second.pair(&ticket).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::NotOpen);
    assert_eq!(t.agent.controllers().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_pairing_is_rejected() {
    let t = start_agent(true).await;
    let mut ticket = t.agent.open_pairing(Duration::ZERO).await;
    ticket.machine = t.agent.loopback_addr();
    let (controller, _dir) = new_controller("mac").await;

    let err = controller.pair(&ticket).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::NotOpen);
}

#[tokio::test]
async fn closed_pairing_is_rejected() {
    let t = start_agent(true).await;
    let ticket = ticket(&t.agent).await;
    t.agent.close_pairing();
    let (controller, _dir) = new_controller("mac").await;

    let err = controller.pair(&ticket).await.unwrap_err();
    assert_eq!(rejection(err), PairRejection::NotOpen);
}

#[tokio::test]
async fn unpaired_device_is_refused() {
    let t = start_agent(true).await;
    let (paired, _d1) = new_controller("paired").await;
    let machine = paired.pair(&ticket(&t.agent).await).await.unwrap();

    // A second device that copied the machine entry but never paired.
    let (stranger, _d2) = new_controller("stranger").await;
    let err = stranger.ping(&machine).await.unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::NotPaired);
}

#[tokio::test]
async fn revoked_device_is_refused_even_on_an_open_connection() {
    let t = start_agent(true).await;
    let (controller, _dir) = new_controller("mac").await;
    let machine = controller.pair(&ticket(&t.agent).await).await.unwrap();
    controller.ping(&machine).await.unwrap();

    assert!(t.agent.revoke(&controller.id()).unwrap());
    assert!(t.agent.controllers().unwrap().is_empty());

    let err = controller.ping(&machine).await.unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::NotPaired);
}

#[tokio::test]
async fn revoke_by_another_process_takes_effect_immediately() {
    let t = start_agent(true).await;
    let (controller, _dir) = new_controller("mac").await;
    let machine = controller.pair(&ticket(&t.agent).await).await.unwrap();
    controller.ping(&machine).await.unwrap();

    // Simulate the agent CLI editing the state file while the agent runs.
    let state_dir = StateDir::new(t._dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    assert!(state.remove_controller(&controller.id()));
    state.save(&state_dir.agent_file()).unwrap();

    let err = controller.ping(&machine).await.unwrap_err();
    assert_eq!(remote_code(err), ErrorCode::NotPaired);
}

#[tokio::test]
async fn pairing_survives_an_agent_restart() {
    let dir = tempfile::tempdir().unwrap();
    let agent_path = dir.path().to_owned();
    let t = start_agent_in(dir, true).await;
    let (controller, _cdir) = new_controller("mac").await;
    controller.pair(&ticket(&t.agent).await).await.unwrap();
    let id = t.agent.id();
    t.agent.shutdown().await;
    let TestAgent { _dir: dir, .. } = t;
    assert_eq!(dir.path(), agent_path);

    let restarted = start_agent_in(dir, true).await;
    assert_eq!(restarted.agent.id(), id, "machine keeps its identity");
    assert_eq!(restarted.agent.controllers().unwrap().len(), 1);

    // The new run listens on a new port; point the saved entry at it.
    let mut machine = controller.find_machine("build-box").unwrap();
    machine.addr = restarted.agent.loopback_addr();
    controller.ping(&machine).await.unwrap();
}

#[tokio::test]
async fn controller_remembers_machines_across_restarts() {
    let t = start_agent(true).await;
    let dir = tempfile::tempdir().unwrap();
    let controller = open_controller(&dir, "mac").await;
    let id = controller.id();
    controller.pair(&ticket(&t.agent).await).await.unwrap();
    controller.close().await;

    let reopened = open_controller(&dir, "mac").await;
    assert_eq!(reopened.id(), id);
    assert_eq!(reopened.machines().len(), 1);
    assert!(reopened.forget(&t.agent.id()).unwrap());
    assert!(reopened.machines().is_empty());
}
