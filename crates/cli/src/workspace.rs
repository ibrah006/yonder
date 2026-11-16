//! `--sync`, `--pull`, `yonder sync` and `yonder pull`: the project you're in, on a machine.
//!
//! The project is the git repository around the current folder. Its files (tracked and
//! untracked, minus ignored ones) are copied to a workspace on the machine; only changes
//! are sent. Ignored files on the machine, such as build folders, stay between runs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use yonder_core::Os;
use yonder_core::controller::Controller;
use yonder_core::store::PairedMachine;
use yonder_core::workspace::{LocalProject, WorkspaceId};

/// The project here, as synced to a machine.
#[derive(Debug)]
pub struct Synced {
    pub id: WorkspaceId,
    pub root: PathBuf,
    /// The workspace folder on the machine.
    pub remote: String,
}

impl Synced {
    /// The folder on the machine matching the current folder here.
    pub fn remote_cwd(&self, machine: &PairedMachine) -> Result<String> {
        let here = std::env::current_dir()?.canonicalize()?;
        let root = self.root.canonicalize()?;
        let relative = here.strip_prefix(&root).unwrap_or(Path::new(""));
        let separator = if machine.os == Os::Windows { "\\" } else { "/" };
        let mut path = self.remote.clone();
        for part in relative.components() {
            path.push_str(separator);
            path.push_str(&part.as_os_str().to_string_lossy());
        }
        Ok(path)
    }
}

fn project_here() -> Result<LocalProject> {
    let here = std::env::current_dir().context("couldn't read the current folder")?;
    Ok(LocalProject::scan(&here)?)
}

/// Copies the project around the current folder to `machine`.
pub async fn sync_here(controller: &Controller, machine: &PairedMachine) -> Result<Synced> {
    let project = project_here()?;
    let id = WorkspaceId::for_project(&project.root, &controller.id());
    eprintln!(
        "[yonder] syncing {} ({} files, {}) to {}…",
        project.root.display(),
        project.files.len(),
        human_bytes(project.total_bytes()),
        machine.name
    );
    if project.skipped > 0 {
        eprintln!(
            "[yonder] left out {} symlinks or special files",
            project.skipped
        );
    }
    let result = controller.sync(machine, &id, &project, |_, _| {}).await?;
    eprintln!(
        "[yonder] synced: sent {} changed files ({})",
        result.sent_files,
        human_bytes(result.sent_bytes)
    );
    Ok(Synced {
        id,
        root: project.root,
        remote: result.path,
    })
}

/// Copies files matching `patterns` from the machine's copy of this project back here.
pub async fn pull_here(
    controller: &Controller,
    machine: &PairedMachine,
    patterns: &[String],
) -> Result<()> {
    let here = std::env::current_dir().context("couldn't read the current folder")?;
    // Only the root is needed; this doesn't hash anything.
    let root = git_root(&here)?;
    let id = WorkspaceId::for_project(&root, &controller.id());
    pull_into(controller, machine, &id, &root, patterns).await
}

pub async fn pull_into(
    controller: &Controller,
    machine: &PairedMachine,
    id: &WorkspaceId,
    root: &Path,
    patterns: &[String],
) -> Result<()> {
    let written = controller.pull(machine, id, patterns, root).await?;
    if written.is_empty() {
        eprintln!("[yonder] nothing new to pull for {}", patterns.join(" "));
    } else {
        let bytes: u64 = written.iter().map(|f| f.size).sum();
        eprintln!(
            "[yonder] pulled {} files ({}) into {}",
            written.len(),
            human_bytes(bytes),
            root.display()
        );
    }
    Ok(())
}

fn git_root(dir: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("couldn't run git")?;
    anyhow::ensure!(
        output.status.success(),
        "{} isn't inside a git repository",
        dir.display()
    );
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

pub fn human_bytes(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} KB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.2} GB", bytes as f64 / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(45_900_000), "45.9 MB");
    }
}
