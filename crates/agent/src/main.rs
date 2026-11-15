//! `yonder-agent`: runs on a machine you want to use remotely.
//!
//! Step 1 runs it in a terminal. Step 2 turns `serve` into a Windows service and adds a
//! tray app; `pair`, `controllers` and `revoke` then talk to the running service.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
use yonder_core::agent::{
    Agent, AgentConfig, AgentEvent, PairingCandidate, PairingClosedReason, approver_fn,
};
use yonder_core::audit;
use yonder_core::identity::{DeviceKey, fingerprint};
use yonder_core::net::NetConfig;
use yonder_core::store::{AgentState, PairedController, StateDir, default_agent_dir, format_date};

/// Runs on a machine you want to use remotely.
#[derive(Debug, Parser)]
#[command(name = "yonder-agent", version, about)]
struct Args {
    /// Folder for this machine's key and paired devices.
    #[arg(long, global = true, env = "YONDER_AGENT_DIR")]
    state_dir: Option<PathBuf>,

    /// Relays to use: "default", "none", or comma-separated relay URLs.
    #[arg(long, global = true, env = "YONDER_RELAY", default_value = "default")]
    relay: String,

    /// Don't publish this machine's address through number 0's DNS service.
    #[arg(long, global = true)]
    no_lookup: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Accept connections from paired devices.
    Serve {
        /// Name shown to your devices (default: this computer's name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Let a new device pair with this machine, then keep serving.
    Pair {
        /// How long the pairing ticket stays valid.
        #[arg(long, default_value_t = 10)]
        minutes: u64,
        /// Name shown to your devices (default: this computer's name).
        #[arg(long)]
        name: Option<String>,
    },
    /// List devices paired with this machine.
    Controllers,
    /// Remove a paired device. Takes effect immediately, even while the agent runs.
    Revoke {
        /// Device name or the start of its id.
        device: String,
    },
    /// Show this machine's id and fingerprint.
    Id,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    init_logging();
    let state = StateDir::new(match &args.state_dir {
        Some(dir) => dir.clone(),
        None => default_agent_dir()
            .context("couldn't find a folder for agent state; pass --state-dir")?,
    });
    let net = NetConfig {
        relay: NetConfig::parse_relay(&args.relay)?,
        address_lookup: !args.no_lookup,
    };

    match args.command {
        Cmd::Serve { name } => {
            let agent = start(&state, net, name).await?;
            println!("{} is ready. Press Ctrl-C to stop.", agent.name());
            wait_for_ctrl_c(&agent).await
        }
        Cmd::Pair { minutes, name } => pair(&state, net, name, minutes).await,
        Cmd::Controllers => list_controllers(&state),
        Cmd::Revoke { device } => revoke(&state, &device),
        Cmd::Id => {
            let key = DeviceKey::load_or_create(&state.key_file())?;
            println!("id:          {}", key.id());
            println!("fingerprint: {}", fingerprint(&key.id()));
            println!("state:       {}", state.path().display());
            Ok(())
        }
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,yonder_core=info,yonder_agent=info".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

async fn start(state: &StateDir, net: NetConfig, name: Option<String>) -> Result<Agent> {
    Agent::start(
        AgentConfig {
            state: state.clone(),
            net,
            name,
        },
        approver_fn(ask_at_terminal),
    )
    .await
    .context("couldn't start the agent")
}

/// Asks the person at this machine to approve a pairing request.
async fn ask_at_terminal(candidate: PairingCandidate) -> bool {
    use std::io::Write;
    println!();
    println!("Pairing request from \"{}\"", candidate.controller_name);
    println!("  Fingerprint: {}", candidate.fingerprint);
    println!("  Check that your device shows the same fingerprint.");
    print!("Allow? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await
    {
        Ok(_) => matches!(line.trim().to_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

async fn pair(state: &StateDir, net: NetConfig, name: Option<String>, minutes: u64) -> Result<()> {
    let agent = start(state, net, name).await?;
    let mut events = agent.subscribe();
    let minutes = minutes.max(1);
    let ttl = Duration::from_secs(minutes * 60);
    let ticket = agent.open_pairing(ttl).await;

    println!(
        "This machine: {}  (fingerprint {})",
        agent.name(),
        agent.fingerprint()
    );
    println!();
    println!("On your other device, run:");
    println!();
    println!("  yonder pair {}", ticket.encode());
    println!();
    println!("Pairing is open for {minutes} minutes. Waiting…");

    // Every pairing outcome ends this wait; the agent keeps serving afterwards.
    tokio::select! {
        event = events.recv() => match event {
            Ok(AgentEvent::Paired(c)) => println!("Paired with \"{}\".", c.name),
            Ok(AgentEvent::PairingDeclined(c)) => {
                println!("Declined \"{}\". Run `yonder-agent pair` again to retry.", c.controller_name);
            }
            Ok(AgentEvent::PairingClosed(PairingClosedReason::TooManyWrongTickets)) => {
                println!("Pairing closed after several wrong tickets. Run `yonder-agent pair` again.");
            }
            Ok(AgentEvent::PairingClosed(PairingClosedReason::Expired)) => println!("Pairing expired."),
            Err(_) => {}
        },
        _ = tokio::time::sleep(ttl) => {
            agent.close_pairing();
            println!("Pairing expired.");
        }
        _ = tokio::signal::ctrl_c() => {
            agent.shutdown().await;
            return Ok(());
        }
    }

    println!("Still serving paired devices. Press Ctrl-C to stop.");
    wait_for_ctrl_c(&agent).await
}

async fn wait_for_ctrl_c(agent: &Agent) -> Result<()> {
    tokio::signal::ctrl_c()
        .await
        .context("couldn't listen for Ctrl-C")?;
    println!("Stopping…");
    agent.shutdown().await;
    Ok(())
}

fn list_controllers(state: &StateDir) -> Result<()> {
    let controllers = AgentState::load(&state.agent_file())?.controllers;
    if controllers.is_empty() {
        println!("No devices are paired. Run `yonder-agent pair` to add one.");
        return Ok(());
    }
    let width = controllers
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{:width$}  {:10}  {:10}  PERMISSIONS",
        "NAME", "ID", "PAIRED"
    );
    for c in &controllers {
        println!(
            "{:width$}  {:10}  {:10}  {}",
            c.name,
            c.id.fmt_short().to_string(),
            format_date(c.paired_at),
            c.permissions
        );
    }
    Ok(())
}

fn revoke(state: &StateDir, query: &str) -> Result<()> {
    let path = state.agent_file();
    let mut agent_state = AgentState::load(&path)?;
    let target = find_controller(&agent_state.controllers, query)?;
    agent_state.remove_controller(&target.id);
    agent_state.save(&path)?;
    println!(
        "Revoked \"{}\". It can no longer use this machine.",
        target.name
    );
    Ok(())
}

fn find_controller(controllers: &[PairedController], query: &str) -> Result<PairedController> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        bail!("give a device name or id");
    }
    let mut matches: Vec<_> = controllers
        .iter()
        .filter(|c| c.name.to_lowercase() == q)
        .collect();
    if matches.is_empty() {
        matches = controllers
            .iter()
            .filter(|c| c.id.to_string().starts_with(&q))
            .collect();
    }
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => bail!("no paired device matches \"{query}\" (see `yonder-agent controllers`)"),
        many => bail!(
            "\"{query}\" matches several devices: {}; use more of the id",
            many.iter()
                .map(|c| format!("{} ({})", c.name, c.id.fmt_short()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use yonder_core::permissions::Permissions;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    fn controller(name: &str) -> PairedController {
        PairedController {
            id: DeviceKey::generate().id(),
            name: name.into(),
            permissions: Permissions::for_new_controller(),
            paired_at: 0,
        }
    }

    #[test]
    fn finds_controllers_by_name_or_id() {
        let list = vec![controller("Mac"), controller("Phone")];
        assert_eq!(find_controller(&list, "mac").unwrap().id, list[0].id);
        let prefix = list[1].id.to_string()[..8].to_string();
        assert_eq!(find_controller(&list, &prefix).unwrap().id, list[1].id);
        assert!(find_controller(&list, "tablet").is_err());
        assert!(find_controller(&list, " ").is_err());
    }

    #[test]
    fn revoke_removes_from_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path());
        let mut s = AgentState::default();
        s.upsert_controller(controller("Mac"));
        s.upsert_controller(controller("Phone"));
        s.save(&state.agent_file()).unwrap();

        revoke(&state, "mac").unwrap();
        let left = AgentState::load(&state.agent_file()).unwrap().controllers;
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "Phone");
    }
}
