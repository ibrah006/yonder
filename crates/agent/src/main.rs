//! `yonder-agent`: runs on a machine you want to use remotely.
//!
//! Step 1 runs it in a terminal. Step 2 turns `serve` into a Windows service and adds a
//! tray app; `pair`, `controllers` and `revoke` then talk to the running service.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(windows)]
mod build_user;
mod ipc;
#[cfg(windows)]
mod service;

use yonder_core::agent::{Agent, AgentConfig};
use yonder_core::audit;
use yonder_core::identity::{DeviceKey, fingerprint};
use yonder_core::jobs::JobStore;
use yonder_core::jobs::manager::{JobsConfig, RunnerCommand};
use yonder_core::net::NetConfig;
use yonder_core::store::{
    AgentState, PairedController, StateDir, default_agent_dir, format_date, format_datetime_ms,
};

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

    /// Fixed UDP port to listen on (default: any free port). Useful with strict firewalls.
    #[arg(long, global = true, env = "YONDER_PORT")]
    port: Option<u16>,

    /// How many jobs may run at once; the rest wait their turn.
    #[arg(long, global = true, default_value_t = 2)]
    max_jobs: usize,

    /// Service only: run jobs and shells as LocalSystem (full administrator) instead of
    /// the limited `yonder-build` account. Give this to `service install`.
    #[arg(long, global = true)]
    run_as_system: bool,

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
    /// Refuse new jobs until `resume`. Running jobs carry on.
    Pause,
    /// Accept new jobs again.
    Resume,
    /// List jobs on this machine.
    Jobs,
    /// Show recent requests from paired devices.
    Audit {
        /// How many entries to show.
        #[arg(long, default_value_t = 20)]
        count: usize,
    },
    /// Show whether the agent (and on Windows, the service) is running.
    Status,
    /// Manage the Windows service (run from an administrator terminal).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Runs one job. Started by the agent; not for direct use.
    #[command(hide = true)]
    JobRun { job_dir: PathBuf },
    /// Starts a job runner or shell as the build user. Used by the service.
    #[command(hide = true)]
    BuildUser {
        #[command(subcommand)]
        what: BuildUserCmd,
    },
}

#[derive(Debug, Subcommand)]
enum BuildUserCmd {
    Job { job_dir: PathBuf },
    Shell,
}

#[derive(Debug, Subcommand)]
enum ServiceAction {
    /// Install (or update) and start the service; it then starts with Windows.
    Install,
    /// Stop and remove the service. Paired devices and job history are kept.
    Uninstall,
    Start,
    Stop,
    /// Entry point used by the service manager.
    #[command(hide = true)]
    Run,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Cmd::JobRun { job_dir } = &args.command {
        // The runner is a plain process: no async runtime, no networking.
        let state = yonder_core::jobs::runner::run(job_dir)?;
        std::process::exit(if state.is_finished() { 0 } else { 1 });
    }
    if let Cmd::BuildUser { what } = &args.command {
        return as_build_user(&args, what);
    }
    if matches!(
        args.command,
        Cmd::Service {
            action: ServiceAction::Run
        }
    ) {
        return run_as_service(args);
    }
    init_logging(None);
    tokio::runtime::Runtime::new()?.block_on(async_main(args))
}

fn state_dir(args: &Args) -> Result<StateDir> {
    Ok(StateDir::new(match &args.state_dir {
        Some(dir) => dir.clone(),
        None => default_agent_dir()
            .context("couldn't find a folder for agent state; pass --state-dir")?,
    }))
}

fn settings(args: &Args) -> Result<(NetConfig, JobsConfig)> {
    let net = NetConfig {
        relay: NetConfig::parse_relay(&args.relay)?,
        address_lookup: !args.no_lookup,
        port: args.port,
    };
    let jobs = JobsConfig {
        runner: RunnerCommand::agent_binary(
            std::env::current_exe().context("couldn't find the agent's own program file")?,
        ),
        max_parallel: args.max_jobs.max(1),
    };
    Ok((net, jobs))
}

#[cfg(windows)]
fn run_as_service(args: Args) -> Result<()> {
    let state = state_dir(&args)?;
    let _ = service::secure_dir(state.path());
    init_logging(Some(&state.path().join("agent.log")));
    let (net, mut jobs) = settings(&args)?;
    let shell = use_build_user(&args, &state, &mut jobs)?;
    service::run(Box::new(move |stop| {
        tokio::runtime::Runtime::new()?.block_on(run_until(&state, net, None, jobs, shell, async {
            let _ = stop.await;
        }))
    }))
}

