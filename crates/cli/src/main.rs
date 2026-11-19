//! `yonder`: use your other machines from this one.
//!
//! Machines can be named by their name, the start of their id, or by OS (`win`, `mac`,
//! `linux`, `any`), in which case the least busy reachable one is used.

mod footprint;
mod jobs;
mod secrets;
mod setup;
mod shell;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use yonder_core::controller::{Controller, ControllerConfig};
use yonder_core::net::NetConfig;
use yonder_core::store::{StateDir, default_controller_dir, format_date};
use yonder_core::ticket::PairingTicket;

/// Use your other machines from this one.
#[derive(Debug, Parser)]
#[command(name = "yonder", version, about)]
struct Args {
    /// Folder for this device's key and paired machines.
    #[arg(long, global = true, env = "YONDER_DIR")]
    state_dir: Option<PathBuf>,

    /// Relays to use: "default", "none", or comma-separated relay URLs.
    #[arg(long, global = true, env = "YONDER_RELAY", default_value = "default")]
    relay: String,

    /// Don't look up machine addresses through number 0's DNS service.
    #[arg(long, global = true)]
    no_lookup: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run a command on a machine and show its output as it runs.
    ///
    /// The job keeps running on the machine if you disconnect or press Ctrl-C; reattach
    /// with `yonder attach`. Exits with the job's exit code.
    ///
    /// Example: yonder run win -- flutter build windows
    Run(jobs::RunArgs),
    /// Follow a job's output until it finishes (default: the latest job).
    Attach {
        machine: String,
        /// Job id or its start; default: the latest job.
        job: Option<String>,
    },
    /// Print a job's output so far (default: the latest job).
    Logs {
        machine: String,
        job: Option<String>,
        /// Keep printing new output until the job finishes.
        #[arg(short, long)]
        follow: bool,
    },
    /// List jobs on a machine.
    Jobs { machine: String },
    /// Stop a queued or running job.
    Cancel {
        machine: String,
        /// Job id or its start; default: the latest job.
        job: Option<String>,
    },
    /// Open a shell on a machine, or reattach to one. It keeps running when you detach
    /// (Ctrl-]) or lose the connection.
    ///
    /// Examples: `yonder shell win` (new), `yonder shell win last` (reattach).
    Shell {
        machine: String,
        /// Shell to reattach to: its id, the start of it, or `last`.
        session: Option<String>,
    },
    /// List shells on a machine.
    Shells {
        machine: String,
        /// End this shell (id, start of it, or `last`).
        #[arg(long, value_name = "SESSION")]
        close: Option<String>,
    },
    /// Copy the git project you're in to a machine (only changes are sent).
    ///
    /// Ignored files on the machine, such as build folders, are kept between syncs.
    Sync { machine: String },
    /// Copy files from the machine's copy of this project back here.
    ///
    /// Example: yonder pull win 'build/windows/x64/runner/Release/**'
    Pull {
        machine: String,
        /// Glob patterns relative to the project, such as `build/windows/**`.
        #[arg(required = true)]
        patterns: Vec<String>,
    },
    /// Install what the project you're in needs on a machine (its Flutter, Shorebird…).
    ///
    /// Runs a recipe's project part as the build account; the recipe's machine part
    /// (tools for every account) runs once on the machine as admin.
    Setup {
        machine: String,
        /// Recipe to use (default: the one that fits the project).
        #[arg(long)]
        recipe: Option<String>,
    },
    /// Save, list or delete secrets your jobs use on a machine (tokens, signing keys).
    Secret {
        #[command(subcommand)]
        what: secrets::SecretCmd,
    },
    /// What this device has left on a machine (or on every paired machine).
    Footprint { machine: Option<String> },
    /// Remove what this device left on a machine (`all` for every machine).
    ///
    /// WHAT: everything, jobs, shells, project (the copy of the git project you're in),
    /// secret:NAME or workspace:ID.
    Clean {
        machine: String,
        #[arg(required = true)]
        what: Vec<String>,
    },
    /// Remove this device's project copies a machine hasn't used for a while (off until
    /// you turn it on). Without options, shows the setting.
    Cleanup {
        /// A machine, or `all`.
        machine: String,
        /// Turn it on: copies unused this long (e.g. 30d, 2w) are removed, checked hourly.
        #[arg(long, value_name = "DAYS")]
        after: Option<String>,
        /// Turn it off.
        #[arg(long)]
        off: bool,
    },
    /// Remove everything this device left on a machine, then unpair from it.
    Unpair {
        machine: String,
        /// Keep this device's secrets, project copies and jobs there.
        #[arg(long)]
        keep: bool,
    },
    /// Devices paired with a machine.
    Devices { machine: String },
    /// Cut a lost device off your machines and remove what it left there.
    ///
    /// Example: yonder revoke old-laptop
    Revoke {
        /// Its name or the start of its id (see `yonder devices <machine>`).
        device: String,
        /// Only on this machine (default: every paired machine).
        #[arg(long, default_value = "all")]
        on: String,
        /// Keep what it left (secrets, project copies, jobs).
        #[arg(long)]
        keep: bool,
    },
    /// Show every paired machine: online, paused, busy.
    Status,
    /// Pair with a machine using the ticket `yonder-agent pair` showed on it.
    Pair {
        ticket: String,
        /// Name this device shows to the machine (default: this computer's name).
        #[arg(long)]
        name: Option<String>,
    },
    /// List paired machines.
    Machines,
    /// Check a machine is reachable.
    Ping { machine: String },
    /// Show a machine's details and what this device may do there.
    Info { machine: String },
    /// Remove a machine from this device's list only; `yonder unpair` also leaves the machine.
    Forget { machine: String },
    /// Show this device's id and fingerprint.
    Whoami,
}

