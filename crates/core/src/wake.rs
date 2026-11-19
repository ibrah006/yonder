//! Wake-on-LAN: waking a sleeping machine with a "magic packet" (six 0xFF bytes, then
//! its network card's address sixteen times) broadcast on its network.
//!
//! A sleeping machine can't answer Yonder, so the packet comes from something awake on
//! the same network: this computer, or another of your machines running the agent.
//! Machines tell their controllers their card addresses whenever asked for info. The
//! machine's firmware and network card must have Wake-on-LAN turned on, and it usually
//! only works over a cable.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use crate::error::{Error, Result};

/// Parses `aa:bb:cc:dd:ee:ff` (or with `-`).
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (byte, part) in mac.iter_mut().zip(parts) {
        if part.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    (mac != [0; 6]).then_some(mac)
}

pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// IPv4 networks this device is on: (its address, the network's broadcast address).
fn networks() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    let Ok(networks) = sysinfo::Networks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for data in networks.values() {
        for net in data.ip_networks() {
            if let IpAddr::V4(addr) = net.addr
                && !addr.is_loopback()
                && !addr.is_link_local()
                && net.prefix > 0
                && net.prefix <= 30
            {
                let mask = u32::MAX << (32 - u32::from(net.prefix));
                let broadcast = Ipv4Addr::from(u32::from(addr) | !mask);
                found.push((addr, broadcast));
            }
        }
    }
    found
}

/// Broadcasts the magic packet for `mac` on every network this device is on. Returns
/// how many sends went out.
pub fn send(mac: [u8; 6]) -> Result<usize> {
    let packet = magic_packet(mac);
    let mut sent = 0;
    let mut targets: Vec<(Ipv4Addr, Ipv4Addr)> = networks();
    // Also the plain broadcast, from whichever interface the system picks.
    targets.push((Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST));
    for (from, to) in targets {
        let Ok(socket) = UdpSocket::bind(SocketAddr::from((from, 0))) else {
            continue;
        };
        if socket.set_broadcast(true).is_err() {
            continue;
        }
        // 9 (discard) is the usual port; some cards listen on 7.
        for port in [9, 7] {
            if socket
                .send_to(&packet, SocketAddr::from((to, port)))
                .is_ok()
            {
                sent += 1;
            }
        }
    }
    if sent == 0 {
        return Err(Error::Protocol(
            "couldn't send the wake signal on any network".into(),
        ));
    }
    Ok(sent)
}

/// This device's network card addresses: physical cards that have an IPv4 address.
pub fn local_macs() -> Vec<String> {
    let Ok(networks) = sysinfo::Networks::new_with_refreshed_list() else {
        return Vec::new();
    };
    let mut macs: Vec<String> = networks
        .values()
        .filter(|data| {
            data.ip_networks().iter().any(|n| match n.addr {
                IpAddr::V4(a) => !a.is_loopback() && !a.is_link_local(),
                IpAddr::V6(_) => false,
            })
        })
        .map(|data| data.mac_address())
        .filter(|mac| !mac.is_unspecified())
        .map(|mac| mac.to_string())
        .filter(|text| parse_mac(text).is_some())
        .collect();
    macs.sort();
    macs.dedup();
    macs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_parse() {
        assert_eq!(
            parse_mac("aa:bb:cc:01:02:ff"),
            Some([0xaa, 0xbb, 0xcc, 0x01, 0x02, 0xff])
        );
        assert_eq!(
            parse_mac("AA-BB-CC-01-02-FF"),
            parse_mac("aa:bb:cc:01:02:ff")
        );
        for bad in [
            "",
            "aa:bb:cc",
            "00:00:00:00:00:00",
            "aa:bb:cc:dd:ee:gg",
            "aaa:bb:cc:dd:ee:f",
        ] {
            assert_eq!(parse_mac(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_packet_has_the_mac_sixteen_times() {
        let mac = [1, 2, 3, 4, 5, 6];
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 6 + 16 * 6);
        assert_eq!(&packet[..6], &[0xFF; 6]);
        assert!(packet[6..].chunks(6).all(|c| c == mac));
    }

    #[test]
    fn this_computer_lists_its_cards() {
        // Every listed address parses; there may be none on a CI runner.
        assert!(local_macs().iter().all(|m| parse_mac(m).is_some()));
    }
}