/// The service starts jobs and shells through `build-user`, unless installed with
/// `--run-as-system`.
#[cfg(windows)]
fn use_build_user(
    args: &Args,
    state: &StateDir,
    jobs: &mut JobsConfig,
) -> Result<Option<yonder_core::terminal::ShellCommand>> {
    if args.run_as_system {
        tracing::warn!("jobs and shells run as LocalSystem (--run-as-system)");
        return Ok(None);
    }
    if !build_user::is_set_up(state.path()) {
        tracing::warn!(
            "the build user isn't set up; jobs and shells will fail until `yonder-agent service install` is run again"
        );
    }
    let program = std::env::current_exe().context("couldn't find the agent's own program file")?;
    let mut prefix: Vec<std::ffi::OsString> = Vec::new();
    if let Some(dir) = &args.state_dir {
        prefix.extend(["--state-dir".into(), dir.clone().into_os_string()]);
    }
    prefix.push("build-user".into());
    jobs.runner = RunnerCommand {
        program: program.clone(),
        args: [prefix.clone(), vec!["job".into()]].concat(),
    };
    Ok(Some(yonder_core::terminal::ShellCommand {
        program,
        args: [prefix, vec!["shell".into()]].concat(),
    }))
}

#[cfg(windows)]
fn as_build_user(args: &Args, what: &BuildUserCmd) -> Result<()> {
    let state = state_dir(args)?;
    let launch = match what {
        BuildUserCmd::Job { job_dir } => build_user::Launch::Job(job_dir),
        BuildUserCmd::Shell => build_user::Launch::Shell,
    };
    match build_user::run(state.path(), launch) {
        Ok(code) => std::process::exit(code as i32),
        Err(err) => {
            match what {
                BuildUserCmd::Job { job_dir } => {
                    build_user::record_failure(job_dir, &format!("{err:#}"))
                }
                BuildUserCmd::Shell => {
                    eprintln!("Couldn't start the shell as {}: {err:#}", build_user::USER)
                }
            }
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn as_build_user(_args: &Args, _what: &BuildUserCmd) -> Result<()> {
    bail!("the build user is Windows-only")
}

#[cfg(not(windows))]
fn run_as_service(_args: Args) -> Result<()> {
    bail!("the service is Windows-only for now; use `yonder-agent serve`")
}

async fn async_main(args: Args) -> Result<()> {
    let state = state_dir(&args)?;
    let (net, jobs) = settings(&args)?;
    let service_args = args_for_service(&args);

    match args.command {
        Cmd::Serve { name } => serve(&state, net, name, jobs).await,
        Cmd::Pair { minutes, name } => pair(&state, net, name, jobs, minutes).await,
        Cmd::Status => status(&state).await,
        Cmd::Service { action } => {
            service_command(action, &service_args, &state, !args.run_as_system)
        }
        Cmd::Controllers => list_controllers(&state),
        Cmd::Revoke { device } => revoke(&state, &device),
        Cmd::Id => {
            let key = DeviceKey::load_or_create(&state.key_file())?;
            println!("id:          {}", key.id());
            println!("fingerprint: {}", fingerprint(&key.id()));
            println!("state:       {}", state.path().display());
            Ok(())
        }
        Cmd::Pause => set_paused(&state, true),
        Cmd::Resume => set_paused(&state, false),
        Cmd::Jobs => list_jobs(&state),
        Cmd::Audit { count } => show_audit(&state, count),
        Cmd::JobRun { .. } | Cmd::BuildUser { .. } => {
            unreachable!("handled before the runtime starts")
        }
    }
}

/// Settings the installed service should run with.
fn args_for_service(args: &Args) -> Vec<std::ffi::OsString> {
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    if let Some(dir) = &args.state_dir {
        out.extend(["--state-dir".into(), dir.clone().into_os_string()]);
    }
    if args.relay != "default" {
        out.extend(["--relay".into(), args.relay.clone().into()]);
    }
    if args.no_lookup {
        out.push("--no-lookup".into());
    }
    if let Some(port) = args.port {
        out.extend(["--port".into(), port.to_string().into()]);
    }
    out.extend(["--max-jobs".into(), args.max_jobs.to_string().into()]);
    if args.run_as_system {
        out.push("--run-as-system".into());
    }
    out
}

#[cfg(windows)]
fn service_command(
    action: ServiceAction,
    service_args: &[std::ffi::OsString],
    state: &StateDir,
    build_user: bool,
) -> Result<()> {
    match action {
        ServiceAction::Install => service::install(state.path(), service_args.to_vec(), build_user),
        ServiceAction::Uninstall => service::uninstall(),
        ServiceAction::Start => service::control(true),
        ServiceAction::Stop => service::control(false),
        ServiceAction::Run => unreachable!("handled before the runtime starts"),
    }
}

#[cfg(not(windows))]
fn service_command(
    _action: ServiceAction,
    _service_args: &[std::ffi::OsString],
    _state: &StateDir,
    _build_user: bool,
) -> Result<()> {
    bail!(
        "the service is Windows-only for now; use `yonder-agent serve` (the macOS agent comes later)"
    )
}

fn set_paused(state: &StateDir, paused: bool) -> Result<()> {
    let path = state.agent_file();
    let mut agent_state = AgentState::load(&path)?;
    agent_state.paused = paused;
    agent_state.save(&path)?;
    if paused {
        println!(
            "Paused: new jobs are refused. Running jobs carry on. `yonder-agent resume` to undo."
        );
    } else {
        println!("Resumed: new jobs are accepted.");
    }
    Ok(())
}

fn list_jobs(state: &StateDir) -> Result<()> {
    let jobs = JobStore::new(state.jobs_dir()).list();
    if jobs.is_empty() {
        println!("No jobs yet.");
        return Ok(());
    }
    println!(
        "{:16}  {:9}  {:16}  {:14}  COMMAND",
        "ID", "STATE", "STARTED", "BY"
    );
    for job in jobs {
        let started = job
            .status
            .started_at_ms
            .map(format_datetime_ms)
            .unwrap_or_else(|| "-".into());
        println!(
            "{:16}  {:9}  {:16}  {:14}  {}",
            job.meta.id.as_str(),
            job.status.state.name(),
            started,
            truncate(&job.meta.controller_name, 14),
            truncate(&job.meta.spec.summary(), 60)
        );
    }
    Ok(())
}

fn show_audit(state: &StateDir, count: usize) -> Result<()> {
    let entries = audit::read_recent(&state.audit_file(), count)?;
    if entries.is_empty() {
        println!("No requests recorded yet.");
        return Ok(());
    }
    println!(
        "{:16}  {:14}  {:11}  {:16}  DETAIL",
        "TIME (UTC)", "DEVICE", "COMMAND", "OUTCOME"
    );
    for e in entries {
        let device = e
            .controller_name
            .clone()
            .unwrap_or_else(|| e.controller.fmt_short().to_string());
        println!(
            "{:16}  {:14}  {:11}  {:16}  {}",
            format_datetime_ms(e.at_ms),
            truncate(&device, 14),
            e.command,
            e.outcome,
            truncate(e.detail.as_deref().unwrap_or(""), 60)
        );
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn init_logging(file: Option<&std::path::Path>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,yonder_core=info,yonder_agent=info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match file.and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    }) {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init(),
        None => builder.with_writer(std::io::stderr).init(),
    }
}

/// Holds `<state>/agent.lock` for as long as an agent runs on that folder, so two agents
/// (say, the service and a terminal) never share one identity and state.
struct InstanceLock(#[allow(dead_code)] std::fs::File);

impl InstanceLock {
    /// `Ok(None)` if another agent holds the lock.
    fn try_acquire(state: &StateDir) -> Result<Option<InstanceLock>> {
        std::fs::create_dir_all(state.path())
            .with_context(|| format!("couldn't create {}", state.path().display()))?;
        let path = state.path().join("agent.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("couldn't open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(InstanceLock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(err)) => {
                Err(err).with_context(|| format!("couldn't lock {}", path.display()))
            }
        }
    }

    /// Takes the lock, or explains that an agent is already running.
    fn acquire(state: &StateDir) -> Result<InstanceLock> {
        Self::try_acquire(state)?.with_context(|| {
            format!(
                "an agent is already running for {} (is the service installed?). \
                 Use `yonder-agent status`, or stop it first.",
                state.path().display()
            )
        })
    }
}

/// Starts the agent and its local control channel in this process.
async fn start_local(
    state: &StateDir,
    net: NetConfig,
    name: Option<String>,
    jobs: JobsConfig,
    shell: Option<yonder_core::terminal::ShellCommand>,
) -> Result<(Agent, tokio::task::JoinHandle<Result<()>>)> {
    let desk = ipc::Desk::new();
    let agent = Agent::start(
        AgentConfig {
            state: state.clone(),
            net,
            name,
            jobs: Some(jobs),
            shell,
        },
        desk.approver(),
    )
    .await
    .context("couldn't start the agent")?;
    let local = tokio::spawn(ipc::serve(agent.clone(), desk, state.clone()));
    Ok((agent, local))
}

/// Runs the agent until `stop` resolves.
#[cfg(windows)]
async fn run_until(
    state: &StateDir,
    net: NetConfig,
    name: Option<String>,
    jobs: JobsConfig,
    shell: Option<yonder_core::terminal::ShellCommand>,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let _lock = InstanceLock::acquire(state)?;
    let (agent, local) = start_local(state, net, name, jobs, shell).await?;
    stop.await;
    agent.shutdown().await;
    local.abort();
    Ok(())
}

async fn ctrl_c() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn serve(
    state: &StateDir,
    net: NetConfig,
    name: Option<String>,
    jobs: JobsConfig,
) -> Result<()> {
    let _lock = InstanceLock::acquire(state)?;
    let (agent, local) = start_local(state, net, name, jobs).await?;
    println!("{} is ready. Press Ctrl-C to stop.", agent.name());
    ctrl_c().await;
    println!("Stopping… (running jobs carry on)");
    agent.shutdown().await;
    local.abort();
    Ok(())
}

/// Pairs through the running agent, or starts one here if none is running.
async fn pair(
    state: &StateDir,
    net: NetConfig,
    name: Option<String>,
    jobs: JobsConfig,
    minutes: u64,
) -> Result<()> {
    let Some(_lock) = InstanceLock::try_acquire(state)? else {
        // An agent is running (or still starting): pair through it.
        return pair_with(connect_with_retry(state).await?, minutes).await;
    };
    let (agent, local) = start_local(state, net, name, jobs).await?;
    let client = connect_with_retry(state).await?;
    let result = pair_with(client, minutes).await;
    if result.is_ok() {
        println!("Still serving paired devices. Press Ctrl-C to stop.");
        ctrl_c().await;
        println!("Stopping… (running jobs carry on)");
    }
    agent.shutdown().await;
    local.abort();
    result
}

async fn connect_with_retry(state: &StateDir) -> Result<ipc::Client> {
    for _ in 0..50 {
        if let Some(client) = ipc::Client::connect(state).await? {
            return Ok(client);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("the agent's local control channel didn't start")
}

/// The terminal side of pairing: shows the ticket and asks before allowing a device.
async fn pair_with(mut client: ipc::Client, minutes: u64) -> Result<()> {
    use ipc::{AgentMsg, ClientMsg};
    client.send(&ClientMsg::Pair { minutes }).await?;
    loop {
        let msg = tokio::select! {
            msg = client.recv() => msg?,
            _ = ctrl_c() => {
                println!("\nPairing cancelled.");
                return Ok(());
            }
        };
        match msg {
            Some(AgentMsg::Ticket {
                ticket,
                machine_name,
                fingerprint,
                minutes,
            }) => {
                println!("This machine: {machine_name}  (fingerprint {fingerprint})");
                println!();
                println!("On your other device, run:");
                println!();
                println!("  yonder pair {ticket}");
                println!();
                println!("Pairing is open for {minutes} minutes. Waiting…");
            }
            Some(AgentMsg::Approve {
                controller_name,
                fingerprint,
            }) => {
                println!();
                println!("Pairing request from \"{controller_name}\"");
                println!("  Fingerprint: {fingerprint}");
                println!("  Check that your device shows the same fingerprint.");
                let allow = ask_yes_no("Allow? [y/N] ").await;
                client.send(&ClientMsg::Decision { allow }).await?;
            }
            Some(AgentMsg::Paired { controller_name }) => {
                println!("Paired with \"{controller_name}\".");
                return Ok(());
            }
            Some(AgentMsg::Declined { controller_name }) => {
                println!("Declined \"{controller_name}\". Run `yonder-agent pair` again to retry.");
                return Ok(());
            }
            Some(AgentMsg::Closed { reason }) => {
                println!("Pairing closed: {reason}. Run `yonder-agent pair` again to retry.");
                return Ok(());
            }
            Some(AgentMsg::Error { message }) => bail!(message),
            Some(AgentMsg::Status(_)) => {}
            None => bail!("the agent closed the connection"),
        }
    }
}

async fn ask_yes_no(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt}");
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

async fn status(state: &StateDir) -> Result<()> {
    #[cfg(windows)]
    match service::state() {
        Some(s) => println!("service:     {s}"),
        None => println!("service:     not installed"),
    }
    let running = InstanceLock::try_acquire(state)?.is_none();
    let client = if running {
        Some(connect_with_retry(state).await?)
    } else {
        None
    };
    match client {
        Some(mut client) => {
            let s = client.status().await?;
            println!("agent:       running");
            println!("name:        {}", s.name);
            println!("fingerprint: {}", s.fingerprint);
            println!(
                "paused:      {}",
                if s.paused {
                    "yes (new jobs refused)"
                } else {
                    "no"
                }
            );
            println!("devices:     {}", s.controllers);
            println!(
                "jobs:        {} running, {} queued",
                s.jobs_running, s.jobs_queued
            );
        }
        None => println!("agent:       not running"),
    }
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