fn main() -> ExitCode {
    let args = Args::parse();
    init_logging();
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(async_main(args)) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn async_main(args: Args) -> Result<ExitCode> {
    let state = StateDir::new(match &args.state_dir {
        Some(dir) => dir.clone(),
        None => default_controller_dir()
            .context("couldn't find a folder for Yonder's state; pass --state-dir")?,
    });
    let net = NetConfig {
        relay: NetConfig::parse_relay(&args.relay)?,
        address_lookup: !args.no_lookup,
        port: None,
    };
    let name = match &args.command {
        Cmd::Pair { name, .. } => name.clone(),
        _ => None,
    };
    let controller = Controller::open(ControllerConfig {
        state: state.clone(),
        net,
        name,
    })
    .await
    .context("couldn't start networking")?;

    let result = run(&controller, args.command, &state).await;
    controller.close().await;
    result
}

fn init_logging() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "error".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn run(controller: &Controller, command: Cmd, state: &StateDir) -> Result<ExitCode> {
    match command {
        Cmd::Run(args) => return jobs::run(controller, args).await,
        Cmd::Attach { machine, job } => {
            let machine = controller.pick_machine(&machine).await?;
            let id = jobs::resolve(controller, &machine, job.as_deref()).await?;
            return jobs::follow(controller, &machine, &id, 0).await;
        }
        Cmd::Logs {
            machine,
            job,
            follow,
        } => {
            let machine = controller.pick_machine(&machine).await?;
            let id = jobs::resolve(controller, &machine, job.as_deref()).await?;
            if follow {
                return jobs::follow(controller, &machine, &id, 0).await;
            }
            jobs::print_logs(controller, &machine, &id).await?;
        }
        Cmd::Jobs { machine } => {
            let machine = controller.pick_machine(&machine).await?;
            jobs::list(controller, &machine).await?;
        }
        Cmd::Cancel { machine, job } => {
            let machine = controller.pick_machine(&machine).await?;
            let id = jobs::resolve(controller, &machine, job.as_deref()).await?;
            let job = controller.cancel(&machine, &id).await?;
            println!("Job {id} on {}: {}", machine.name, job.status.state);
        }
        Cmd::Shell { machine, session } => {
            let machine = controller.pick_machine(&machine).await?;
            let session = match session {
                Some(query) => Some(shell::resolve(controller, &machine, &query).await?),
                None => None,
            };
            return shell::run(controller, &machine, session).await;
        }
        Cmd::Shells { machine, close } => {
            let machine = controller.pick_machine(&machine).await?;
            match close {
                Some(query) => {
                    let id = shell::resolve(controller, &machine, &query).await?;
                    controller.close_terminal(&machine, &id).await?;
                    println!("Closed shell {id} on {}.", machine.name);
                }
                None => shell::list(controller, &machine).await?,
            }
        }
        Cmd::Sync { machine } => {
            let machine = controller.pick_machine(&machine).await?;
            let synced = workspace::sync_here(controller, &machine).await?;
            println!("{}", synced.remote);
        }
        Cmd::Pull { machine, patterns } => {
            let machine = controller.pick_machine(&machine).await?;
            workspace::pull_here(controller, &machine, &patterns).await?;
        }
        Cmd::Setup { machine, recipe } => {
            return setup::run(controller, &machine, recipe.as_deref()).await;
        }
        Cmd::Secret { what } => secrets::run(controller, what).await?,
        Cmd::Footprint { machine } => footprint::show(controller, machine).await?,
        Cmd::Clean { machine, what } => footprint::clean(controller, &machine, &what).await?,
        Cmd::Cleanup {
            machine,
            after,
            off,
        } => footprint::cleanup(controller, &machine, after, off).await?,
        Cmd::Unpair { machine, keep } => footprint::unpair(controller, &machine, keep).await?,
        Cmd::Devices { machine } => footprint::devices(controller, &machine).await?,
        Cmd::Revoke { device, on, keep } => {
            footprint::revoke(controller, &device, &on, keep).await?
        }
        Cmd::Status => status(controller).await?,
        Cmd::Pair { ticket, .. } => {
            let ticket = PairingTicket::decode(&ticket)?;
            println!("This device: {}", controller.name());
            println!("Fingerprint: {}", controller.fingerprint());
            println!();
            println!("Approve the request on the machine if it shows this fingerprint.");
            println!("Waiting for approval…");
            let machine = controller.pair(&ticket).await?;
            println!();
            println!(
                "Paired with {} ({}). Use it as \"{}\".",
                machine.name, machine.os, machine.name
            );
        }
        Cmd::Machines => {
            let machines = controller.machines();
            if machines.is_empty() {
                println!(
                    "No machines yet. Run `yonder-agent pair` on a machine, then `yonder pair <ticket>` here."
                );
                return Ok(ExitCode::SUCCESS);
            }
            let width = name_width(machines.iter().map(|m| m.name.as_str()));
            println!("{:width$}  {:7}  {:10}  PAIRED", "NAME", "OS", "ID");
            for m in &machines {
                println!(
                    "{:width$}  {:7}  {:10}  {}",
                    m.name,
                    m.os.to_string(),
                    m.id.fmt_short().to_string(),
                    format_date(m.paired_at)
                );
            }
        }
        Cmd::Ping { machine } => {
            let machine = controller.pick_machine(&machine).await?;
            let rtt = controller.ping(&machine).await?;
            println!("{} answered in {} ms", machine.name, rtt.as_millis());
        }
        Cmd::Info { machine } => {
            let machine = controller.pick_machine(&machine).await?;
            let info = controller.info(&machine).await?;
            println!("name:        {}", info.name);
            println!("os:          {}", info.os);
            println!("agent:       {}", info.agent_version);
            println!("id:          {}", machine.id);
            println!("paused:      {}", if info.paused { "yes" } else { "no" });
            println!("permissions: {}", info.your_permissions);
        }
        Cmd::Forget { machine } => {
            let machine = controller.find_machine(&machine)?;
            controller.forget(&machine.id)?;
            println!(
                "Forgot {}. To stop this device being trusted there, run `yonder-agent revoke {}` on it.",
                machine.name,
                controller.id().fmt_short()
            );
        }
        Cmd::Whoami => {
            println!("name:        {}", controller.name());
            println!("id:          {}", controller.id());
            println!("fingerprint: {}", controller.fingerprint());
            println!("state:       {}", state.path().display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn name_width<'a>(names: impl Iterator<Item = &'a str>) -> usize {
    names.map(|n| n.chars().count()).max().unwrap_or(4).max(4)
}

/// Asks every machine at once and prints one line each.
async fn status(controller: &Controller) -> Result<()> {
    let machines = controller.machines();
    if machines.is_empty() {
        println!(
            "No machines yet. Run `yonder-agent pair` on a machine, then `yonder pair <ticket>` here."
        );
        return Ok(());
    }
    let checks = machines.iter().map(|machine| async move {
        let check = async {
            let rtt = controller.ping(machine).await?;
            let info = controller.info(machine).await?;
            let jobs = controller.jobs(machine).await.unwrap_or_default();
            Ok::<_, yonder_core::Error>((rtt, info, jobs))
        };
        tokio::time::timeout(Duration::from_secs(10), check).await
    });
    let results = futures_util::future::join_all(checks).await;
    let width = name_width(machines.iter().map(|m| m.name.as_str()));
    println!(
        "{:width$}  {:7}  {:8}  {:6}  JOBS",
        "NAME", "OS", "STATE", "PING"
    );
    for (machine, result) in machines.iter().zip(results) {
        let (state, ping, jobs) = match result {
            Ok(Ok((rtt, info, jobs))) => {
                let running = jobs
                    .iter()
                    .filter(|j| j.status.state == yonder_core::jobs::JobState::Running)
                    .count();
                let queued = jobs
                    .iter()
                    .filter(|j| j.status.state == yonder_core::jobs::JobState::Queued)
                    .count();
                (
                    if info.paused { "paused" } else { "online" }.to_string(),
                    format!("{} ms", rtt.as_millis()),
                    format!("{running} running, {queued} queued"),
                )
            }
            Ok(Err(yonder_core::Error::Remote(err))) => {
                ("refused".to_string(), "-".to_string(), err.message)
            }
            _ => ("offline".to_string(), "-".to_string(), String::new()),
        };
        println!(
            "{:width$}  {:7}  {:8}  {:6}  {}",
            machine.name,
            machine.os.to_string(),
            state,
            ping,
            jobs
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn run_takes_everything_after_the_separator() {
        let args = Args::try_parse_from([
            "yonder",
            "run",
            "win",
            "--env",
            "A=1",
            "--",
            "flutter",
            "build",
            "--release",
        ])
        .unwrap();
        let Cmd::Run(run) = args.command else {
            panic!("expected run")
        };
        assert_eq!(run.machine, "win");
        assert_eq!(run.command, ["flutter", "build", "--release"]);
        assert_eq!(run.env, ["A=1"]);
    }
}
