//! End-to-end terminal tests: real shells in real PTYs on an agent, driven by a controller
//! over loopback QUIC. Unix shells only; the Windows (ConPTY) path runs in Windows CI.
#![cfg(unix)]

use std::time::Duration;

use tempfile::TempDir;
use yonder_core::Error;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig, TerminalEvent, TerminalReader};
use yonder_core::net::NetConfig;
use yonder_core::permissions::Permission;
use yonder_core::protocol::ErrorCode;
use yonder_core::store::{AgentState, PairedMachine, StateDir};

struct Setup {
    agent_dir: TempDir,
    controller: Controller,
    machine: PairedMachine,
    _agent: Agent,
    _controller_dir: TempDir,
}

async fn setup() -> Setup {
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
        agent_dir,
        controller,
        machine,
        _agent: agent,
        _controller_dir: controller_dir,
    }
}

/// Reads output until `text` appears; returns everything read and the next offset.
async fn read_until(reader: &mut TerminalReader, text: &str) -> (String, u64) {
    let mut out = String::new();
    let mut end = 0;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !out.contains(text) {
            match reader.next().await.unwrap() {
                TerminalEvent::Output { offset, data } => {
                    out.push_str(&String::from_utf8_lossy(&data));
                    end = offset + data.len() as u64;
                }
                TerminalEvent::Exit(code) => {
                    panic!("shell exited ({code:?}) before {text:?}: {out}")
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never saw {text:?} in {out:?}"));
    (out, end)
}

#[tokio::test]
async fn shell_survives_detach_and_replays_from_an_offset() {
    let s = setup().await;
    let (info, mut writer, mut reader) = s
        .controller
        .open_terminal(&s.machine, None, 100, 30, None)
        .await
        .unwrap();
    assert_eq!((info.cols, info.rows), (100, 30));
    assert_eq!(info.started_by, "mac");

    writer.send(b"echo first-$((20+1))\r").await.unwrap();
    let (_, seen_up_to) = read_until(&mut reader, "first-21").await;

    // Detach: the shell keeps running on the machine.
    writer.detach();
    drop(reader);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let listed = s.controller.terminals(&s.machine).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, info.id);
    assert!(!listed[0].exited);

    // Reattach from where we got to, and keep working in the same shell.
    let (again, mut writer, mut reader) = s
        .controller
        .open_terminal(&s.machine, Some(info.id.clone()), 100, 30, Some(seen_up_to))
        .await
        .unwrap();
    assert_eq!(again.id, info.id);
    writer.send(b"echo second-$((40+2))\r").await.unwrap();
    let (out, _) = read_until(&mut reader, "second-42").await;
    assert!(
        !out.contains("first-21"),
        "already-seen output isn't replayed: {out}"
    );

    // Attaching from 0 replays the whole session.
    let (_, _w, mut replay) = s
        .controller
        .open_terminal(&s.machine, Some(info.id.clone()), 100, 30, Some(0))
        .await
        .unwrap();
    let (out, _) = read_until(&mut replay, "second-42").await;
    assert!(out.contains("first-21"));
}

#[tokio::test]
async fn exit_code_is_reported() {
    let s = setup().await;
    let (_, mut writer, mut reader) = s
        .controller
        .open_terminal(&s.machine, None, 80, 24, None)
        .await
        .unwrap();
    writer.send(b"exit 5\r").await.unwrap();
    let code = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let TerminalEvent::Exit(code) = reader.next().await.unwrap() {
                return code;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, Some(5));
}

#[tokio::test]
async fn closed_session_is_gone() {
    let s = setup().await;
    let (info, _w, _r) = s
        .controller
        .open_terminal(&s.machine, None, 80, 24, None)
        .await
        .unwrap();
    let closed = s
        .controller
        .close_terminal(&s.machine, &info.id)
        .await
        .unwrap();
    assert_eq!(closed.id, info.id);
    assert!(s.controller.terminals(&s.machine).await.unwrap().is_empty());

    let err = s
        .controller
        .open_terminal(&s.machine, Some(info.id), 80, 24, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::NotFound));
}

#[tokio::test]
async fn terminal_needs_its_permission() {
    let s = setup().await;
    let state_dir = StateDir::new(s.agent_dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0]
        .permissions
        .remove(Permission::Terminal);
    state.save(&state_dir.agent_file()).unwrap();

    let err = s
        .controller
        .open_terminal(&s.machine, None, 80, 24, None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Remote(e) if e.code == ErrorCode::PermissionDenied));
}

#[tokio::test]
async fn a_port_on_the_machine_is_reachable_through_a_forward() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let s = setup().await;
    // A tiny server on the machine's localhost that shouts back what it gets.
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = server.accept().await.unwrap();
        let mut buf = vec![0u8; 64];
        let n = socket.read(&mut buf).await.unwrap();
        socket
            .write_all(&buf[..n].to_ascii_uppercase())
            .await
            .unwrap();
    });

    let (mut send, mut recv) = s
        .controller
        .forward(&s.machine, "127.0.0.1", port)
        .await
        .unwrap();
    send.write_all(b"hello through yonder").await.unwrap();
    send.flush().await.unwrap();
    let mut answer = vec![0u8; 20];
    tokio::time::timeout(Duration::from_secs(10), recv.read_exact(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, b"HELLO THROUGH YONDER");

    // Nothing listening: a clear refusal, not a hang.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let free = closed.local_addr().unwrap().port();
    drop(closed);
    let err = s
        .controller
        .forward(&s.machine, "127.0.0.1", free)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, Error::Remote(ref e) if e.code == ErrorCode::NotFound),
        "{err}"
    );

    // It needs the terminal permission.
    let state_dir = StateDir::new(s.agent_dir.path());
    let mut state = AgentState::load(&state_dir.agent_file()).unwrap();
    state.controllers[0]
        .permissions
        .remove(Permission::Terminal);
    state.save(&state_dir.agent_file()).unwrap();
    let err = s
        .controller
        .forward(&s.machine, "127.0.0.1", port)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Remote(ref e) if e.code == ErrorCode::PermissionDenied));
}
