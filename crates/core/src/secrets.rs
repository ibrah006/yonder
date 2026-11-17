//! Saved secrets: values a controller keeps on a machine for its unattended jobs, such as
//! a Shorebird token or a signing key.
//!
//! - Each controller has its own secrets; no other device can use, list or remove them.
//! - They live in the agent's state folder (`<state>/secrets/<controller>/`), which only
//!   the service and administrators can read. They never go into job files, job lists or
//!   the audit log.
//! - A job asks for them by name ([`SecretUse`]). The agent hands their values to that
//!   job's runner only, as an environment variable or a file that exists while the job
//!   runs.
//! - A secret can expire; after that, jobs can't use it.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::DeviceId;
use crate::protocol::now_ms;
use crate::store::write_secret_file;
use crate::workspace::valid_path;

/// Largest secret value: room for keys and certificates, not for archives.
pub const MAX_SECRET_BYTES: usize = 1024 * 1024;

/// A secret's name: 1–64 letters, digits, `_`, `-` or `.`, not starting with `.`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SecretName(String);

impl SecretName {
    pub fn parse(text: &str) -> Option<Self> {
        let valid = (1..=64).contains(&text.len())
            && !text.starts_with('.')
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        valid.then(|| SecretName(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretName {
    type Error = String;
    fn try_from(value: String) -> std::result::Result<Self, String> {
        SecretName::parse(&value).ok_or_else(|| {
            format!("\"{value}\" isn't a secret name (letters, digits, _ - . up to 64)")
        })
    }
}

impl From<SecretName> for String {
    fn from(name: SecretName) -> String {
        name.0
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a job gets a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretTarget {
    /// An environment variable with this name.
    Env(String),
    /// A file at this path, relative to the job's working folder. It's written before the
    /// command starts and deleted when the job ends.
    File(String),
}

impl SecretTarget {
    /// Rejects names and paths that could reach outside what the job asked for.
    pub fn check(&self) -> std::result::Result<(), String> {
        match self {
            SecretTarget::Env(name) => {
                if name.is_empty() || name.contains(['=', '\0']) {
                    return Err(format!("\"{name}\" isn't an environment variable name"));
                }
            }
            SecretTarget::File(path) => {
                if !valid_path(path) {
                    return Err(format!(
                        "\"{path}\" must be a plain path inside the job's folder"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A job's request for one of its controller's secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretUse {
    pub name: SecretName,
    pub target: SecretTarget,
}

impl fmt::Display for SecretUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.target {
            SecretTarget::Env(var) => write!(f, "{} as ${var}", self.name),
            SecretTarget::File(path) => write!(f, "{} as file {path}", self.name),
        }
    }
}

/// What anyone may know about a secret: everything except its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretInfo {
    pub name: SecretName,
    pub size: u64,
    pub created_at_ms: u64,
    pub expires_at_ms: Option<u64>,
    pub last_used_ms: Option<u64>,
    /// What it is or where it came from, e.g. "Shorebird CI token".
    pub note: Option<String>,
}

impl SecretInfo {
    pub fn expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|at| at <= now_ms)
    }
}

/// One secret on disk.
#[derive(Serialize, Deserialize)]
struct Stored {
    info: SecretInfo,
    value: Vec<u8>,
}

/// A secret's value on its way to a job.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub target: SecretTarget,
    pub value: Vec<u8>,
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the value.
        f.debug_struct("Resolved")
            .field("target", &self.target)
            .field("bytes", &self.value.len())
            .finish()
    }
}

/// Why a job can't have the secrets it asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseError {
    Missing(SecretName),
    Expired(SecretName),
    BadTarget(String),
}

impl fmt::Display for UseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UseError::Missing(name) => write!(
                f,
                "no secret {name} from this device here; save it with `yonder secret set`"
            ),
            UseError::Expired(name) => {
                write!(
                    f,
                    "secret {name} has expired; save a new one with `yonder secret set`"
                )
            }
            UseError::BadTarget(message) => f.write_str(message),
        }
    }
}

/// The secrets folder.
#[derive(Debug, Clone)]
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        SecretStore { dir: dir.into() }
    }

