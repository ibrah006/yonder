//! The limited Windows account that jobs and shells run as under the service.
//!
//! The service itself runs as LocalSystem, which can do anything on the machine. Work sent
//! from paired devices runs as `yonder-build` instead: an ordinary local user (not an
//! administrator) with its own profile, so tools, caches and sign-ins live in one place.
//! The account can't sign in at the screen, over Remote Desktop or over the network, and is
//! hidden from the sign-in screen; it only starts batch work. Its random password is kept
//! in the agent's state folder, which only SYSTEM and administrators can read, and is
//! replaced each time the service is installed.
//!
//! The service starts work through `yonder-agent build-user job|shell`, which signs in as
//! the account, loads its profile and starts the runner or shell with its environment.
//! That helper stays alive until the work ends and passes on its exit code; stopping the
//! helper stops the work.

#![cfg(windows)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::{null, null_mut};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, LUID, SetHandleInformation, WAIT_FAILED,
};
use windows_sys::Win32::NetworkManagement::NetManagement::{
    NERR_UserNotFound, NetApiBufferFree, NetUserAdd, NetUserGetInfo, NetUserSetInfo,
    UF_DONT_EXPIRE_PASSWD, UF_PASSWD_CANT_CHANGE, UF_SCRIPT, USER_INFO_1, USER_INFO_1003,
    USER_PRIV_USER,
};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_HANDLE, LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose,
    LsaNtStatusToWinError, LsaOpenPolicy, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LOGON32_LOGON_BATCH, LOGON32_PROVIDER_DEFAULT, LUID_AND_ATTRIBUTES,
    LogonUserW, LookupAccountNameW, LookupPrivilegeValueW, SE_ASSIGNPRIMARYTOKEN_NAME,
    SE_BACKUP_NAME, SE_INCREASE_QUOTA_NAME, SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SID_NAME_USE,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, PROCESS_INFORMATION,
    ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    GetUserProfileDirectoryW, LoadUserProfileW, PROFILEINFOW, UnloadUserProfile,
};
use yonder_core::jobs::{JobId, JobState, JobStore};

/// The account's name.
pub const USER: &str = "yonder-build";

/// What it may and may not do: start batch work, and nothing interactive.
const RIGHTS: [&str; 4] = [
    "SeBatchLogonRight",
    "SeDenyInteractiveLogonRight",
    "SeDenyRemoteInteractiveLogonRight",
    "SeDenyNetworkLogonRight",
];

/// `PI_NOUI`: never show profile errors in a dialog (nobody would see it).
const PI_NOUI: u32 = 1;

fn password_file(state: &Path) -> PathBuf {
    state.join("build-user.secret")
}

/// Whether `service install` has set up the account for this state folder.
pub fn is_set_up(state: &Path) -> bool {
    password_file(state).is_file()
}

