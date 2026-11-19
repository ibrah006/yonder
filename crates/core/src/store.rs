//! On-disk state for agents and controllers.
//!
//! Small JSON files, written atomically (temp file + rename) so a crash or power cut never
//! leaves a half-written file. Everything is written readable only by the owner.
//!
//! On Windows the agent's state directory lives under `%ProgramData%`; restricting it to
//! the service account with ACLs is part of Step 2.

use std::path::{Path, PathBuf};

use iroh::EndpointAddr;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::Os;
use crate::error::{Error, Result};
use crate::identity::DeviceId;
use crate::permissions::Permissions;

/// Writes `contents` to `path` atomically, readable only by the owner.
pub fn write_secret_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent)?;
    }
    let tmp = path.with_extension("tmp");
    write_private(&tmp, contents).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::io(dir, e))?;
    }
    Ok(())
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| Error::CorruptFile {
            path: path.to_owned(),
            message: e.to_string(),
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(Error::io(path, err)),
    }
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).expect("state is always serializable");
    write_secret_file(path, &bytes)
}

/// Seconds since the Unix epoch.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Formats Unix seconds as a UTC date, e.g. `2026-11-16`.
pub fn format_date(unix_secs: u64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Formats Unix milliseconds as UTC date and time, e.g. `2026-11-16 14:03`.
pub fn format_datetime_ms(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let minute_of_day = (secs % 86_400) / 60;
    format!(
        "{} {:02}:{:02}",
        format_date(secs),
        minute_of_day / 60,
        minute_of_day % 60
    )
}

/// The name of this computer, used as the default device name.
pub fn default_device_name() -> String {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    let name = name.trim_end_matches(".local").trim();
    if name.is_empty() {
        "Unnamed device".to_string()
    } else {
        name.to_string()
    }
}

/// Where Yonder keeps a machine's agent state by default.
///
/// Windows: `%ProgramData%\Yonder\agent`, so the service finds it whoever is signed in.
/// Elsewhere (until the macOS/Linux agents are services): the user's data folder.
pub fn default_agent_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(program_data) = std::env::var_os("ProgramData") {
            return Some(PathBuf::from(program_data).join("Yonder").join("agent"));
        }
    }
    user_data_dir().map(|d| d.join("agent"))
}

/// Where Yonder keeps this device's controller state by default (the user's data folder).
/// Shared by the CLI and the app, so both act as the same device.
pub fn default_controller_dir() -> Option<PathBuf> {
    user_data_dir().map(|d| d.join("controller"))
}

fn user_data_dir() -> Option<PathBuf> {
    let base = directories::BaseDirs::new()?;
    let folder = if cfg!(any(windows, target_os = "macos")) {
        "Yonder"
    } else {
        "yonder"
    };
    Some(base.data_dir().join(folder))
}

/// A directory holding one device's key and state.
#[derive(Debug, Clone)]
pub struct StateDir(PathBuf);

impl StateDir {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        StateDir(path.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn key_file(&self) -> PathBuf {
        self.0.join("device.key")
    }

    pub fn agent_file(&self) -> PathBuf {
        self.0.join("agent.json")
    }

    pub fn controller_file(&self) -> PathBuf {
        self.0.join("controller.json")
    }

    pub fn jobs_dir(&self) -> PathBuf {
        self.0.join("jobs")
    }

    pub fn audit_file(&self) -> PathBuf {
        self.0.join("audit.log")
    }

    /// Synced project copies, where jobs run.
    pub fn workspaces_dir(&self) -> PathBuf {
        self.0.join("workspaces")
    }

    /// Bookkeeping for workspace syncs.
    pub fn sync_dir(&self) -> PathBuf {
        self.0.join("sync")
    }

    /// Secrets controllers saved for their jobs.
    pub fn secrets_dir(&self) -> PathBuf {
        self.0.join("secrets")
    }
}

/// A controller that was approved on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedController {
    pub id: DeviceId,
    pub name: String,
    pub permissions: Permissions,
    /// Unix seconds.
    pub paired_at: u64,
    /// Remove this device's project copies unused for this many days. Off when `None`.
    #[serde(default)]
    pub cleanup_after_days: Option<u32>,
}

/// Agent state: which controllers may connect, and what they may do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    /// Name shown to controllers. Empty means "use the computer's name".
    #[serde(default)]
    pub machine_name: String,
    #[serde(default)]
    pub controllers: Vec<PairedController>,
    /// New jobs are refused while paused; running jobs carry on.
    #[serde(default)]
    pub paused: bool,
}

