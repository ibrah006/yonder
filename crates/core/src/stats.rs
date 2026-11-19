//! How a machine is doing: CPU, memory, disk, uptime and whether it waits for a restart.
//! Sent with every `Info` answer, so lists of machines can show them.

use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineStats {
    /// All cores together, 0–100.
    pub cpu_percent: u8,
    pub memory_used: u64,
    pub memory_total: u64,
    /// The disk holding the agent's folder (where project copies and builds go).
    pub disk_free: u64,
    pub disk_total: u64,
    pub uptime_secs: u64,
    /// Windows and Linux say when updates wait for a restart; `None` when unknown.
    pub pending_restart: Option<bool>,
}

/// Keeps the previous CPU reading: usage is measured between two samples.
#[derive(Default)]
pub struct StatsSampler {
    system: Mutex<(Option<sysinfo::System>, Option<Instant>)>,
}

impl std::fmt::Debug for StatsSampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatsSampler")
    }
}

impl StatsSampler {
    /// Measures now. The first call waits a moment for a CPU reading; later ones use the
    /// time since the previous call. Blocking: run it off the async threads.
    pub fn sample(&self, data_dir: &Path) -> Option<MachineStats> {
        let mut guard = self.system.lock().unwrap_or_else(|p| p.into_inner());
        let (system, last) = &mut *guard;
        if system.is_none() {
            *system = sysinfo::System::new().ok();
        }
        let system = system.as_mut()?;
        if last.is_none_or(|t| t.elapsed() < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL) {
            system.refresh_cpu_usage();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        }
        system.refresh_cpu_usage();
        system.refresh_memory();
        *last = Some(Instant::now());
        let (disk_free, disk_total) = disk_space(data_dir);
        Some(MachineStats {
            cpu_percent: system.global_cpu_usage().round().clamp(0.0, 100.0) as u8,
            memory_used: system.used_memory(),
            memory_total: system.total_memory(),
            disk_free,
            disk_total,
            uptime_secs: sysinfo::System::uptime().unwrap_or(0),
            pending_restart: pending_restart(),
        })
    }
}

/// Free and total space of the disk that holds `dir`.
fn disk_space(dir: &Path) -> (u64, u64) {
    let Ok(disks) = sysinfo::Disks::new_with_refreshed_list() else {
        return (0, 0);
    };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    disks
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| (d.available_space(), d.total_space()))
        .unwrap_or((0, 0))
}

#[cfg(windows)]
fn pending_restart() -> Option<bool> {
    // Windows Update and component servicing leave these keys until the restart.
    let keys = [
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending",
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired",
    ];
    let mut any = false;
    for key in keys {
        let status = std::process::Command::new("reg")
            .args(["query", key])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        any |= status.success();
    }
    Some(any)
}

#[cfg(target_os = "linux")]
fn pending_restart() -> Option<bool> {
    // Debian and Ubuntu create this after updates that need a restart.
    Some(Path::new("/var/run/reboot-required").exists())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn pending_restart() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_machine_reports_sensible_numbers() {
        let sampler = StatsSampler::default();
        let dir = std::env::temp_dir();
        let stats = sampler.sample(&dir).unwrap();
        assert!(stats.cpu_percent <= 100, "{stats:?}");
        assert!(stats.memory_total > 0 && stats.memory_used <= stats.memory_total);
        assert!(
            stats.disk_total > 0 && stats.disk_free <= stats.disk_total,
            "{stats:?}"
        );
        assert!(stats.uptime_secs > 0);
        // A second sample doesn't need to wait again.
        let started = Instant::now();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sampler.sample(&dir);
        assert!(started.elapsed() < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL * 3);
    }
}