/// `COMPUTER\yonder-build`, so a domain account with the same name is never picked.
fn qualified_name() -> String {
    match std::env::var("COMPUTERNAME") {
        Ok(computer) if !computer.is_empty() => format!("{computer}\\{USER}"),
        _ => USER.to_string(),
    }
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

/// A random password that meets Windows' complexity rules.
fn new_password() -> String {
    let bytes: [u8; 24] = rand::random();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("Yb!{hex}")
}

// ---------------------------------------------------------------------------
// Setting up the account (from `service install`, as an administrator)

/// Creates the account if needed, gives it a new password, limits how it can sign in and
/// hides it from the sign-in screen. Safe to run again.
pub fn set_up(state: &Path) -> Result<()> {
    let password = new_password();
    let created = create_or_reset(&password)?;
    yonder_core::store::write_secret_file(&password_file(state), password.as_bytes())
        .context("couldn't save the build user's password")?;
    grant_rights()?;
    // Synced projects live here and jobs run in them, so the account needs to write there.
    let workspaces = yonder_core::store::StateDir::new(state).workspaces_dir();
    std::fs::create_dir_all(&workspaces)
        .with_context(|| format!("couldn't create {}", workspaces.display()))?;
    grant_access(&workspaces)?;
    hide_from_sign_in();
    if created {
        println!("Created the {USER} account; jobs and shells run as this limited user.");
    }
    Ok(())
}

/// Returns whether the account was created (rather than already there).
fn create_or_reset(password: &str) -> Result<bool> {
    let name = wide(USER);
    let password = wide(password);
    // SAFETY: every pointer refers to a live, NUL-terminated buffer for the whole call;
    // the buffer NetUserGetInfo allocates is freed.
    unsafe {
        let mut existing: *mut u8 = null_mut();
        let found = NetUserGetInfo(null(), name.as_ptr(), 0, &mut existing);
        if !existing.is_null() {
            NetApiBufferFree(existing as *const _);
        }
        match found {
            0 => {
                let info = USER_INFO_1003 {
                    usri1003_password: password.as_ptr() as *mut u16,
                };
                let status = NetUserSetInfo(
                    null(),
                    name.as_ptr(),
                    1003,
                    &info as *const _ as *const u8,
                    null_mut(),
                );
                if status != 0 {
                    bail!("couldn't set the {USER} account's password (error {status})");
                }
                Ok(false)
            }
            code if code == NERR_UserNotFound => {
                let comment = wide("Runs work sent to this machine through Yonder.");
                let info = USER_INFO_1 {
                    usri1_name: name.as_ptr() as *mut u16,
                    usri1_password: password.as_ptr() as *mut u16,
                    usri1_priv: USER_PRIV_USER,
                    usri1_comment: comment.as_ptr() as *mut u16,
                    usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD | UF_PASSWD_CANT_CHANGE,
                    ..Default::default()
                };
                let status = NetUserAdd(null(), 1, &info as *const _ as *const u8, null_mut());
                if status != 0 {
                    bail!("couldn't create the {USER} account (error {status})");
                }
                Ok(true)
            }
            other => bail!("couldn't look up the {USER} account (error {other})"),
        }
    }
}

/// The account's security id.
fn account_sid() -> Result<Vec<u32>> {
    let name = wide(qualified_name());
    let (mut sid_len, mut domain_len) = (0u32, 0u32);
    let mut kind: SID_NAME_USE = 0;
    // SAFETY: the first call only asks for buffer sizes; the second gets buffers that big
    // (the SID buffer is u32s so it's suitably aligned).
    unsafe {
        LookupAccountNameW(
            null(),
            name.as_ptr(),
            null_mut(),
            &mut sid_len,
            null_mut(),
            &mut domain_len,
            &mut kind,
        );
        if sid_len == 0 {
            bail!("couldn't find the {USER} account");
        }
        let mut sid = vec![0u32; (sid_len as usize).div_ceil(4)];
        let mut domain = vec![0u16; domain_len as usize];
        let ok = LookupAccountNameW(
            null(),
            name.as_ptr(),
            sid.as_mut_ptr() as *mut _,
            &mut sid_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        );
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context(format!("couldn't look up the {USER} account"));
        }
        Ok(sid)
    }
}

fn grant_rights() -> Result<()> {
    let mut sid = account_sid()?;
    let names: Vec<Vec<u16>> = RIGHTS.iter().map(|r| r.encode_utf16().collect()).collect();
    let rights: Vec<LSA_UNICODE_STRING> = names
        .iter()
        .map(|n| LSA_UNICODE_STRING {
            Length: (n.len() * 2) as u16,
            MaximumLength: (n.len() * 2) as u16,
            Buffer: n.as_ptr() as *mut u16,
        })
        .collect();
    // SAFETY: zeroed object attributes are what LsaOpenPolicy expects; the SID and right
    // names outlive the calls; the policy handle is closed.
    unsafe {
        let attributes = LSA_OBJECT_ATTRIBUTES::default();
        let mut policy: LSA_HANDLE = 0;
        let status = LsaOpenPolicy(
            null(),
            &attributes,
            (POLICY_CREATE_ACCOUNT | POLICY_LOOKUP_NAMES) as u32,
            &mut policy,
        );
        if status != 0 {
            bail!(
                "couldn't open the local security policy (error {})",
                LsaNtStatusToWinError(status)
            );
        }
        let status = LsaAddAccountRights(
            policy,
            sid.as_mut_ptr() as *mut _,
            rights.as_ptr(),
            rights.len() as u32,
        );
        LsaClose(policy);
        if status != 0 {
            bail!(
                "couldn't set what the {USER} account may do (error {})",
                LsaNtStatusToWinError(status)
            );
        }
    }
    Ok(())
}

