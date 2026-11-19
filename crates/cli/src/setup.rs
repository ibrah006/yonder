//! `yonder setup`, `yonder doctor` and `yonder prep`: bring a machine to what the project
//! you're in needs.
//!
//! A recipe (see `yonder_core::recipes`) has two parts:
//! - `prep` runs the machine part with administrator rights: tools for every account
//!   and system settings. It needs the Admin permission, which the machine's owner
//!   grants with `yonder-agent allow <device> admin`.
//! - `setup` runs the project part as the build account in the machine's copy of the
//!   project: the project's own versions (Flutter through fvm, Shorebird). `doctor` runs
//!   the same checks without installing anything.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use yonder_core::controller::Controller;
use yonder_core::jobs::JobSpec;
use yonder_core::protocol::now_ms;
use yonder_core::recipes::{self, NOT_READY, Recipe};
use yonder_core::secrets::{SecretName, SecretTarget, SecretUse};
use yonder_core::store::PairedMachine;

/// What to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The project part: install what's missing.
    Setup,
    /// The project part, only reporting.
    Doctor,
    /// The machine part, as administrator.
    Prep,
}

/// Picks the recipe named, or the one that fits the project at `root`.
fn choose(name: Option<&str>, root: Option<&Path>) -> Result<&'static Recipe> {
    if let Some(name) = name {
        return match recipes::by_name(name) {
            Some(recipe) => Ok(recipe),
            None => bail!(
                "no recipe called {name}; there is:\n{}",
                recipes::describe_all()
            ),
        };
    }
    if let Some(recipe) = root.and_then(recipes::fitting) {
        return Ok(recipe);
    }
    bail!(
        "no recipe fits {}; name one with --recipe:\n{}",
        root.map(|r| r.display().to_string())
            .unwrap_or_else(|| "this folder".into()),
        recipes::describe_all()
    )
}

fn check_os(recipe: &Recipe, machine: &PairedMachine) -> Result<()> {
    if machine.os != recipe.os {
        bail!(
            "the {} recipe is for {} machines; {} is {}",
            recipe.name,
            recipe.os.short_name(),
            machine.name,
            machine.os.short_name()
        );
    }
    Ok(())
}

pub async fn run(
    controller: &Controller,
    machine: &str,
    recipe: Option<&str>,
    mode: Mode,
) -> Result<ExitCode> {
    let machine = controller.pick_machine(machine).await?;
    let here = std::env::current_dir()?;
    let root = crate::workspace::git_root(&here).ok();
    // The machine part doesn't need a project, but one tells which recipe fits.
    if mode != Mode::Prep && root.is_none() {
        bail!("run this inside the git project to set up");
    }
    let recipe = choose(recipe, root.as_deref())?;
    check_os(recipe, &machine)?;

    let spec = if mode == Mode::Prep {
        eprintln!(
            "[yonder] preparing {} for the {} recipe, with administrator rights…",
            machine.name, recipe.name
        );
        let mut spec = JobSpec::shell("");
        spec.program = recipes::powershell(recipe.machine_script, &[]);
        spec.label = Some(format!("prep {}", recipe.name));
        spec.admin = true;
        spec
    } else {
        let root = root.expect("checked above");
        eprintln!(
            "[yonder] {} {} for {} with the {} recipe",
            if mode == Mode::Doctor {
                "checking"
            } else {
                "setting up"
            },
            machine.name,
            root.file_name().unwrap_or_default().to_string_lossy(),
            recipe.name
        );
        let synced = crate::workspace::sync_here(controller, &machine).await?;
        // Saved secrets the recipe checks (a Shorebird token), when this device has them.
        let saved = controller.secrets(&machine).await.unwrap_or_default();
        let secrets = recipe
            .secrets
            .iter()
            .filter(|name| {
                saved
                    .iter()
                    .any(|s| s.name.as_str() == **name && !s.expired(now_ms()))
            })
            .filter_map(|name| SecretName::parse(name))
            .map(|name| SecretUse {
                target: SecretTarget::Env(name.as_str().to_string()),
                name,
            })
            .collect();
        let flags: &[&str] = if mode == Mode::Doctor {
            &["-CheckOnly"]
        } else {
            &[]
        };
        let mut spec = JobSpec::shell("");
        spec.program = recipes::powershell(recipe.project_script, flags);
        spec.cwd = Some(synced.remote.clone());
        spec.label = Some(format!(
            "{} {}",
            if mode == Mode::Doctor {
                "doctor"
            } else {
                "setup"
            },
            recipe.name
        ));
        spec.secrets = secrets;
        spec
    };

    let job = controller.run(&machine, spec).await?;
    eprintln!(
        "[yonder] running on {} as job {}",
        machine.name, job.meta.id
    );
    let code = crate::jobs::follow(controller, &machine, &job.meta.id, 0).await?;
    if code != ExitCode::SUCCESS && mode != Mode::Prep {
        let info = controller.job(&machine, &job.meta.id).await?;
        if info.status.exit_code == Some(NOT_READY) {
            eprintln!(
                "[yonder] tools for every account come from the machine part: `yonder prep {}` \
                 (needs the admin permission: on the machine, `yonder-agent allow \"{}\" admin`).",
                machine.name,
                controller.name()
            );
        }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipes_are_chosen_by_name_or_project() {
        let temp = tempfile::tempdir().unwrap();
        assert!(choose(None, Some(temp.path())).is_err());
        assert!(choose(None, None).is_err());
        std::fs::write(temp.path().join("pubspec.yaml"), "name: app").unwrap();
        std::fs::create_dir(temp.path().join("windows")).unwrap();
        assert_eq!(
            choose(None, Some(temp.path())).unwrap().name,
            "flutter-windows"
        );
        assert_eq!(
            choose(Some("flutter-windows"), None).unwrap().name,
            "flutter-windows"
        );
        assert!(choose(Some("nope"), Some(temp.path())).is_err());
    }
}
