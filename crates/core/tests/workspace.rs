//! End-to-end workspace sync: a git project on the controller, copied to an agent over
//! loopback QUIC, and build outputs copied back.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use yonder_core::agent::{Agent, AgentConfig, PairingCandidate, approver_fn};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::net::NetConfig;
use yonder_core::store::{PairedMachine, StateDir};
use yonder_core::workspace::{LocalProject, WorkspaceId};

struct Setup {
    controller: Controller,
    machine: PairedMachine,
    _agent: Agent,
    _dirs: (TempDir, TempDir),
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
        controller,
        machine,
        _agent: agent,
        _dirs: (agent_dir, controller_dir),
    }
}

fn write(root: &Path, path: &str, contents: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, contents).unwrap();
}

fn git_project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init");
    write(dir.path(), ".gitignore", "build/\n");
    write(dir.path(), "lib/main.dart", "void main() {}\n");
    write(dir.path(), "README.md", "hello\n");
    write(dir.path(), "build/cache.bin", "ignored");
    dir
}

#[tokio::test]
async fn project_syncs_incrementally_and_outputs_come_back() {
    let s = setup().await;
    let project_dir = git_project();
    let project = LocalProject::scan(&project_dir.path().join("lib")).unwrap();
    let paths: Vec<_> = project.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [".gitignore", "README.md", "lib/main.dart"]);

    let id = WorkspaceId::for_project(&project.root, &s.controller.id());
    let first = s
        .controller
        .sync(&s.machine, &id, &project, |_, _| {})
        .await
        .unwrap();
    assert_eq!(first.sent_files, 3);
    let remote = Path::new(&first.path);
    assert_eq!(
        std::fs::read_to_string(remote.join("lib/main.dart")).unwrap(),
        "void main() {}\n"
    );
    assert!(
        !remote.join("build/cache.bin").exists(),
        "ignored files stay home"
    );

    // Nothing changed: nothing is sent.
    let again = s
        .controller
        .sync(&s.machine, &id, &project, |_, _| {})
        .await
        .unwrap();
    assert_eq!(again.sent_files, 0);

    // A build on the machine; one edit and one deletion here.
    write(remote, "build/windows/app.exe", "binary");
    write(project_dir.path(), "README.md", "changed\n");
    std::fs::remove_file(project_dir.path().join("lib/main.dart")).unwrap();
    let project = LocalProject::scan(project_dir.path()).unwrap();
    let third = s
        .controller
        .sync(&s.machine, &id, &project, |_, _| {})
        .await
        .unwrap();
    assert_eq!(third.sent_files, 1);
    assert_eq!(
        std::fs::read_to_string(remote.join("README.md")).unwrap(),
        "changed\n"
    );
    assert!(!remote.join("lib/main.dart").exists());
    assert!(
        remote.join("build/windows/app.exe").exists(),
        "build output kept"
    );

    let pulled = s
        .controller
        .pull(&s.machine, &id, &["build/windows/**".into()], &project.root)
        .await
        .unwrap();
    assert_eq!(pulled.len(), 1);
    assert_eq!(
        std::fs::read_to_string(project_dir.path().join("build/windows/app.exe")).unwrap(),
        "binary"
    );
    // Already here: not copied again.
    let pulled = s
        .controller
        .pull(&s.machine, &id, &["build/windows/**".into()], &project.root)
        .await
        .unwrap();
    assert!(pulled.is_empty());
}