/// Keeps the account off the sign-in screen. Cosmetic, so failures are only reported.
fn hide_from_sign_in() {
    let status = Command::new("reg")
        .args([
            "add",
            r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList",
            "/v",
            USER,
            "/t",
            "REG_DWORD",
            "/d",
            "0",
            "/f",
        ])
        .stdout(Stdio::null())
        .status();
    if !matches!(status, Ok(s) if s.success()) {
        eprintln!("Note: couldn't hide {USER} from the sign-in screen.");
    }
}

// ---------------------------------------------------------------------------
// Starting work as the account (from the service, as LocalSystem)

/// What to start.
#[derive(Debug, Clone, Copy)]
pub enum Launch<'a> {
    /// A job runner for this job folder.
    Job(&'a Path),
    /// An interactive shell in this process's terminal.
    Shell,
}

/// A handle closed when dropped.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle this process opened and nothing else closes.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// The account's profile, loaded until dropped.
struct Profile<'a> {
    token: &'a Owned,
    handle: HANDLE,
}

impl Drop for Profile<'_> {
    fn drop(&mut self) {
        // SAFETY: the token and profile handle came from LoadUserProfileW.
        unsafe {
            UnloadUserProfile(self.token.0, self.handle);
        }
    }
}

/// The account's environment block (its PATH, TEMP, USERPROFILE…), freed when dropped.
struct Environment(*mut core::ffi::c_void);

impl Drop for Environment {
    fn drop(&mut self) {
        // SAFETY: allocated by CreateEnvironmentBlock.
        unsafe {
            DestroyEnvironmentBlock(self.0);
        }
    }
}

/// Starts `what` as the account, waits for it, and returns its exit code.
pub fn run(state: &Path, what: Launch<'_>) -> Result<u32> {
    let password = std::fs::read_to_string(password_file(state)).with_context(|| {
        format!("the {USER} account isn't set up; run `yonder-agent service install` again")
    })?;
    enable_privileges();
    let token = sign_in(password.trim())?;
    let _profile = load_profile(&token)?;
    let home = profile_dir(&token)?;
    let environment = environment(&token)?;

    let (command_line, flags, console) = match what {
        Launch::Job(dir) => {
            if !dir.is_dir() {
                bail!("no job folder at {}", dir.display());
            }
            grant_access(dir)?;
            // What to run and who asked: the job reads it, but must not change it (the
            // agent trusts it for ownership, borrowed logins and the job's folder).
            read_only(&dir.join("job.json"))?;
            let program = std::env::current_exe()?;
            let line = command_line(&[program.as_os_str(), "job-run".as_ref(), dir.as_os_str()]);
            (line, CREATE_NO_WINDOW, false)
        }
        Launch::Shell => {
            ignore_ctrl_c();
            let line = command_line(&["powershell.exe".as_ref(), "-NoLogo".as_ref()]);
            (line, 0, true)
        }
    };
    start_and_wait(&token, command_line, flags, console, &environment, &home)
}

/// Turns on privileges LocalSystem has but may not have enabled, which loading another
/// user's profile and starting a process as them need. Missing ones show up as errors from
/// the calls that need them.
fn enable_privileges() {
    // SAFETY: the token handle is checked and closed; the privilege struct is fully set.
    unsafe {
        let mut raw: HANDLE = null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut raw,
        ) == 0
        {
            return;
        }
        let token = Owned(raw);
        for name in [
            SE_ASSIGNPRIMARYTOKEN_NAME,
            SE_INCREASE_QUOTA_NAME,
            SE_BACKUP_NAME,
            SE_RESTORE_NAME,
        ] {
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(null(), name, &mut luid) == 0 {
                continue;
            }
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            AdjustTokenPrivileges(token.0, 0, &privileges, 0, null_mut(), null_mut());
        }
    }
}

