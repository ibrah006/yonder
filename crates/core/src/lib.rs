//! Shared code for the Yonder agent, CLI and app.
//!
//! - [`identity`]: each device's key; its public half is the device id.
//! - [`ticket`]: pairing tickets shown on a machine and entered on a controller.
//! - [`protocol`]: signed requests and responses.
//! - [`agent`] / [`controller`]: the two sides of a connection.
//! - [`store`]: small on-disk state files.

use std::fmt;

use serde::{Deserialize, Serialize};

pub mod agent;
pub mod audit;
pub mod controller;
pub mod credentials;
pub mod error;
pub mod footprint;
pub mod identity;
pub mod jobs;
pub mod net;
pub mod permissions;
pub mod protocol;
pub mod recipes;
pub mod secrets;
pub mod stats;
pub mod store;
pub mod terminal;
pub mod ticket;
pub mod wake;
pub mod workspace;

pub use error::{Error, Result};

/// Version of the wire protocol spoken between controllers and agents.
///
/// Part of the connection's ALPN, so peers with different versions fail to connect with a
/// clear error rather than misread each other. Bump on any incompatible change.
pub const PROTOCOL_VERSION: u32 = 1;

/// Version of this Yonder build, taken from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Operating system family of a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    /// The OS this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }

    /// Short lowercase name used in machine labels and on the command line (`win`, `mac`, `linux`).
    pub const fn short_name(self) -> &'static str {
        match self {
            Os::Windows => "win",
            Os::MacOs => "mac",
            Os::Linux => "linux",
        }
    }

    /// Parses a short or full OS name, ignoring case.
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "win" | "windows" => Some(Os::Windows),
            "mac" | "macos" | "darwin" => Some(Os::MacOs),
            "linux" => Some(Os::Linux),
            _ => None,
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Os::Windows => "Windows",
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names_round_trip() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert_eq!(Os::parse(os.short_name()), Some(os));
        }
    }

    #[test]
    fn parse_accepts_full_names_in_any_case() {
        assert_eq!(Os::parse("Windows"), Some(Os::Windows));
        assert_eq!(Os::parse("MACOS"), Some(Os::MacOs));
        assert_eq!(Os::parse("darwin"), Some(Os::MacOs));
        assert_eq!(Os::parse("plan9"), None);
    }

    #[test]
    fn current_matches_build_target() {
        let expected = if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        };
        assert_eq!(Os::current(), expected);
    }
}