    fn controller_dir(&self, controller: &DeviceId) -> PathBuf {
        self.dir.join(controller.to_string())
    }

    fn path(&self, controller: &DeviceId, name: &SecretName) -> PathBuf {
        self.controller_dir(controller)
            .join(format!("{}.secret", name.as_str()))
    }

    /// Saves (or replaces) a secret.
    pub fn set(
        &self,
        controller: &DeviceId,
        name: SecretName,
        value: Vec<u8>,
        expires_at_ms: Option<u64>,
        note: Option<String>,
    ) -> Result<SecretInfo> {
        if value.len() > MAX_SECRET_BYTES {
            return Err(Error::Protocol(format!(
                "secret {name} is {} bytes; the most is {MAX_SECRET_BYTES}",
                value.len()
            )));
        }
        let info = SecretInfo {
            size: value.len() as u64,
            created_at_ms: now_ms(),
            expires_at_ms,
            last_used_ms: None,
            note: note.filter(|n| !n.trim().is_empty()),
            name,
        };
        self.write(controller, &Stored { info, value })
    }

    fn write(&self, controller: &DeviceId, stored: &Stored) -> Result<SecretInfo> {
        let path = self.path(controller, &stored.info.name);
        let bytes = postcard::to_stdvec(stored).expect("secrets are always serializable");
        write_secret_file(&path, &bytes)?;
        Ok(stored.info.clone())
    }

    fn read(&self, controller: &DeviceId, name: &SecretName) -> Result<Option<Stored>> {
        read_stored(&self.path(controller, name))
    }

    /// The controller's secrets, by name, without values.
    pub fn list(&self, controller: &DeviceId) -> Result<Vec<SecretInfo>> {
        let dir = self.controller_dir(controller);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(&dir, e)),
        };
        let mut infos = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "secret")
                && let Some(stored) = read_stored(&path)?
            {
                infos.push(stored.info);
            }
        }
        infos.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(infos)
    }

    /// Deletes one secret; false if there was none.
    pub fn remove(&self, controller: &DeviceId, name: &SecretName) -> Result<bool> {
        let path = self.path(controller, name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Deletes all of a controller's secrets and returns how many there were.
    pub fn remove_all(&self, controller: &DeviceId) -> Result<usize> {
        let count = self.list(controller)?.len();
        let dir = self.controller_dir(controller);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(count),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(Error::io(&dir, e)),
        }
    }

    /// The values a job asked for, all or nothing, and marks them used.
    pub fn resolve(
        &self,
        controller: &DeviceId,
        uses: &[SecretUse],
    ) -> Result<std::result::Result<Vec<Resolved>, UseError>> {
        let now = now_ms();
        let mut resolved = Vec::with_capacity(uses.len());
        let mut used = Vec::new();
        for secret in uses {
            if let Err(message) = secret.target.check() {
                return Ok(Err(UseError::BadTarget(message)));
            }
            let Some(stored) = self.read(controller, &secret.name)? else {
                return Ok(Err(UseError::Missing(secret.name.clone())));
            };
            if stored.info.expired(now) {
                return Ok(Err(UseError::Expired(secret.name.clone())));
            }
            resolved.push(Resolved {
                target: secret.target.clone(),
                value: stored.value.clone(),
            });
            used.push(stored);
        }
        for mut stored in used {
            stored.info.last_used_ms = Some(now);
            // Bookkeeping only: a failure here mustn't stop the job.
            let _ = self.write(controller, &stored);
        }
        Ok(Ok(resolved))
    }
}

