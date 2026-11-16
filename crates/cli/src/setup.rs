//! `yonder setup`: bring a machine to the state the project you're in needs.
//!
//! A recipe has two parts. The machine part installs tools for every account and needs
//! an administrator, so you run it once on the machine itself. The project part (here)
//! runs as the build account in the machine's copy of the project: it installs the
//! project's own versions (Flutter through fvm, Shorebird) and lists what's still missing.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use yonder_core::Os;
use yonder_core::controller::Controller;
use yonder_core::jobs::{JobProgram, JobSpec};

/// A recipe the CLI carries.
struct Recipe {
    name: &'static str,
    os: Os,
    /// Says what the recipe is for, in `--recipe` errors.
    about: &'static str,
    /// The machine part's path in the Yonder repository.
    machine_script: &'static str,
    /// PowerShell run as the build account, in the project's workspace.
    project_script: &'static str,
}

const RECIPES: &[Recipe] = &[Recipe {
    name: "flutter-windows",
    os: Os::Windows,
    about: "Flutter apps for Windows, with Shorebird when the project uses it",
    machine_script: r"recipes\flutter-windows\machine.ps1",
    project_script: include_str!("../../../recipes/flutter-windows/project.ps1"),
}];

/// Exit code of a project script that finished but left things to do.
const NOT_READY: i32 = 2;

/// Picks the recipe named, or the one that fits the project at `root`.
fn choose(name: Option<&str>, root: &Path) -> Result<&'static Recipe> {
    if let Some(name) = name {
        return match RECIPES.iter().find(|r| r.name == name) {
            Some(recipe) => Ok(recipe),
            None => bail!("no recipe called {name}; there is:\n{}", list()),
        };
    }
    if root.join("pubspec.yaml").is_file() && root.join("windows").is_dir() {
        return Ok(&RECIPES[0]);
    }
    bail!(
        "no recipe fits {}; name one with --recipe:\n{}",
        root.display(),
        list()
    )
}

fn list() -> String {
    RECIPES
        .iter()
        .map(|r| format!("  {:16} {}", r.name, r.about))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Arguments that run `script` in Windows PowerShell. The script travels as base64 inside
/// a short command that writes it to a temporary file and runs it, so it arrives whole
/// without quoting. (`-EncodedCommand` would be shorter, but Windows PowerShell then
/// writes its messages as serialized XML instead of text.)
fn powershell_args(script: &str) -> Vec<String> {
    let payload = data_encoding::BASE64.encode(script.as_bytes());
    let command = format!(
        "$f = Join-Path $env:TEMP ('yonder-recipe-' + $PID + '.ps1'); \
         [IO.File]::WriteAllText($f, [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{payload}'))); \
         try {{ & $f; $code = $LASTEXITCODE }} finally {{ Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue }}; \
         exit $code"
    );
    vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-OutputFormat".into(),
        "Text".into(),
        "-Command".into(),
        command,
    ]
}

pub async fn run(controller: &Controller, machine: &str, recipe: Option<&str>) -> Result<ExitCode> {
    let machine = controller.pick_machine(machine).await?;
    let here = std::env::current_dir()?;
    let root = crate::workspace::git_root(&here)?;
    let recipe = choose(recipe, &root)?;
    if machine.os != recipe.os {
        bail!(
            "the {} recipe is for {} machines; {} is {}",
            recipe.name,
            recipe.os.short_name(),
            machine.name,
            machine.os.short_name()
        );
    }
    eprintln!(
        "[yonder] setting {} up for {} with the {} recipe",
        machine.name,
        root.file_name().unwrap_or_default().to_string_lossy(),
        recipe.name
    );
    let synced = crate::workspace::sync_here(controller, &machine).await?;
    let spec = JobSpec {
        program: JobProgram::Exec {
            program: "powershell.exe".into(),
            args: powershell_args(recipe.project_script),
        },
        cwd: Some(synced.remote.clone()),
        env: Vec::new(),
        label: Some(format!("setup {}", recipe.name)),
    };
    let job = controller.run(&machine, spec).await?;
    eprintln!(
        "[yonder] running on {} as job {}",
        machine.name, job.meta.id
    );
    let code = crate::jobs::follow(controller, &machine, &job.meta.id, 0).await?;
    if code != ExitCode::SUCCESS {
        let info = controller.job(&machine, &job.meta.id).await?;
        if info.status.exit_code == Some(NOT_READY) {
            eprintln!(
                "[yonder] tools for every account come from the machine part: on {}, in an \
                 Administrator PowerShell, run {} from a Yonder checkout. Then run `yonder setup {}` again.",
                machine.name, recipe.machine_script, machine.name
            );
        }
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flutter_projects_get_the_flutter_recipe() {
        let temp = tempfile::tempdir().unwrap();
        assert!(choose(None, temp.path()).is_err());
        std::fs::write(temp.path().join("pubspec.yaml"), "name: app").unwrap();
        std::fs::create_dir(temp.path().join("windows")).unwrap();
        assert_eq!(choose(None, temp.path()).unwrap().name, "flutter-windows");
        assert!(choose(Some("nope"), temp.path()).is_err());
    }

    #[test]
    fn scripts_fit_on_a_windows_command_line() {
        // CreateProcess allows 32,767 characters for the whole line.
        for recipe in RECIPES {
            let args = powershell_args(recipe.project_script);
            let length: usize = args.iter().map(|a| a.len() + 1).sum();
            assert!(length < 30_000, "{} is {length} characters", recipe.name);
        }
    }

    #[test]
    fn script_travels_as_base64_in_a_plain_command() {
        let args = powershell_args("exit 2");
        assert_eq!(args[args.len() - 2], "-Command");
        let command = args.last().unwrap();
        assert!(
            command.contains("FromBase64String('ZXhpdCAy')"),
            "{command}"
        );
        // Arrives as one argument: nothing Windows quoting would need to escape.
        assert!(!command.contains('"'));
    }
}