impl AgentState {
    pub fn load(path: &Path) -> Result<Self> {
        load_json(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_json(path, self)
    }

    pub fn controller(&self, id: &DeviceId) -> Option<&PairedController> {
        self.controllers.iter().find(|c| &c.id == id)
    }

    /// Adds a controller, or replaces it if it was already paired.
    pub fn upsert_controller(&mut self, controller: PairedController) {
        self.controllers.retain(|c| c.id != controller.id);
        self.controllers.push(controller);
    }

    /// Removes a controller. Returns whether it was paired.
    pub fn remove_controller(&mut self, id: &DeviceId) -> bool {
        let before = self.controllers.len();
        self.controllers.retain(|c| &c.id != id);
        self.controllers.len() != before
    }
}

/// A machine this controller has paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedMachine {
    pub id: DeviceId,
    pub name: String,
    pub os: Os,
    /// Last known addresses. Address lookup fills in changes; this helps on a LAN
    /// without internet.
    pub addr: EndpointAddr,
    pub permissions: Permissions,
    /// Unix seconds.
    pub paired_at: u64,
}

/// Controller state: the machines this device can use.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ControllerState {
    /// Name shown to machines when pairing. Empty means "use the computer's name".
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub machines: Vec<PairedMachine>,
}

impl ControllerState {
    pub fn load(path: &Path) -> Result<Self> {
        load_json(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        save_json(path, self)
    }

    pub fn upsert_machine(&mut self, machine: PairedMachine) {
        self.machines.retain(|m| m.id != machine.id);
        self.machines.push(machine);
    }

    pub fn remove_machine(&mut self, id: &DeviceId) -> bool {
        let before = self.machines.len();
        self.machines.retain(|m| &m.id != id);
        self.machines.len() != before
    }

    /// Finds a machine by exact name (ignoring case), or by a prefix of its id.
    pub fn find_machine(&self, query: &str) -> Result<&PairedMachine> {
        let query_lower = query.trim().to_lowercase();
        if query_lower.is_empty() {
            return Err(Error::UnknownMachine(query.to_string()));
        }
        let by_name: Vec<_> = self
            .machines
            .iter()
            .filter(|m| m.name.to_lowercase() == query_lower)
            .collect();
        let matches = if by_name.is_empty() {
            self.machines
                .iter()
                .filter(|m| m.id.to_string().starts_with(&query_lower))
                .collect()
        } else {
            by_name
        };
        match matches.as_slice() {
            [one] => Ok(one),
            [] => Err(Error::UnknownMachine(query.to_string())),
            many => Err(Error::AmbiguousMachine {
                query: query.to_string(),
                matches: many
                    .iter()
                    .map(|m| format!("{} ({})", m.name, m.id.fmt_short()))
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKey;

    fn machine(name: &str) -> PairedMachine {
        let id = DeviceKey::generate().id();
        PairedMachine {
            id,
            name: name.to_string(),
            os: Os::Windows,
            addr: EndpointAddr::new(id),
            permissions: Permissions::for_new_controller(),
            paired_at: 0,
        }
    }

    #[test]
    fn dates_format_in_utc() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(1_794_787_200), "2026-11-16");
        assert_eq!(
            format_datetime_ms(1_794_787_200_000 + 14 * 3_600_000 + 3 * 60_000),
            "2026-11-16 14:03"
        );
    }

    #[test]
    fn missing_state_file_means_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let state = AgentState::load(&dir.path().join("agent.json")).unwrap();
        assert!(state.controllers.is_empty());
    }

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controller.json");
        let mut state = ControllerState::default();
        state.upsert_machine(machine("build-box"));
        state.save(&path).unwrap();
        let loaded = ControllerState::load(&path).unwrap();
        assert_eq!(loaded.machines, state.machines);
    }

    #[test]
    fn damaged_state_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(
            AgentState::load(&path),
            Err(Error::CorruptFile { .. })
        ));
    }

    #[test]
    fn find_machine_by_name_or_id_prefix() {
        let mut state = ControllerState::default();
        let a = machine("Studio-PC");
        let b = machine("laptop");
        state.upsert_machine(a.clone());
        state.upsert_machine(b.clone());

        assert_eq!(state.find_machine("studio-pc").unwrap().id, a.id);
        let prefix = &b.id.to_string()[..10];
        assert_eq!(state.find_machine(prefix).unwrap().id, b.id);
        assert!(matches!(
            state.find_machine("nope"),
            Err(Error::UnknownMachine(_))
        ));
    }

    #[test]
    fn duplicate_names_are_ambiguous() {
        let mut state = ControllerState::default();
        state.upsert_machine(machine("pc"));
        state.upsert_machine(machine("PC"));
        assert!(matches!(
            state.find_machine("pc"),
            Err(Error::AmbiguousMachine { .. })
        ));
    }

    #[test]
    fn upsert_replaces_existing_controller() {
        let id = DeviceKey::generate().id();
        let mut state = AgentState::default();
        for name in ["old", "new"] {
            state.upsert_controller(PairedController {
                id,
                name: name.into(),
                permissions: Permissions::default(),
                paired_at: 0,
                cleanup_after_days: None,
            });
        }
        assert_eq!(state.controllers.len(), 1);
        assert_eq!(state.controller(&id).unwrap().name, "new");
    }
}
