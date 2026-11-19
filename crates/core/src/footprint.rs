//! Footprint: everything one controller has left on a machine, and removing it.
//!
//! A device sees and removes only its own footprint: its saved secrets, its project
//! copies (with their build output), its jobs and their logs, and its shells. Removing
//! everything, then unpairing, leaves nothing of that device behind. Another device with
//! the "manage machine" permission can do the same for a lost device (revoke with wipe).

use serde::{Deserialize, Serialize};

use crate::identity::DeviceId;
use crate::permissions::Permissions;
use crate::secrets::{SecretInfo, SecretName};
use crate::terminal::TerminalInfo;
use crate::workspace::{WorkspaceId, WorkspaceInfo};

/// One device's footprint on a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Footprint {
    /// Unix seconds.
    pub paired_at: u64,
    pub permissions: Permissions,
    pub secrets: Vec<SecretInfo>,
    pub workspaces: Vec<WorkspaceInfo>,
    /// Jobs kept on the machine, with their logs.
    pub jobs: usize,
    /// Of those, queued or running.
    pub unfinished_jobs: usize,
    pub job_bytes: u64,
    pub shells: Vec<TerminalInfo>,
}

impl Footprint {
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
            && self.workspaces.is_empty()
            && self.jobs == 0
            && self.shells.is_empty()
    }

    /// Bytes on disk: project copies and job logs (secrets and shells are small).
    pub fn bytes(&self) -> u64 {
        self.workspaces.iter().map(|w| w.bytes).sum::<u64>() + self.job_bytes
    }
}

/// What to remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FootprintItem {
    Secret(SecretName),
    Workspace(WorkspaceId),
    /// Every job: unfinished ones are cancelled, finished ones deleted with their logs.
    Jobs,
    /// Every shell this device started.
    Shells,
    Everything,
}

/// What a removal did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removed {
    pub secrets: usize,
    pub workspaces: usize,
    pub jobs: usize,
    pub shells: usize,
    pub bytes: u64,
    /// Things left in place, and why ("a job is still running in app-1234abcd").
    pub kept: Vec<String>,
}

impl Removed {
    pub fn add(&mut self, other: Removed) {
        self.secrets += other.secrets;
        self.workspaces += other.workspaces;
        self.jobs += other.jobs;
        self.shells += other.shells;
        self.bytes += other.bytes;
        self.kept.extend(other.kept);
    }
}

/// A device paired with a machine, as other devices see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub permissions: Permissions,
    /// Unix seconds.
    pub paired_at: u64,
    /// The device asking.
    pub you: bool,
}
