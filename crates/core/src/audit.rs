//! The machine's local audit log: one JSON line per request a controller made.
//!
//! Accepted requests keep the signed request bytes and signature, so anyone can later check
//! that the named device really asked for it. Environment values are never written.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::DeviceId;
use crate::protocol::now_ms;

/// The log moves to `audit.log.1` when it grows past this.
const ROTATE_AT_BYTES: u64 = 10 * 1024 * 1024;

/// One request, as recorded on the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: u64,
    pub controller: DeviceId,
    /// Present when the device was paired at the time.
    pub controller_name: Option<String>,
    /// Command name (`run_job`, `cancel_job`, …), or `?` if the request couldn't be read.
    pub command: String,
    /// The command line for new jobs; the job id for other job commands.
    pub detail: Option<String>,
    /// `ok`, or why it was refused (`PermissionDenied`, `Paused`, …).
    pub outcome: String,
    /// Hex of the signed request body, for accepted signatures.
    pub request: Option<String>,
    /// Hex of the signature over `request`.
    pub signature: Option<String>,
}

impl AuditEntry {
    pub fn new(
        controller: DeviceId,
        command: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        AuditEntry {
            at_ms: now_ms(),
            controller,
            controller_name: None,
            command: command.into(),
            detail: None,
            outcome: outcome.into(),
            request: None,
            signature: None,
        }
    }
}

/// Append-only log file.
#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        AuditLog {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends an entry.
    pub fn record(&self, entry: &AuditEntry) -> Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > ROTATE_AT_BYTES) {
            let old = self.path.with_extension("log.1");
            std::fs::rename(&self.path, &old).map_err(|e| Error::io(&old, e))?;
        }
        let mut line = serde_json::to_vec(entry).expect("audit entries are serializable");
        line.push(b'\n');
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&self.path)
            .map_err(|e| Error::io(&self.path, e))?;
        file.write_all(&line).map_err(|e| Error::io(&self.path, e))
    }

    /// The last `count` entries, oldest first. Lines that can't be read are skipped.
    pub fn recent(&self, count: usize) -> Result<Vec<AuditEntry>> {
        read_recent(&self.path, count)
    }
}

/// Reads the last `count` entries of an audit log file, oldest first.
pub fn read_recent(path: &Path, count: usize) -> Result<Vec<AuditEntry>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(path, err)),
    };
    let mut entries: Vec<AuditEntry> = std::io::BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();
    let skip = entries.len().saturating_sub(count);
    Ok(entries.split_off(skip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKey;

    #[test]
    fn records_and_reads_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(dir.path().join("audit.log"));
        let id = DeviceKey::generate().id();
        for command in ["ping", "run_job", "cancel_job"] {
            log.record(&AuditEntry::new(id, command, "ok")).unwrap();
        }
        let recent = log.recent(2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].command, "run_job");
        assert_eq!(recent[1].command, "cancel_job");
    }

    #[test]
    fn missing_log_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_recent(&dir.path().join("nope.log"), 10)
                .unwrap()
                .is_empty()
        );
    }
}
