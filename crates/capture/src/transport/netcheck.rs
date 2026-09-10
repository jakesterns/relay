//! Which physical link carries the share. 4K60 over Wi-Fi is unreliable, so
//! the sender surfaces a "use wired or 6 GHz" recommendation when the route to
//! the peer leaves over an 802.11 adapter.

use std::net::IpAddr;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Wired,
    WiFi,
    Other,
    Unknown,
}

impl LinkKind {
    pub fn recommendation(self) -> Option<&'static str> {
        match self {
            LinkKind::WiFi => Some(
                "You're sharing over Wi-Fi. For reliable 4K60, use a wired connection \
                 or a 6 GHz (Wi-Fi 6E) link.",
            ),
            _ => None,
        }
    }
}

/// The link kind of the local adapter that owns `local_ip` (the address the
/// share's UDP/ICE traffic goes out of).
#[cfg(windows)]
pub fn link_kind_for(local_ip: IpAddr) -> Result<LinkKind> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211,
        IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::Networking::WinSock::{AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6};

    // SAFETY: standard GetAdaptersAddresses two-call pattern.
    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 0u32;
        let _ = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, None, &mut size);
        anyhow::ensure!(size > 0, "GetAdaptersAddresses reported no size");
        let mut buf = vec![0u8; size as usize];
        let ret = GetAdaptersAddresses(
            AF_UNSPEC.0 as u32,
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
            &mut size,
        );
        anyhow::ensure!(ret == 0, "GetAdaptersAddresses failed: {ret}");

        let mut adapter = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() {
            let a = &*adapter;
            let mut ua = a.FirstUnicastAddress;
            while !ua.is_null() {
                let sa = (*ua).Address.lpSockaddr;
                if !sa.is_null() {
                    let family = (*sa).sa_family;
                    let matches = match local_ip {
                        IpAddr::V4(v4) => {
                            family == windows::Win32::Networking::WinSock::AF_INET && {
                                let s = &*(sa as *const SOCKADDR_IN);
                                u32::from(v4).to_be() == s.sin_addr.S_un.S_addr
                            }
                        }
                        IpAddr::V6(v6) => {
                            family == windows::Win32::Networking::WinSock::AF_INET6 && {
                                let s = &*(sa as *const SOCKADDR_IN6);
                                s.sin6_addr.u.Byte == v6.octets()
                            }
                        }
                    };
                    if matches {
                        return Ok(match a.IfType {
                            x if x == IF_TYPE_IEEE80211 => LinkKind::WiFi,
                            x if x == IF_TYPE_ETHERNET_CSMACD => LinkKind::Wired,
                            _ => LinkKind::Other,
                        });
                    }
                }
                ua = (*ua).Next;
            }
            adapter = a.Next;
        }
    }
    Ok(LinkKind::Unknown)
}

/// Local IP the OS routes toward `peer` (the interface ICE will use).
pub fn local_ip_towards(peer: IpAddr) -> Result<IpAddr> {
    let s = std::net::UdpSocket::bind(if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    s.connect((peer, 9)).context("no route to peer")?;
    Ok(s.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wifi_gets_a_recommendation() {
        assert!(LinkKind::WiFi.recommendation().is_some());
        assert!(LinkKind::Wired.recommendation().is_none());
        assert!(LinkKind::Other.recommendation().is_none());
        assert!(LinkKind::Unknown.recommendation().is_none());
    }

    #[test]
    fn link_kind_serializes_snake_case() {
        // Locks the wire format of the `link` event's `kind` field.
        assert_eq!(serde_json::to_string(&LinkKind::WiFi).unwrap(), r#""wi_fi""#);
        assert_eq!(serde_json::to_string(&LinkKind::Wired).unwrap(), r#""wired""#);
        assert_eq!(serde_json::to_string(&LinkKind::Unknown).unwrap(), r#""unknown""#);
    }

    #[test]
    fn local_ip_towards_loopback_is_loopback() {
        let ip = local_ip_towards("127.0.0.1".parse().unwrap()).unwrap();
        assert!(ip.is_loopback(), "{ip}");
    }

    #[cfg(windows)]
    #[test]
    fn loopback_adapter_is_never_reported_as_wifi() {
        // The loopback pseudo-interface always exists on Windows; whatever it
        // classifies as, it must not trigger the Wi-Fi warning.
        let kind = link_kind_for("127.0.0.1".parse().unwrap()).unwrap();
        assert_ne!(kind, LinkKind::WiFi);
    }

    #[cfg(windows)]
    #[test]
    fn unassigned_address_is_unknown() {
        // TEST-NET-3 (RFC 5737) is never a local adapter address.
        let kind = link_kind_for("203.0.113.7".parse().unwrap()).unwrap();
        assert_eq!(kind, LinkKind::Unknown);
    }
}