fn sign_in(password: &str) -> Result<Owned> {
    let (user, domain, password) = (wide(USER), wide("."), wide(password));
    let mut token: HANDLE = null_mut();
    // SAFETY: NUL-terminated strings; the token is owned from here on.
    let ok = unsafe {
        LogonUserW(
            user.as_ptr(),
            domain.as_ptr(),
            password.as_ptr(),
            LOGON32_LOGON_BATCH,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context(format!(
            "couldn't sign in as {USER}; run `yonder-agent service install` again to reset it"
        ));
    }
    Ok(Owned(token))
}

/// Loads the profile (creating it the first time), so the registry hive, AppData and
/// per-user installs are there.
fn load_profile(token: &Owned) -> Result<Profile<'_>> {
    let mut user = wide(USER);
    let mut info = PROFILEINFOW {
        dwSize: size_of::<PROFILEINFOW>() as u32,
        dwFlags: PI_NOUI,
        lpUserName: user.as_mut_ptr(),
        ..Default::default()
    };
    // SAFETY: info is fully initialised and `user` outlives the call.
    if unsafe { LoadUserProfileW(token.0, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("couldn't load the {USER} profile"));
    }
    Ok(Profile {
        token,
        handle: info.hProfile,
    })
}

fn profile_dir(token: &Owned) -> Result<Vec<u16>> {
    let mut buffer = vec![0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: the buffer holds `len` characters.
    if unsafe { GetUserProfileDirectoryW(token.0, buffer.as_mut_ptr(), &mut len) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("couldn't find the {USER} profile folder"));
    }
    // `len` includes the terminating NUL.
    buffer.truncate(len as usize);
    Ok(buffer)
}

fn environment(token: &Owned) -> Result<Environment> {
    let mut block = null_mut();
    // SAFETY: CreateEnvironmentBlock allocates the block; Environment frees it.
    if unsafe { CreateEnvironmentBlock(&mut block, token.0, 0) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("couldn't read the {USER} environment"));
    }
    Ok(Environment(block))
}

/// Lets the account read and write this folder: a job's, or the workspaces (nothing else of
/// the agent's).
fn grant_access(dir: &Path) -> Result<()> {
    let status = Command::new("icacls")
        .arg(dir)
        .args([
            "/grant",
            &format!("{}:(OI)(CI)M", qualified_name()),
            "/T",
            "/Q",
        ])
        .stdout(Stdio::null())
        .status()
        .context("couldn't run icacls")?;
    if !status.success() {
        bail!("couldn't give {USER} access to {}", dir.display());
    }
    Ok(())
}

/// Lets the account only read this file: no inherited rights, so it can't change,
/// rename or delete it either. SYSTEM and Administrators keep full control.
fn read_only(file: &Path) -> Result<()> {
    let status = Command::new("icacls")
        .arg(file)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:F",
            "/grant:r",
            "*S-1-5-32-544:F",
            "/grant:r",
            &format!("{}:R", qualified_name()),
            "/Q",
        ])
        .stdout(Stdio::null())
        .status()
        .context("couldn't run icacls")?;
    if !status.success() {
        bail!("couldn't make {} read-only for {USER}", file.display());
    }
    Ok(())
}

/// Ctrl-C in the shell is for the shell: this helper shares its terminal and must not stop.
fn ignore_ctrl_c() {
    unsafe extern "system" fn ignore(_event: u32) -> windows_sys::core::BOOL {
        1
    }
    // SAFETY: registers a handler that lives for the whole process. A handler (unlike
    // passing none) isn't inherited, so the shell still gets Ctrl-C.
    unsafe {
        SetConsoleCtrlHandler(Some(ignore), 1);
    }
}

fn start_and_wait(
    token: &Owned,
    mut command_line: Vec<u16>,
    flags: u32,
    console: bool,
    environment: &Environment,
    home: &[u16],
) -> Result<u32> {
    // An empty desktop name gives the account its own window station; it can't use the
    // service's, and programs that load user32 (PowerShell does) fail to start without one.
    let mut desktop = wide("");
    let mut startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: desktop.as_mut_ptr(),
        ..Default::default()
    };
    if console {
        // The shell uses this helper's terminal: hand over its standard handles.
        // SAFETY: plain calls on this process's own standard handles.
        unsafe {
            startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
            startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
            startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
            for handle in [startup.hStdInput, startup.hStdOutput, startup.hStdError] {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            }
        }
        startup.dwFlags = STARTF_USESTDHANDLES;
    }

    let job = kill_on_close_job()?;
    let mut process = PROCESS_INFORMATION::default();
    // SAFETY: every buffer outlives the call; the command line is mutable as required.
    let ok = unsafe {
        CreateProcessAsUserW(
            token.0,
            null(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            i32::from(console),
            flags | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            environment.0,
            home.as_ptr(),
            &startup,
            &mut process,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("couldn't start a process as {USER}"));
    }
    let (child, thread) = (Owned(process.hProcess), Owned(process.hThread));
    // SAFETY: valid handles from CreateProcessAsUserW and kill_on_close_job.
    unsafe {
        if AssignProcessToJobObject(job.0, child.0) == 0 {
            let err = std::io::Error::last_os_error();
            TerminateProcess(child.0, 1);
            return Err(err).context("couldn't track the process");
        }
        ResumeThread(thread.0);
        if WaitForSingleObject(child.0, INFINITE) == WAIT_FAILED {
            return Err(std::io::Error::last_os_error()).context("couldn't wait for the process");
        }
        let mut code = 1u32;
        GetExitCodeProcess(child.0, &mut code);
        Ok(code)
    }
}

