//! The app hosts this device's connections; another process with the same key (the CLI)
//! works through it without going online itself.
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig, TerminalEvent};
use yonder_core::net::NetConfig;
use yonder_core::protocol::ErrorCode;
use yonder_core::store::StateDir;

async fn open(dir: &std::path::Path, name: &str) -> Controller {
    Controller::open(ControllerConfig {
        state: StateDir::new(dir),
        net: NetConfig::local_only(),
        name: Some(name.into()),
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn the_cli_goes_through_the_app_while_it_hosts() {
    let agent_dir = tempfile::tempdir().unwrap();
    let agent = Agent::start(
        AgentConfig {
            state: StateDir::new(agent_dir.path()),
            net: NetConfig::local_only(),
            name: Some("box".into()),
            jobs: None,
            shell: None,
            screenshot: None,
        },
        approver_fn(|_: PairingCandidate| async { true }),
    )
    .await
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(open(dir.path(), "mac").await);
    let mut ticket = agent.open_pairing(Duration::from_secs(60)).await;
    ticket.machine = agent.loopback_addr();
    app.pair(&ticket).await.unwrap();
    let task = app.host().unwrap();
    // A second host is refused.
    assert!(open(dir.path(), "mac").await.pipe_host().is_err());

    // The CLI: same folder, same key.
    let cli = open(dir.path(), "mac").await;
    let machine = cli.machines()[0].clone();
    let info = cli.info(&machine).await.unwrap();
    assert_eq!(info.name, "box");
    assert!(!cli.is_online(), "the CLI used the app's connection");

    // Streams work too: a shell, through the app.
    let (_, mut writer, mut reader) = cli
        .open_terminal(&machine, None, 80, 24, None)
        .await
        .unwrap();
    writer.send(b"echo through-$((1+1))\r").await.unwrap();
    let mut out = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !out.contains("through-2") {
            if let TerminalEvent::Output { data, .. } = reader.next().await.unwrap() {
                out.push_str(&String::from_utf8_lossy(&data));
            }
        }
    })
    .await
    .expect("shell output arrived");
    assert!(!cli.is_online());

    // Errors from the machine come back as they are.
    agent.revoke(&cli.id()).unwrap();
    let err = cli.info(&machine).await.unwrap_err();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::NotPaired),
        "{err}"
    );

    // Once the app stops hosting, the CLI connects on its own.
    app.stop_hosting(task);
    let _ = cli.info(&machine).await;
    assert!(cli.is_online());
}

trait PipeHost {
    fn pipe_host(self) -> yonder_core::Result<tokio::task::JoinHandle<()>>;
}

impl PipeHost for Controller {
    fn pipe_host(self) -> yonder_core::Result<tokio::task::JoinHandle<()>> {
        Arc::new(self).host()
    }
}
