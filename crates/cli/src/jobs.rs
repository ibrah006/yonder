//! `yonder run`, `attach`, `logs`, `jobs`: running work and watching it.

use std::io::Write;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail};
use yonder_core::controller::Controller;
use yonder_core::jobs::{JobId, JobInfo, JobProgram, JobSpec, JobState};
use yonder_core::store::{PairedMachine, format_datetime_ms};

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Machine name, the start of its id, or `win` / `mac` / `linux` / `any`.
    pub machine: String,
    /// Start the job and return without showing its output.
    #[arg(short, long)]
    pub detach: bool,
    /// Working folder on the machine.
    #[arg(long)]
    pub cwd: Option<String>,
    /// Environment variable for the job, as NAME=value. Repeatable.
    #[arg(short, long, value_name = "NAME=VALUE")]
    pub env: Vec<String>,
    /// Short name shown in job lists.
    #[arg(long)]
    pub label: Option<String>,
    /// Run the program directly instead of through the machine's shell.
    #[arg(long)]
    pub exec: bool,
    /// Copy the git project you're in to the machine first (only changes are sent) and
    /// run there, in the folder matching this one.
    #[arg(short, long)]
    pub sync: bool,
    /// After the job succeeds, copy files matching this pattern (relative to the project,
    /// such as `build/windows/**`) back here. Implies --sync. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    pub pull: Vec<String>,
    /// The command, after `--`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

impl RunArgs {
    pub fn spec(&self) -> Result<JobSpec> {
        let program = if self.exec {
            JobProgram::Exec {
                program: self.command[0].clone(),
                args: self.command[1..].to_vec(),
            }
        } else {
            // Like ssh: the words become one line for the machine's shell.
            JobProgram::Shell(self.command.join(" "))
        };
        let env = self
            .env
            .iter()
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) if !name.is_empty() => {
                    Ok((name.to_string(), value.to_string()))
                }
                _ => bail!("--env needs NAME=value, got \"{pair}\""),
            })
            .collect::<Result<_>>()?;
        Ok(JobSpec {
            program,
            cwd: self.cwd.clone(),
            env,
            label: self.label.clone(),
        })
    }
}

pub async fn run(controller: &Controller, args: RunArgs) -> Result<ExitCode> {
    let mut spec = args.spec()?;
    let machine = controller.pick_machine(&args.machine).await?;
    let synced = if args.sync || !args.pull.is_empty() {
        let synced = crate::workspace::sync_here(controller, &machine).await?;
        if spec.cwd.is_none() {
            spec.cwd = Some(synced.remote_cwd(&machine)?);
        }
        Some(synced)
    } else {
        None
    };
    let job = controller.run(&machine, spec).await?;
    let id = job.meta.id.clone();
    if args.detach {
        println!("{id}");
        eprintln!(
            "Started on {}. Follow it with `yonder attach {} {id}`.",
            machine.name, machine.name
        );
        return Ok(ExitCode::SUCCESS);
    }
    if job.status.state == JobState::Queued {
        eprintln!(
            "[yonder] queued on {}; waiting for a free slot…",
            machine.name
        );
    } else {
        eprintln!("[yonder] running on {} as job {id}", machine.name);
    }
    follow(controller, &machine, &id, 0).await
}

/// Streams a job's output to stdout until it finishes, reconnecting after network drops.
/// Ctrl-C detaches and leaves the job running.
pub async fn follow(
    controller: &Controller,
    machine: &PairedMachine,
    id: &JobId,
    from: u64,
) -> Result<ExitCode> {
    let mut offset = from;
    let mut backoff = Duration::from_secs(1);
    let mut warned = false;
    loop {
        let attempt = controller.logs(machine, id, offset, true, |chunk_offset, data| {
            // Skip anything already printed (a resend after reconnecting).
            let skip = offset.saturating_sub(chunk_offset) as usize;
            if skip < data.len() {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&data[skip..]);
                let _ = out.flush();
                offset = chunk_offset + data.len() as u64;
            }
        });
        let result = tokio::select! {
            result = attempt => result,
            _ = tokio::signal::ctrl_c() => {
                eprintln!();
                eprintln!("[yonder] detached; job {id} keeps running on {}.", machine.name);
                eprintln!("[yonder] reattach: yonder attach {} {id}", machine.name);
                eprintln!("[yonder] stop it:  yonder cancel {} {id}", machine.name);
                return Ok(ExitCode::from(130));
            }
        };
        match result {
            Ok(job) => return Ok(finish_message(machine, &job)),
            Err(yonder_core::Error::Remote(err)) => bail!(err.to_string()),
            Err(err) => {
                if !warned {
                    eprintln!(
                        "\n[yonder] connection lost ({err}); reconnecting… (Ctrl-C to stop watching)"
                    );
                    warned = true;
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("[yonder] job {id} keeps running; reattach with `yonder attach {} {id}`.", machine.name);
                        return Ok(ExitCode::from(130));
                    }
                }
                backoff = (backoff * 2).min(Duration::from_secs(15));
            }
        }
    }
}

