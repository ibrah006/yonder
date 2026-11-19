//! Setup recipes the CLI and the app carry: scripts that bring a machine to what a
//! project needs. See `recipes/README.md`.
//!
//! - The *machine part* installs tools for every account and changes system settings;
//!   it runs as a job with administrator rights (`yonder prep`), which needs the
//!   machine's owner to grant the Admin permission.
//! - The *project part* runs as the build account in the machine's copy of the project
//!   (`yonder setup`); with `-CheckOnly` it only reports (`yonder doctor`).

use std::path::Path;

use crate::Os;
use crate::jobs::JobProgram;

#[derive(Debug)]
pub struct Recipe {
    pub name: &'static str,
    pub os: Os,
    pub about: &'static str,
    /// PowerShell run with administrator rights: tools for every account.
    pub machine_script: &'static str,
    /// PowerShell run as the build account in the project's copy.
    pub project_script: &'static str,
    /// Saved secrets the project part uses when this device has them.
    pub secrets: &'static [&'static str],
}

pub const RECIPES: &[Recipe] = &[Recipe {
    name: "flutter-windows",
    os: Os::Windows,
    about: "Flutter apps for Windows, with Shorebird when the project uses it",
    machine_script: include_str!("../../../recipes/flutter-windows/machine.ps1"),
    project_script: include_str!("../../../recipes/flutter-windows/project.ps1"),
    secrets: &["SHOREBIRD_TOKEN"],
}];

/// Exit code of a project script that finished but left things to do.
pub const NOT_READY: i32 = 2;

pub fn by_name(name: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|r| r.name == name)
}

/// The recipe that fits the project at `root`, if one does.
pub fn fitting(root: &Path) -> Option<&'static Recipe> {
    if root.join("pubspec.yaml").is_file() && root.join("windows").is_dir() {
        return by_name("flutter-windows");
    }
    None
}

/// For lists and errors: one line per recipe.
pub fn describe_all() -> String {
    RECIPES
        .iter()
        .map(|r| format!("  {:16} {}", r.name, r.about))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A job program running `script` in Windows PowerShell with `flags` (such as
/// `-CheckOnly`). The script travels as base64 inside a short command that writes it to
/// a temporary file and runs it, so it arrives whole without quoting. (`-EncodedCommand`
/// would be shorter, but Windows PowerShell then writes its messages as XML.)
pub fn powershell(script: &str, flags: &[&str]) -> JobProgram {
    let payload = data_encoding::BASE64.encode(script.as_bytes());
    let flags: String = flags
        .iter()
        .filter(|f| f.starts_with('-') && f[1..].chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|f| format!(" {f}"))
        .collect();
    let command = format!(
        "$f = Join-Path $env:TEMP ('yonder-recipe-' + $PID + '.ps1'); \
         [IO.File]::WriteAllText($f, [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{payload}'))); \
         try {{ & $f{flags}; $code = $LASTEXITCODE }} finally {{ Remove-Item -LiteralPath $f -ErrorAction SilentlyContinue }}; \
         exit $code"
    );
    JobProgram::Exec {
        program: "powershell.exe".into(),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-OutputFormat".into(),
            "Text".into(),
            "-Command".into(),
            command,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(program: JobProgram) -> Vec<String> {
        match program {
            JobProgram::Exec { args, .. } => args,
            JobProgram::Shell(_) => unreachable!(),
        }
    }

    #[test]
    fn flutter_projects_get_the_flutter_recipe() {
        let temp = tempfile::tempdir().unwrap();
        assert!(fitting(temp.path()).is_none());
        std::fs::write(temp.path().join("pubspec.yaml"), "name: app").unwrap();
        std::fs::create_dir(temp.path().join("windows")).unwrap();
        assert_eq!(fitting(temp.path()).unwrap().name, "flutter-windows");
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn scripts_fit_on_a_windows_command_line() {
        // CreateProcess allows 32,767 characters for the whole line.
        for recipe in RECIPES {
            for script in [recipe.machine_script, recipe.project_script] {
                let length: usize = args(powershell(script, &["-CheckOnly"]))
                    .iter()
                    .map(|a| a.len() + 1)
                    .sum();
                assert!(length < 30_000, "{} is {length} characters", recipe.name);
            }
        }
    }

    #[test]
    fn script_travels_as_base64_with_safe_flags() {
        let a = args(powershell("exit 2", &["-CheckOnly", "; rm -r C:\\"]));
        assert_eq!(a[a.len() - 2], "-Command");
        let command = a.last().unwrap();
        assert!(
            command.contains("FromBase64String('ZXhpdCAy')"),
            "{command}"
        );
        assert!(command.contains("& $f -CheckOnly;"), "{command}");
        assert!(!command.contains("rm -r"));
        assert!(!command.contains('"'));
    }
}
