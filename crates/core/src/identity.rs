//! Device identity.
//!
//! Every device (an agent's machine or a controller like your Mac) has one ed25519 key.
//! Its public half is the device's id and also its iroh endpoint id, so a connection to a
//! device is end-to-end encrypted to that key and nobody else can answer for it.

use std::path::Path;

use iroh::{SecretKey, Signature};

use crate::error::{Error, Result};
use crate::store;

/// Public id of a device. Same as its iroh endpoint id.
pub type DeviceId = iroh::EndpointId;

/// The private key that identifies this device. Never leaves the device.
#[derive(Clone)]
pub struct DeviceKey(SecretKey);

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret.
        f.debug_tuple("DeviceKey")
            .field(&self.id().fmt_short().to_string())
            .finish()
    }
}

impl DeviceKey {
    /// Creates a new random key.
    pub fn generate() -> Self {
        DeviceKey(SecretKey::generate())
    }

    /// Loads the key stored at `path`, creating and saving a new one if the file doesn't exist.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(text.trim()).map_err(|message| Error::CorruptFile {
                path: path.to_owned(),
                message,
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let key = Self::generate();
                store::write_secret_file(path, Self::encode(&key).as_bytes())?;
                Ok(key)
            }
            Err(err) => Err(Error::io(path, err)),
        }
    }

    fn encode(&self) -> String {
        data_encoding::HEXLOWER.encode(&self.0.to_bytes())
    }

    fn parse(text: &str) -> std::result::Result<Self, String> {
        let bytes = data_encoding::HEXLOWER
            .decode(text.as_bytes())
            .map_err(|e| format!("not a hex key: {e}"))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "key must be 32 bytes".to_string())?;
        Ok(DeviceKey(SecretKey::from_bytes(&bytes)))
    }

    /// This device's public id.
    pub fn id(&self) -> DeviceId {
        self.0.public()
    }

    /// Signs `message` with this device's key.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.0.sign(message)
    }

    pub(crate) fn secret_key(&self) -> SecretKey {
        self.0.clone()
    }
}

/// Short code people compare by eye while pairing, e.g. `K7QM-2XRA-PB4D`.
///
/// 60 bits of the device id: enough that an attacker can't cheaply make a key whose
/// fingerprint matches someone else's.
pub fn fingerprint(id: &DeviceId) -> String {
    let encoded = data_encoding::BASE32_NOPAD.encode(&id.as_bytes()[..8]);
    let chars: Vec<char> = encoded.chars().take(12).collect();
    chars
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_created_once_and_reloaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        let first = DeviceKey::load_or_create(&path).unwrap();
        let second = DeviceKey::load_or_create(&path).unwrap();
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn damaged_key_file_is_reported_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        std::fs::write(&path, "not a key").unwrap();
        let err = DeviceKey::load_or_create(&path).unwrap_err();
        assert!(matches!(err, Error::CorruptFile { .. }), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not a key");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private_to_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        DeviceKey::load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn signatures_verify_with_the_device_id() {
        let key = DeviceKey::generate();
        let sig = key.sign(b"hello");
        assert!(key.id().verify(b"hello", &sig).is_ok());
        assert!(key.id().verify(b"hullo", &sig).is_err());
    }

    #[test]
    fn fingerprint_shape() {
        let fp = fingerprint(&DeviceKey::generate().id());
        assert_eq!(fp.len(), 14);
        assert_eq!(fp.matches('-').count(), 2);
    }

    #[test]
    fn debug_does_not_leak_the_secret() {
        let key = DeviceKey::generate();
        let secret_hex = key.encode();
        assert!(!format!("{key:?}").contains(&secret_hex));
    }
}
