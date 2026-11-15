//! Operating-system specifics for running jobs.
//!
//! | need                    | Windows                                  | Unix                          |
//! |-------------------------|------------------------------------------|-------------------------------|
//! | stop the whole tree     | runner joins a Job Object; terminate it  | command gets its own group; signal it |
//! | low priority            | below-normal priority class (inherited)  | nice 10 (inherited)           |
//! | keep the machine awake  | `SetThreadExecutionState` while running  | macOS: `caffeinate -w <pid>`  |
//! | is the runner alive?    | `OpenProcess` + exit code                | `kill(pid, 0)`                |

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

#[cfg(unix)]
mod unix {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    /// Whether a process with this id is running.
    pub fn process_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks the process exists and may be signalled.
        let result = unsafe { libc::kill(pid, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Lowers this process's priority; children inherit it.
    pub fn lower_own_priority() {
        // SAFETY: plain syscall on the current process.
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
        }
    }

    /// Makes the command start its own process group, so the whole tree can be stopped.
    pub fn prepare_command(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    /// Holds the tree-stopping handle. On Unix the process group is enough.
    #[derive(Debug, Default)]
    pub struct TreeGuard;

    impl TreeGuard {
        pub fn new() -> std::io::Result<Self> {
            Ok(TreeGuard)
        }

        /// Stops the command and everything it started: politely first, then forcefully.
        pub fn stop(&self, child: &mut Child) {
            let Ok(group) = libc::pid_t::try_from(child.id()) else {
                return;
            };
            // SAFETY: signalling our own child's process group.
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // SAFETY: as above. Also catches grandchildren still in the group.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }

    /// Stops a process immediately. Used when a runner doesn't respond to cancellation.
    pub fn kill_process(pid: u32) {
        if let Ok(pid) = libc::pid_t::try_from(pid) {
            // SAFETY: plain syscall.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    /// When this machine last started, in milliseconds since the Unix epoch.
    #[cfg(target_os = "macos")]
    pub fn boot_time_ms() -> Option<u64> {
        let mut tv: libc::timeval = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        // SAFETY: reads a fixed-size struct into a correctly sized buffer.
        let rc = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&mut tv as *mut libc::timeval).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && tv.tv_sec > 0).then(|| tv.tv_sec as u64 * 1000 + tv.tv_usec as u64 / 1000)
    }

    /// When this machine last started, in milliseconds since the Unix epoch.
    #[cfg(not(target_os = "macos"))]
    pub fn boot_time_ms() -> Option<u64> {
        let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
        let secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
        crate::protocol::now_ms().checked_sub((secs * 1000.0) as u64)
    }

    /// Keeps the machine from sleeping while the command runs.
    #[derive(Debug)]
    pub struct KeepAwake(Option<Child>);

    impl KeepAwake {
        pub fn while_running(pid: u32) -> Self {
            if cfg!(target_os = "macos") {
                let child = Command::new("/usr/bin/caffeinate")
                    .args(["-i", "-w", &pid.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .ok();
                KeepAwake(child)
            } else {
                KeepAwake(None)
            }
        }
    }

    impl Drop for KeepAwake {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, GetCurrentProcess, GetExitCodeProcess,
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, SetPriorityClass,
        TerminateProcess,
    };

    /// Whether a process with this id is running.
    pub fn process_alive(pid: u32) -> bool {
        // SAFETY: handle is checked and closed; exit code is a plain out-parameter.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code) != 0;
            CloseHandle(handle);
            ok && code == STILL_ACTIVE as u32
        }
    }

    /// Lowers this process's priority; children inherit below-normal priority.
    pub fn lower_own_priority() {
        // SAFETY: pseudo-handle for the current process.
        unsafe {
            SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
    }

    /// No console window for the command.
    pub fn prepare_command(command: &mut Command) {
        command.creation_flags(CREATE_NO_WINDOW);
    }

    /// The runner joins a Job Object before starting the command, so the command and
    /// everything it starts are in it too. Stopping terminates the whole job, runner
    /// included, so the runner records the result *before* calling [`TreeGuard::stop`].
    #[derive(Debug)]
    pub struct TreeGuard(HANDLE);

    // SAFETY: a job handle can be used from any thread.
    unsafe impl Send for TreeGuard {}
    unsafe impl Sync for TreeGuard {}

    impl TreeGuard {
        pub fn new() -> std::io::Result<Self> {
            // SAFETY: standard Job Object setup; every handle is checked.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                // If the runner dies, nothing it started is left behind.
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                    let err = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(err);
                }
                Ok(TreeGuard(job))
            }
        }

        /// Stops the command, everything it started, and this runner.
        pub fn stop(&self, _child: &mut Child) {
            // SAFETY: valid job handle owned by self.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for TreeGuard {
        fn drop(&mut self) {
            // SAFETY: valid handle; closing it kills anything left in the job.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Stops a process immediately. Used when a runner doesn't respond to cancellation;
    /// its Job Object then takes the command down with it.
    pub fn kill_process(pid: u32) {
        // SAFETY: handle is checked and closed.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }

    /// When this machine last started, in milliseconds since the Unix epoch.
    pub fn boot_time_ms() -> Option<u64> {
        // SAFETY: plain call.
        let uptime = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
        crate::protocol::now_ms().checked_sub(uptime)
    }

    /// Keeps the machine from sleeping while this (runner) process lives.
    #[derive(Debug)]
    pub struct KeepAwake;

    impl KeepAwake {
        pub fn while_running(_pid: u32) -> Self {
            // SAFETY: plain call; applies to this thread until reset or exit.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
            }
            KeepAwake
        }
    }

    impl Drop for KeepAwake {
        fn drop(&mut self) {
            // SAFETY: plain call.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn boot_time_is_in_the_past() {
        let boot = boot_time_ms().expect("boot time available");
        let now = crate::protocol::now_ms();
        assert!(boot < now);
        assert!(
            boot > now - 400 * 24 * 3600 * 1000,
            "booted within the last 400 days"
        );
    }

    #[test]
    fn finished_process_is_not_alive() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit"])
                .spawn()
        } else {
            std::process::Command::new("true").spawn()
        }
        .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert!(!process_alive(pid));
    }
}