/// A Job Object that ends everything in it when this helper exits or is killed, so
/// stopping the helper (closing a shell, a runner kill) stops the work too.
fn kill_on_close_job() -> Result<Owned> {
    // SAFETY: standard Job Object setup; the handle is owned and checked.
    unsafe {
        let job = Owned(CreateJobObjectW(null(), null()));
        if job.0.is_null() {
            return Err(std::io::Error::last_os_error()).context("couldn't create a job object");
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("couldn't set up a job object");
        }
        Ok(job)
    }
}

/// Records why a job never started, so it shows as failed with a reason rather than lost.
pub fn record_failure(job_dir: &Path, reason: &str) {
    let id = job_dir
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(JobId::parse);
    let (Some(id), Some(parent)) = (id, job_dir.parent()) else {
        return;
    };
    let store = JobStore::new(parent);
    if let Ok(info) = store.info(&id)
        && !info.status.state.is_finished()
    {
        let message = format!("couldn't start as {USER}: {reason}");
        let _ = store.write_status(&id, &info.status.finish(JobState::Failed, Some(message)));
    }
}

/// Joins arguments into one command line, quoted the way Windows programs split it again.
pub(crate) fn command_line(args: &[&OsStr]) -> Vec<u16> {
    let mut line = Vec::new();
    for arg in args {
        if !line.is_empty() {
            line.push(u16::from(b' '));
        }
        push_quoted(&mut line, arg);
    }
    line.push(0);
    line
}

fn push_quoted(line: &mut Vec<u16>, arg: &OsStr) {
    let (quote, backslash) = (u16::from(b'"'), u16::from(b'\\'));
    let arg: Vec<u16> = arg.encode_wide().collect();
    let plain = !arg.is_empty()
        && !arg
            .iter()
            .any(|&c| c == quote || c == u16::from(b' ') || c == u16::from(b'\t'));
    if plain {
        line.extend(arg);
        return;
    }
    line.push(quote);
    let mut backslashes = 0;
    for &c in &arg {
        if c == backslash {
            backslashes += 1;
        } else {
            if c == quote {
                // Backslashes before a quote are doubled, plus one to escape the quote.
                line.extend(std::iter::repeat_n(backslash, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(c);
    }
    // Backslashes before the closing quote are doubled.
    line.extend(std::iter::repeat_n(backslash, backslashes));
    line.push(quote);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(args: &[&str]) -> String {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        let wide = command_line(&args);
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    }

    #[test]
    fn quoting_matches_windows_rules() {
        assert_eq!(line(&["a", "b"]), "a b");
        assert_eq!(
            line(&[r"C:\Program Files\Yonder\yonder-agent.exe", "job-run"]),
            r#""C:\Program Files\Yonder\yonder-agent.exe" job-run"#
        );
        assert_eq!(line(&[""]), r#""""#);
        assert_eq!(line(&[r#"say "hi""#]), r#""say \"hi\"""#);
        assert_eq!(line(&[r"dir with space\"]), r#""dir with space\\""#);
        assert_eq!(line(&[r"no\space\"]), r"no\space\");
    }

    #[test]
    fn passwords_meet_complexity_rules() {
        let p = new_password();
        assert!(p.len() >= 14);
        assert!(p.chars().any(|c| c.is_ascii_uppercase()));
        assert!(p.chars().any(|c| c.is_ascii_lowercase()));
        assert!(p.chars().any(|c| c.is_ascii_digit()));
        assert!(p.chars().any(|c| !c.is_ascii_alphanumeric()));
        assert_ne!(p, new_password());
    }
}
