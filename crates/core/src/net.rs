//! Network setup shared by agents and controllers.

use std::net::{Ipv4Addr, SocketAddr};

use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl};

use crate::error::{Error, Result};
use crate::identity::DeviceKey;

/// Which relay servers to use when a direct connection isn't possible.
///
/// Relays only forward encrypted traffic; they can't read or change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayChoice {
    /// The public relays run by number 0 (iroh's maintainers).
    Default,
    /// Your own relays (`iroh-relay`), e.g. for a company network.
    Custom(Vec<RelayUrl>),
    /// No relays: direct connections only (same network, or reachable addresses).
    Disabled,
}

/// How a device connects to others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConfig {
    pub relay: RelayChoice,
    /// Publish and look up device addresses through number 0's DNS service, so a device
    /// can be found by its id after its address changes.
    pub address_lookup: bool,
    /// Fixed UDP port to listen on (IPv4 and IPv6), e.g. to allow it through a firewall.
    /// `None` picks a free port.
    pub port: Option<u16>,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            relay: RelayChoice::Default,
            address_lookup: true,
            port: None,
        }
    }
}

impl NetConfig {
    /// No relays and no address lookup: talks only to addresses it's given. For tests and
    /// fully offline networks.
    pub fn local_only() -> Self {
        NetConfig {
            relay: RelayChoice::Disabled,
            address_lookup: false,
        }
    }

    /// Parses a relay setting: `default`, `none`, or comma-separated relay URLs.
    pub fn parse_relay(value: &str) -> Result<RelayChoice> {
        match value.trim() {
            "" | "default" => Ok(RelayChoice::Default),
            "none" | "off" => Ok(RelayChoice::Disabled),
            urls => urls
                .split(',')
                .map(|url| {
                    url.trim().parse::<RelayUrl>().map_err(|e| {
                        Error::Network(format!("\"{}\" isn't a relay URL: {e}", url.trim()))
                    })
                })
                .collect::<Result<Vec<_>>>()
                .map(RelayChoice::Custom),
        }
    }

    pub(crate) fn uses_relays(&self) -> bool {
        !matches!(self.relay, RelayChoice::Disabled)
    }
}

pub(crate) async fn bind(
    key: &DeviceKey,
    alpns: Vec<Vec<u8>>,
    config: &NetConfig,
) -> Result<Endpoint> {
    let builder = if config.address_lookup {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
    };
    let relay_mode = match &config.relay {
        RelayChoice::Default => RelayMode::Default,
        RelayChoice::Custom(urls) => RelayMode::custom(urls.iter().cloned()),
        RelayChoice::Disabled => RelayMode::Disabled,
    };
    builder
        .secret_key(key.secret_key())
        .alpns(alpns)
        .relay_mode(relay_mode)
        .bind()
        .await
        .map_err(Error::network)
}

/// The endpoint's address on this computer's loopback interface. Reaches it from the same
/// computer without any network; used by tests and local tooling.
pub(crate) fn loopback_addr(endpoint: &Endpoint) -> EndpointAddr {
    endpoint
        .bound_sockets()
        .into_iter()
        .filter(SocketAddr::is_ipv4)
        .fold(EndpointAddr::new(endpoint.id()), |addr, socket| {
            addr.with_ip_addr(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), socket.port()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_setting_parses() {
        assert_eq!(
            NetConfig::parse_relay("default").unwrap(),
            RelayChoice::Default
        );
        assert_eq!(
            NetConfig::parse_relay("none").unwrap(),
            RelayChoice::Disabled
        );
        match NetConfig::parse_relay("https://relay.example.com, https://r2.example.com").unwrap() {
            RelayChoice::Custom(urls) => assert_eq!(urls.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(NetConfig::parse_relay("not a url").is_err());
    }
}