fn read_stored(path: &Path) -> Result<Option<Stored>> {
    match std::fs::read(path) {
        Ok(bytes) => postcard::from_bytes(&bytes)
            .map(Some)
            .map_err(|e| Error::CorruptFile {
                path: path.to_owned(),
                message: e.to_string(),
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKey;

    fn name(text: &str) -> SecretName {
        SecretName::parse(text).unwrap()
    }

    fn env(secret: &str, var: &str) -> SecretUse {
        SecretUse {
            name: name(secret),
            target: SecretTarget::Env(var.into()),
        }
    }

    #[test]
    fn names_are_strict() {
        for good in ["SHOREBIRD_TOKEN", "dsa_priv.pem", "a", "key-2"] {
            assert!(SecretName::parse(good).is_some(), "{good}");
        }
        for bad in ["", ".hidden", "../x", "a/b", "a b", "a\\b", &"x".repeat(65)] {
            assert!(SecretName::parse(bad).is_none(), "{bad}");
        }
        assert!(serde_json::from_str::<SecretName>("\"../x\"").is_err());
    }

    #[test]
    fn targets_stay_in_the_job() {
        assert!(SecretTarget::File("keys/a.pem".into()).check().is_ok());
        assert!(SecretTarget::File("../a.pem".into()).check().is_err());
        assert!(SecretTarget::File("C:/a.pem".into()).check().is_err());
        assert!(SecretTarget::Env("TOKEN".into()).check().is_ok());
        assert!(SecretTarget::Env("A=B".into()).check().is_err());
        assert!(SecretTarget::Env(String::new()).check().is_err());
    }

    #[test]
    fn each_controller_sees_only_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path());
        let (mac, laptop) = (DeviceKey::generate().id(), DeviceKey::generate().id());
        store
            .set(
                &mac,
                name("TOKEN"),
                b"s3cret".to_vec(),
                None,
                Some("CI".into()),
            )
            .unwrap();

        let listed = store.list(&mac).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].size, 6);
        assert_eq!(listed[0].note.as_deref(), Some("CI"));
        assert!(store.list(&laptop).unwrap().is_empty());
        assert_eq!(
            store.resolve(&laptop, &[env("TOKEN", "T")]).unwrap(),
            Err(UseError::Missing(name("TOKEN")))
        );
        assert!(!store.remove(&laptop, &name("TOKEN")).unwrap());

        let got = store.resolve(&mac, &[env("TOKEN", "T")]).unwrap().unwrap();
        assert_eq!(got[0].value, b"s3cret");
        assert!(store.list(&mac).unwrap()[0].last_used_ms.is_some());
        assert!(!format!("{got:?}").contains("s3cret"));

        assert!(store.remove(&mac, &name("TOKEN")).unwrap());
        assert!(store.list(&mac).unwrap().is_empty());
    }

    #[test]
    fn expired_secrets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path());
        let mac = DeviceKey::generate().id();
        store
            .set(&mac, name("OLD"), b"x".to_vec(), Some(now_ms() - 1), None)
            .unwrap();
        store
            .set(&mac, name("NEW"), b"y".to_vec(), None, None)
            .unwrap();
        // All or nothing: the good one isn't handed out either.
        assert_eq!(
            store
                .resolve(&mac, &[env("NEW", "A"), env("OLD", "B")])
                .unwrap(),
            Err(UseError::Expired(name("OLD")))
        );
        assert!(
            store
                .list(&mac)
                .unwrap()
                .iter()
                .all(|s| s.last_used_ms.is_none())
        );
    }

    #[test]
    fn remove_all_clears_a_controller() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path());
        let mac = DeviceKey::generate().id();
        store
            .set(&mac, name("A"), b"1".to_vec(), None, None)
            .unwrap();
        store
            .set(&mac, name("B"), b"2".to_vec(), None, None)
            .unwrap();
        assert_eq!(store.remove_all(&mac).unwrap(), 2);
        assert!(store.list(&mac).unwrap().is_empty());
        assert_eq!(store.remove_all(&mac).unwrap(), 0);
    }

    #[test]
    fn oversized_values_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::new(dir.path());
        let mac = DeviceKey::generate().id();
        let big = vec![0; MAX_SECRET_BYTES + 1];
        assert!(store.set(&mac, name("BIG"), big, None, None).is_err());
    }
}
