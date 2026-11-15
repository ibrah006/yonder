//! `yonder`: use your other machines from this one.
//!
//! Machines can be named by their name, the start of their id, or by OS (`win`, `mac`,
//! `linux`, `any`), in which case the least busy reachable one is used.

mod jobs;

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
    /// Remove a machine from this device's list. Revoke it on the machine too.
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
                return Ok(());
            }
            let width = machines
                .iter()
                .map(|m| m.name.chars().count())
                .max()
                .unwrap_or(4)
                .max(4);
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
            let machine = controller.find_machine(&machine)?;
            let rtt = controller.ping(&machine).await?;
            println!("{} answered in {} ms", machine.name, rtt.as_millis());
        }
        Cmd::Info { machine } => {
            let machine = controller.find_machine(&machine)?;
            let info = controller.info(&machine).await?;
            println!("name:        {}", info.name);
            println!("os:          {}", info.os);
            println!("agent:       {}", info.agent_version);
            println!("id:          {}", machine.id);
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
}
