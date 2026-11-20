//! `yonder upgrade`: rebuilds a Windows machine's agent from Yonder's source on this
//! computer and reinstalls it, so updates need nobody at the machine.
//!
//! The source is synced like any project, built there as an admin job (with the Rust
//! toolchain installed on the machine), and a scheduled task reinstalls the service once
//! the job has reported back. This waits for the agent to come back and shows how the
//! reinstall went.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use yonder_core::Os;
use yonder_core::controller::Controller;
use yonder_core::jobs::JobSpec;
use yonder_core::recipes;
use yonder_core::store::PairedMachine;

const SCRIPT: &str = include_str!("upgrade.ps1");
/// Shows the reinstall's output once the agent is back.
const SHOW_LOG: &str = "$log = Join-Path $env:ProgramData 'Yonder\\upgrade\\install.log'\n\
    if (Test-Path $log) { Get-Content $log } else { Write-Host '(no install log)' }\n\
    Unregister-ScheduledTask -TaskName 'Yonder upgrade' -Confirm:$false -ErrorAction SilentlyContinue\n";

/// Yonder's source: `--source`, `YONDER_SOURCE`, the checkout this CLI was built from,
/// or the repository you're in.
fn source(given: Option<PathBuf>) -> Result<PathBuf> {
    let looks_right = |dir: &Path| dir.join("crates/agent/Cargo.toml").is_file();
    let candidates = given
        .into_iter()
        .chain(std::env::var_os("YONDER_SOURCE").map(PathBuf::from))
        .chain([Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")])
        .chain(
            std::env::current_dir()
                .ok()
                .and_then(|d| crate::workspace::git_root(&d).ok()),
        );
    for dir in candidates {
        if looks_right(&dir) {
            return dir
                .canonicalize()
                .context("couldn't read Yonder's source folder");
        }
    }
    bail!("couldn't find Yonder's source; pass --source <your yonder checkout>")
}

pub async fn run(
    controller: &Controller,
    machine: &str,
    given: Option<PathBuf>,
) -> Result<ExitCode> {
    let machine = controller.pick_machine(machine).await?;
    if machine.os != Os::Windows {
        bail!(
            "upgrade is for Windows machines for now; on {} pull and reinstall the agent there",
            machine.name
        );
    }
    let source = source(given)?;
    eprintln!(
        "[yonder] upgrading {} from {}",
        machine.name,
        source.display()
    );
    let synced = crate::workspace::sync_at(controller, &machine, &source).await?;

    let mut spec = JobSpec::shell("");
    spec.program = recipes::powershell(SCRIPT, &[]);
    spec.cwd = Some(synced.remote.clone());
    spec.label = Some("upgrade agent".into());
    spec.admin = true;
    let job = controller.run(&machine, spec).await?;
    eprintln!(
        "[yonder] building on {} as job {}",
        machine.name, job.meta.id
    );
    let code = crate::jobs::follow(controller, &machine, &job.meta.id, 0).await?;
    if code != ExitCode::SUCCESS {
        return Ok(code);
    }

    eprintln!(
        "[yonder] waiting for {} to restart its agent…",
        machine.name
    );
    wait_for_restart(controller, &machine).await?;
    let info = controller.info(&machine).await?;
    eprintln!(
        "[yonder] {} is back, agent {}",
        machine.name, info.agent_version
    );

    let mut spec = JobSpec::shell("");
    spec.program = recipes::powershell(SHOW_LOG, &[]);
    spec.label = Some("upgrade log".into());
    spec.admin = true;
    let job = controller.run(&machine, spec).await?;
    crate::jobs::follow(controller, &machine, &job.meta.id, 0).await
}

/// Waits for the agent to go away and answer again.
async fn wait_for_restart(controller: &Controller, machine: &PairedMachine) -> Result<()> {
    let started = Instant::now();
    let mut went_away = false;
    while started.elapsed() < Duration::from_secs(180) {
        let answered = tokio::time::timeout(Duration::from_secs(8), controller.ping(machine))
            .await
            .is_ok_and(|r| r.is_ok());
        match (answered, went_away) {
            (false, _) => went_away = true,
            (true, true) => return Ok(()),
            // Still the old agent; the reinstall starts a few seconds after the job.
            (true, false) if started.elapsed() > Duration::from_secs(60) => return Ok(()),
            (true, false) => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!(
        "{} didn't come back within 3 minutes; on the machine, see \
         C:\\ProgramData\\Yonder\\upgrade\\install.log",
        machine.name
    )
}