fn finish_message(machine: &PairedMachine, job: &JobInfo) -> ExitCode {
    let status = &job.status;
    let took = match (status.started_at_ms, status.finished_at_ms) {
        (Some(start), Some(end)) => format!(" in {}", human_duration(end.saturating_sub(start))),
        _ => String::new(),
    };
    let detail = status
        .message
        .as_deref()
        .map(|m| format!(": {m}"))
        .unwrap_or_default();
    match status.state {
        JobState::Succeeded => {
            eprintln!("[yonder] succeeded on {}{took}", machine.name);
            ExitCode::SUCCESS
        }
        JobState::Failed => {
            let code = status.exit_code.unwrap_or(1);
            eprintln!(
                "[yonder] failed on {} with exit code {code}{took}{detail}",
                machine.name
            );
            ExitCode::from(u8::try_from(code).ok().filter(|c| *c != 0).unwrap_or(1))
        }
        JobState::Cancelled => {
            eprintln!("[yonder] cancelled{took}");
            ExitCode::from(130)
        }
        other => {
            eprintln!("[yonder] job ended as {other}{detail}");
            ExitCode::FAILURE
        }
    }
}

fn human_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{}.{}s", secs, (ms % 1000) / 100),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// Turns what the user typed into a job id: nothing or `last` means the newest job;
/// otherwise a full id or its start.
pub async fn resolve(
    controller: &Controller,
    machine: &PairedMachine,
    query: Option<&str>,
) -> Result<JobId> {
    if let Some(q) = query
        && let Some(id) = JobId::parse(q)
    {
        return Ok(id);
    }
    let jobs = controller.jobs(machine).await?;
    match query.map(str::trim) {
        None | Some("last") | Some("") => match jobs.first() {
            Some(job) => Ok(job.meta.id.clone()),
            None => bail!("{} has no jobs yet", machine.name),
        },
        Some(prefix) => {
            let prefix = prefix.to_lowercase();
            let matches: Vec<_> = jobs
                .iter()
                .filter(|j| j.meta.id.as_str().starts_with(&prefix))
                .collect();
            match matches.as_slice() {
                [one] => Ok(one.meta.id.clone()),
                [] => bail!("no job on {} starts with \"{prefix}\"", machine.name),
                _ => bail!("several jobs start with \"{prefix}\"; type more of the id"),
            }
        }
    }
}

pub async fn print_logs(
    controller: &Controller,
    machine: &PairedMachine,
    id: &JobId,
) -> Result<()> {
    let job = controller
        .logs(machine, id, 0, false, |_, data| {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(data);
            let _ = out.flush();
        })
        .await?;
    if !job.status.state.is_finished() {
        eprintln!(
            "[yonder] still {}; `yonder attach {} {id}` to follow",
            job.status.state, machine.name
        );
    }
    Ok(())
}

pub async fn list(controller: &Controller, machine: &PairedMachine) -> Result<()> {
    let jobs = controller.jobs(machine).await?;
    if jobs.is_empty() {
        println!("No jobs on {} yet.", machine.name);
        return Ok(());
    }
    println!(
        "{:16}  {:9}  {:16}  {:9}  COMMAND",
        "ID", "STATE", "STARTED", "TOOK"
    );
    for job in jobs {
        let s = &job.status;
        let started = s
            .started_at_ms
            .map(format_datetime_ms)
            .unwrap_or_else(|| "-".into());
        let took = match (s.started_at_ms, s.finished_at_ms) {
            (Some(a), Some(b)) => human_duration(b.saturating_sub(a)),
            _ => "-".into(),
        };
        let state = match (s.state, s.exit_code) {
            (JobState::Failed, Some(code)) => format!("failed {code}"),
            (state, _) => state.to_string(),
        };
        let what = job
            .meta
            .spec
            .label
            .clone()
            .unwrap_or_else(|| job.meta.spec.summary());
        println!(
            "{:16}  {:9}  {:16}  {:9}  {}",
            job.meta.id.as_str(),
            state,
            started,
            took,
            what.chars().take(70).collect::<String>()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &[&str]) -> RunArgs {
        RunArgs {
            machine: "win".into(),
            detach: false,
            cwd: None,
            env: vec![],
            label: None,
            exec: false,
            command: command.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn words_become_one_shell_line() {
        let spec = args(&["flutter", "build", "windows"]).spec().unwrap();
        assert_eq!(
            spec.program,
            JobProgram::Shell("flutter build windows".into())
        );
    }

    #[test]
    fn exec_keeps_arguments_apart() {
        let mut a = args(&["git", "commit", "-m", "two words"]);
        a.exec = true;
        let JobProgram::Exec { program, args } = a.spec().unwrap().program else {
            panic!("expected exec")
        };
        assert_eq!(program, "git");
        assert_eq!(args, ["commit", "-m", "two words"]);
    }

    #[test]
    fn env_needs_name_and_value() {
        let mut a = args(&["x"]);
        a.env = vec!["TOKEN=abc=def".into()];
        assert_eq!(a.spec().unwrap().env, [("TOKEN".into(), "abc=def".into())]);
        a.env = vec!["novalue".into()];
        assert!(a.spec().is_err());
        a.env = vec!["=x".into()];
        assert!(a.spec().is_err());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(1_500), "1.5s");
        assert_eq!(human_duration(125_000), "2m 05s");
        assert_eq!(human_duration(3_720_000), "1h 02m");
    }
}
