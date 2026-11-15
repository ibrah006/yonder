//! Pairing tickets.
//!
//! When pairing is opened on a machine, it shows a ticket: the machine's id and addresses
//! plus a one-time secret. The controller uses the ticket to reach the machine and prove it
//! was given the ticket. The machine owner still approves the pairing on the machine.

use iroh::EndpointAddr;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::DeviceId;

const PREFIX: &str = "yonder-pair-";
const TICKET_VERSION: u8 = 1;

/// Everything a controller needs to pair with a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTicket {
    pub machine: EndpointAddr,
    pub secret: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Wire {
    version: u8,
    machine: EndpointAddr,
    secret: [u8; 32],
}

impl PairingTicket {
    /// Encodes the ticket as text safe to paste anywhere (lowercase letters, digits, dashes).
    pub fn encode(&self) -> String {
        let wire = Wire {
            version: TICKET_VERSION,
            machine: self.machine.clone(),
            secret: self.secret,
        };
        let bytes = postcard::to_stdvec(&wire).expect("ticket is always serializable");
        format!(
            "{PREFIX}{}",
            data_encoding::BASE32_NOPAD.encode(&bytes).to_lowercase()
        )
    }

    /// Parses a ticket, tolerating surrounding whitespace and line breaks from copy-paste.
    pub fn decode(text: &str) -> Result<Self> {
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        let body = compact
            .strip_prefix(PREFIX)
            .ok_or_else(|| Error::InvalidTicket(format!("it should start with \"{PREFIX}\"")))?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_uppercase().as_bytes())
            .map_err(|_| Error::InvalidTicket("it was cut off or mistyped".into()))?;
        let wire: Wire = postcard::from_bytes(&bytes)
            .map_err(|_| Error::InvalidTicket("it was cut off or mistyped".into()))?;
        if wire.version != TICKET_VERSION {
            return Err(Error::InvalidTicket(format!(
                "it was made by a different Yonder version (ticket v{}, this is v{TICKET_VERSION})",
                wire.version
            )));
        }
        Ok(PairingTicket {
            machine: wire.machine,
            secret: wire.secret,
        })
    }

    pub fn machine_id(&self) -> DeviceId {
        self.machine.id
    }
}

/// Proof that a controller holds the ticket's secret, bound to both device ids so it
/// can't be replayed by or for a different device.
pub(crate) fn pairing_proof(
    secret: &[u8; 32],
    machine: &DeviceId,
    controller: &DeviceId,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_keyed(secret);
    hasher.update(b"yonder pairing proof v1");
    hasher.update(machine.as_bytes());
    hasher.update(controller.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Constant-time check of a pairing proof.
pub(crate) fn proof_matches(expected: &[u8; 32], given: &[u8; 32]) -> bool {
    blake3::Hash::from_bytes(*expected) == blake3::Hash::from_bytes(*given)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceKey;

    fn ticket() -> PairingTicket {
        let id = DeviceKey::generate().id();
        PairingTicket {
            machine: EndpointAddr::new(id).with_ip_addr("192.168.1.20:4433".parse().unwrap()),
            secret: rand::random(),
        }
    }

    #[test]
    fn round_trip() {
        let t = ticket();
        assert_eq!(PairingTicket::decode(&t.encode()).unwrap(), t);
    }

    #[test]
    fn tolerates_whitespace_and_case() {
        let t = ticket();
        let text = t.encode();
        let (a, b) = text.split_at(30);
        let messy = format!("  {}\n  {b}\n", a.to_uppercase());
        assert_eq!(PairingTicket::decode(&messy).unwrap(), t);
    }

    #[test]
    fn rejects_truncated_and_foreign_text() {
        let text = ticket().encode();
        assert!(PairingTicket::decode(&text[..text.len() - 10]).is_err());
        assert!(PairingTicket::decode("hello").is_err());
    }

    #[test]
    fn proof_is_bound_to_both_devices() {
        let secret = rand::random();
        let m = DeviceKey::generate().id();
        let c = DeviceKey::generate().id();
        let other = DeviceKey::generate().id();
        let proof = pairing_proof(&secret, &m, &c);
        assert!(proof_matches(&proof, &pairing_proof(&secret, &m, &c)));
        assert!(!proof_matches(&proof, &pairing_proof(&secret, &m, &other)));
        assert!(!proof_matches(
            &proof,
            &pairing_proof(&rand::random(), &m, &c)
        ));
    }
}
