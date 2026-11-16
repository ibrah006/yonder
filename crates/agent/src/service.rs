//! Running the agent as a Windows service: starts at boot, works while nobody is signed in.
//!
//! `yonder-agent service install` (from an administrator terminal):
//! 1. copies this program to `%ProgramFiles%\Yonder\yonder-agent.exe`;
//! 2. locks the state folder (`%ProgramData%\Yonder\agent`) to SYSTEM and Administrators;
//! 3. sets up the limited `yonder-build` account that jobs and shells run as;
//! 4. registers and starts the `YonderAgent` service (runs as LocalSystem, restarts on failure).
//!
//! Running it again updates the installed program in place.

#![cfg(windows)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "YonderAgent";
const DISPLAY_NAME: &str = "Yonder Agent";
const DESCRIPTION: &str = "Runs jobs for your paired Yonder devices. See `yonder-agent controllers` and `yonder-agent audit`.";

/// Where `install` puts the program.
fn install_path() -> Result<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles").context("ProgramFiles isn't set")?;
    Ok(PathBuf::from(program_files)
        .join("Yonder")
        .join("yonder-agent.exe"))
}

/// Restricts a folder to SYSTEM and Administrators, removing inherited access.
pub fn secure_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let status = std::process::Command::new("icacls")
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
            "/Q",
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .context("couldn't run icacls")?;
    if !status.success() {
        bail!("couldn't restrict access to {}", dir.display());
    }
    Ok(())
}

/// Installs or updates the service. `agent_args` are passed before `service run`. With
/// `build_user`, jobs and shells run as the limited build user instead of LocalSystem.
pub fn install(state_dir: &Path, agent_args: Vec<OsString>, build_user: bool) -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("couldn't open the service manager; run this from an administrator terminal")?;

    let target = install_path()?;
    let access = ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG;
    let existing = manager.open_service(SERVICE_NAME, access).ok();
    if let Some(service) = &existing {
        stop_and_wait(service)?;
    }

    let current = std::env::current_exe()?;
    if !same_file(&current, &target) {
        std::fs::create_dir_all(target.parent().expect("install path has a parent"))?;
        std::fs::copy(&current, &target)
            .with_context(|| format!("couldn't copy the agent to {}", target.display()))?;
    }
    secure_dir(state_dir)?;
    if build_user {
        crate::build_user::set_up(state_dir)?;
    }

    let mut launch_arguments = agent_args;
    launch_arguments.extend(["service".into(), "run".into()]);
    let info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: DISPLAY_NAME.into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target.clone(),
        launch_arguments,
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match existing {
        Some(service) => {
            service.change_config(&info)?;
            service
        }
        None => manager.create_service(&info, access)?,
    };
    service.set_description(DESCRIPTION)?;
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            restart_after(Duration::from_secs(5)),
            restart_after(Duration::from_secs(30)),
            restart_after(Duration::from_secs(120)),
        ]),
    })?;
    service.start(&[] as &[&OsStr])?;
    println!(
        "Installed {} and started the {SERVICE_NAME} service.",
        target.display()
    );
    println!("It starts with Windows and runs while nobody is signed in.");
    println!();
    if build_user {
        println!(
            "Jobs and shells run as {}, a standard user without administrator rights.",
            crate::build_user::USER
        );
        println!("Install tools for all users (or as that user) so jobs can find them.");
    } else {
        println!("Note: jobs and shells run as LocalSystem (--run-as-system), which has full");
        println!("administrator rights on this machine. Only pair devices you control.");
    }
    Ok(())
}

fn restart_after(delay: Duration) -> ServiceAction {
    ServiceAction {
        action_type: ServiceActionType::Restart,
        delay,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn stop_and_wait(service: &windows_service::service::Service) -> Result<()> {
    if service.query_status()?.current_state == ServiceState::Stopped {
        return Ok(());
    }
    let _ = service.stop();
    let deadline = Instant::now() + Duration::from_secs(30);
    while service.query_status()?.current_state != ServiceState::Stopped {
        if Instant::now() > deadline {
            bail!("the service didn't stop within 30 seconds");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// Stops and removes the service. Keeps the state folder (paired devices, job logs).
pub fn uninstall() -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("couldn't open the service manager; run this from an administrator terminal")?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )
        .context("the service isn't installed")?;
    stop_and_wait(&service)?;
    service.delete()?;
    println!("Removed the {SERVICE_NAME} service. Paired devices and job history were kept.");
    println!(
        "The {} account and its files were kept; remove it with `net user {} /delete`.",
        crate::build_user::USER,
        crate::build_user::USER
    );
    Ok(())
}

/// Starts or stops the installed service.
pub fn control(start: bool) -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("couldn't open the service manager; run this from an administrator terminal")?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP,
        )
        .context("the service isn't installed; run `yonder-agent service install`")?;
    if start {
        service.start(&[] as &[&OsStr])?;
        println!("Started {SERVICE_NAME}.");
    } else {
        stop_and_wait(&service)?;
        println!("Stopped {SERVICE_NAME}.");
    }
    Ok(())
}

/// Describes the service's state, or `None` if it isn't installed.
pub fn state() -> Option<String> {
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT).ok()?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        .ok()?;
    Some(format!("{:?}", service.query_status().ok()?.current_state).to_lowercase())
}

// ---------------------------------------------------------------------------
// Running under the service manager

type Body = Box<dyn FnOnce(tokio::sync::oneshot::Receiver<()>) -> Result<()> + Send>;

static BODY: std::sync::Mutex<Option<Body>> = std::sync::Mutex::new(None);

/// Hands control to the service manager, which calls `body` with a receiver that fires
/// when Windows asks the service to stop.
pub fn run(body: Body) -> Result<()> {
    *BODY.lock().unwrap_or_else(|p| p.into_inner()) = Some(body);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(
        "not started by the service manager (use `yonder-agent serve` to run in a terminal)",
    )
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        tracing::error!("service failed: {err:#}");
    }
}

fn run_service() -> Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let stop_tx = std::sync::Mutex::new(Some(stop_tx));
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(tx) = stop_tx.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = tx.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let set = |state: ServiceState, accept: ServiceControlAccept, code: u32| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        })
    };
    set(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
    )?;

    let body = BODY
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .context("service body missing")?;
    let result = body(stop_rx);

    set(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        u32::from(result.is_err()),
    )?;
    result
}
