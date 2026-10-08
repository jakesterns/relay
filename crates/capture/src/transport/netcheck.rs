//! Which physical link carries the share, and what it is (S49).
//!
//! 4K60 over Wi-Fi is unreliable (CLAUDE.md, key risk 5), so each end works
//! out whether its route to the other PC leaves over 802.11 or Ethernet, and
//! — for Wi-Fi — the band and the rate the radio negotiated. Both ends send
//! theirs in signalling, so either screen can say "Wi-Fi (5 GHz, 866 Mb/s)"
//! about either end.
//!
//! Everything here is read-only and asks for no permission:
//!
//! * `GetAdaptersAddresses` gives the adapter type (IfType 71 is 802.11,
//!   6 is Ethernet) and its link speed, which for Wi-Fi is the PHY rate the
//!   radio is running at.
//! * `wlanapi` gives the band. Only the opcodes Windows 11 24H2 does *not*
//!   put behind location consent are used: the channel number, and the
//!   realtime connection quality (which carries the centre frequency). The
//!   current-connection opcode and the BSS list would raise a location
//!   prompt, because they carry the SSID — so they are never called.
//!   `wlanapi.dll` is loaded on demand, so a PC without it (a server SKU)
//!   still runs.
//!
//! The parsing is pure and fixture-tested; the Windows calls only fill the
//! rows it parses.

use std::net::IpAddr;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
                "This share runs over Wi-Fi. For steady 4K60, Ethernet or a 6 GHz \
                 (Wi-Fi 6E) link holds up best; Relay adjusts to keep the picture smooth.",
            ),
            _ => None,
        }
    }
}

/// The Wi-Fi band, from the channel's centre frequency (or channel number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Band {
    #[serde(rename = "2.4")]
    G2_4,
    #[serde(rename = "5")]
    G5,
    #[serde(rename = "6")]
    G6,
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::G2_4 => "2.4 GHz",
            Band::G5 => "5 GHz",
            Band::G6 => "6 GHz",
        }
    }

    pub fn from_mhz(mhz: u32) -> Option<Band> {
        match mhz {
            2400..=2500 => Some(Band::G2_4),
            4900..=5925 => Some(Band::G5),
            5926..=7125 => Some(Band::G6),
            _ => None,
        }
    }

    /// From a channel number alone. 6 GHz reuses 1-233, so a low number is
    /// only 2.4 GHz when the radio is too old to be on 6 GHz; otherwise it is
    /// ambiguous and the band is left unsaid rather than guessed.
    pub fn from_channel(ch: u32, phy: Option<u32>) -> Option<Band> {
        // An unknown generation might be 6E: treat it as able.
        let six_capable = phy.is_none_or(|p| p >= PHY_HE);
        // 6 GHz channel numbers are 4n+1 up to 233, and 2.
        let six_number = ch == 2 || (ch % 4 == 1 && ch <= 233);
        let legacy = match ch {
            1..=14 => Some(Band::G2_4),
            32..=177 => Some(Band::G5),
            _ => None,
        };
        match (six_capable, six_number) {
            (true, true) if legacy.is_none() => Some(Band::G6),
            (true, true) => None,
            _ => legacy,
        }
    }
}

/// `DOT11_PHY_TYPE` values that name a Wi-Fi generation.
const PHY_HT: u32 = 7;
const PHY_VHT: u32 = 8;
const PHY_HE: u32 = 10;
const PHY_EHT: u32 = 11;

fn phy_label(phy: u32) -> Option<&'static str> {
    match phy {
        PHY_HT => Some("Wi-Fi 4"),
        PHY_VHT => Some("Wi-Fi 5"),
        PHY_HE => Some("Wi-Fi 6"),
        PHY_EHT => Some("Wi-Fi 7"),
        _ => None,
    }
}

/// What one end knows about its link. Travels in signalling (S49), so every
/// field is optional and an older peer simply sends none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub kind: LinkKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<Band>,
    /// "Wi-Fi 6", when the radio says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phy: Option<String>,
    /// Negotiated link rate, Mb/s (for Wi-Fi, the PHY rate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbps: Option<u32>,
}

impl LinkInfo {
    pub fn unknown() -> Self {
        Self { kind: LinkKind::Unknown, band: None, phy: None, mbps: None }
    }

    pub fn is_wifi(&self) -> bool {
        self.kind == LinkKind::WiFi
    }

    /// "Wi-Fi (5 GHz, 866 Mb/s)", "Wired (1 Gb/s)", "Wi-Fi".
    pub fn label(&self) -> String {
        let name = match self.kind {
            LinkKind::WiFi => "Wi-Fi",
            LinkKind::Wired => "Wired",
            LinkKind::Other => "Network",
            LinkKind::Unknown => "Unknown link",
        };
        let mut parts: Vec<String> = Vec::new();
        if let Some(b) = self.band {
            parts.push(b.label().into());
        } else if let Some(p) = &self.phy {
            parts.push(p.clone());
        }
        if let Some(m) = self.mbps.filter(|m| *m > 0) {
            parts.push(if m >= 1000 && m % 1000 == 0 {
                format!("{} Gb/s", m / 1000)
            } else if m >= 1000 {
                format!("{:.1} Gb/s", m as f64 / 1000.0)
            } else {
                format!("{m} Mb/s")
            });
        }
        if parts.is_empty() {
            name.into()
        } else {
            format!("{name} ({})", parts.join(", "))
        }
    }
}

/// The calm note for the screens when either end is on Wi-Fi (S49); `None`
/// on wired, where nothing changes. Says what the link is and what helps,
/// never that the user did something wrong.
pub fn wifi_note(local: &LinkInfo, peer: Option<&LinkInfo>) -> Option<String> {
    let here = local.is_wifi();
    let there = peer.is_some_and(LinkInfo::is_wifi);
    let which = match (here, there) {
        (false, false) => return None,
        (true, true) => "Both PCs are on Wi-Fi",
        (true, false) => "This PC is on Wi-Fi",
        (false, true) => "The other PC is on Wi-Fi",
    };
    Some(format!(
        "{which}. For steady 4K60, Ethernet or Wi-Fi 6E holds up best; \
         Relay adjusts quality to keep the picture smooth."
    ))
}

/// One adapter as `GetAdaptersAddresses` reports it — the parser's input,
/// and the shape of the fixtures.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterRow {
    /// `IfType`: 6 Ethernet, 71 IEEE 802.11, 24 loopback, 131 tunnel, ...
    pub if_type: u32,
    /// `AdapterName`: the interface GUID, braces included.
    #[serde(default)]
    pub guid: String,
    #[serde(default)]
    pub addrs: Vec<IpAddr>,
    /// `TransmitLinkSpeed` / `ReceiveLinkSpeed`, bits per second.
    #[serde(default)]
    pub tx_bps: u64,
    #[serde(default)]
    pub rx_bps: u64,
}

/// What `wlanapi` said about one Wi-Fi interface (any part may be missing).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WlanRow {
    #[serde(default)]
    pub channel: Option<u32>,
    #[serde(default)]
    pub phy: Option<u32>,
    #[serde(default)]
    pub freq_mhz: Option<u32>,
}

pub const IF_TYPE_ETHERNET: u32 = 6;
pub const IF_TYPE_WIFI: u32 = 71;

/// The adapter that owns `local_ip`, if any.
pub fn adapter_for(rows: &[AdapterRow], local_ip: IpAddr) -> Option<&AdapterRow> {
    rows.iter().find(|r| r.addrs.contains(&local_ip))
}

/// Classify the adapter that owns `local_ip`, with Wi-Fi detail from `wlan`.
pub fn classify(rows: &[AdapterRow], local_ip: IpAddr, wlan: Option<WlanRow>) -> LinkInfo {
    let Some(a) = adapter_for(rows, local_ip) else { return LinkInfo::unknown() };
    let kind = match a.if_type {
        IF_TYPE_WIFI => LinkKind::WiFi,
        IF_TYPE_ETHERNET => LinkKind::Wired,
        _ => LinkKind::Other,
    };
    // The slower direction is the honest number; an unknown speed reads as
    // u64::MAX on some drivers and 0 on others.
    let bps = [a.tx_bps, a.rx_bps].into_iter().filter(|b| *b > 0 && *b < u64::MAX).min();
    let mbps = bps.map(|b| (b / 1_000_000) as u32).filter(|m| *m > 0);
    let mut info = LinkInfo { kind, band: None, phy: None, mbps };
    if kind == LinkKind::WiFi {
        if let Some(w) = wlan {
            info.band = w
                .freq_mhz
                .and_then(Band::from_mhz)
                .or_else(|| w.channel.and_then(|c| Band::from_channel(c, w.phy)));
            info.phy = w.phy.and_then(phy_label).map(str::to_string);
        }
    }
    info
}

/// The link of the local adapter that owns `local_ip` (the address the
/// share's UDP/ICE traffic goes out of).
pub fn link_info_for(local_ip: IpAddr) -> Result<LinkInfo> {
    let rows = adapter_rows()?;
    let wlan = adapter_for(&rows, local_ip)
        .filter(|a| a.if_type == IF_TYPE_WIFI)
        .and_then(|a| wlan_row(&a.guid));
    Ok(classify(&rows, local_ip, wlan))
}

/// Kept for callers that only need the kind.
pub fn link_kind_for(local_ip: IpAddr) -> Result<LinkKind> {
    Ok(link_info_for(local_ip)?.kind)
}

/// Every adapter, as rows.
#[cfg(windows)]
pub fn adapter_rows() -> Result<Vec<AdapterRow>> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };

    let mut rows = Vec::new();
    // SAFETY: standard GetAdaptersAddresses two-call pattern; every pointer
    // walked below lives in `buf`.
    unsafe {
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 0u32;
        let _ = GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, None, &mut size);
        anyhow::ensure!(size > 0, "GetAdaptersAddresses reported no size");
        // u64 storage keeps the structures aligned.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
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
            let mut row = AdapterRow {
                if_type: a.IfType,
                guid: a.AdapterName.to_string().unwrap_or_default(),
                addrs: Vec::new(),
                tx_bps: a.TransmitLinkSpeed,
                rx_bps: a.ReceiveLinkSpeed,
            };
            let mut ua = a.FirstUnicastAddress;
            while !ua.is_null() {
                let sa = (*ua).Address.lpSockaddr;
                if !sa.is_null() {
                    if (*sa).sa_family == AF_INET {
                        let s = &*(sa as *const SOCKADDR_IN);
                        let raw = u32::from_be(s.sin_addr.S_un.S_addr);
                        row.addrs.push(IpAddr::V4(raw.into()));
                    } else if (*sa).sa_family == AF_INET6 {
                        let s = &*(sa as *const SOCKADDR_IN6);
                        row.addrs.push(IpAddr::V6(s.sin6_addr.u.Byte.into()));
                    }
                }
                ua = (*ua).Next;
            }
            rows.push(row);
            adapter = a.Next;
        }
    }
    Ok(rows)
}

#[cfg(not(windows))]
pub fn adapter_rows() -> Result<Vec<AdapterRow>> {
    Ok(Vec::new())
}

/// Band and generation for the Wi-Fi interface `guid`, from `wlanapi`.
/// `None` on any failure: the label then just says "Wi-Fi (866 Mb/s)".
#[cfg(windows)]
fn wlan_row(guid: &str) -> Option<WlanRow> {
    wlan::query(guid)
}

#[cfg(not(windows))]
fn wlan_row(_guid: &str) -> Option<WlanRow> {
    None
}

#[cfg(windows)]
mod wlan {
    //! `wlanapi.dll`, loaded on demand. The structures are declared here
    //! rather than taken from the `windows` crate, whose bindings would put
    //! `wlanapi.dll` in the import table — and a PC without it would then
    //! fail to start `relay-share` at all.

    use std::ffi::c_void;

    use windows::core::{GUID, PCSTR, PCWSTR};
    use windows::Win32::Foundation::{FreeLibrary, HANDLE, HMODULE};
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };

    use super::WlanRow;

    type OpenHandle = unsafe extern "system" fn(u32, *const c_void, *mut u32, *mut HANDLE) -> u32;
    type CloseHandle = unsafe extern "system" fn(HANDLE, *const c_void) -> u32;
    type QueryInterface = unsafe extern "system" fn(
        HANDLE,
        *const GUID,
        i32,
        *const c_void,
        *mut u32,
        *mut *mut c_void,
        *mut i32,
    ) -> u32;
    type FreeMemory = unsafe extern "system" fn(*mut c_void);

    const OPCODE_CHANNEL_NUMBER: i32 = 8;
    /// `wlan_intf_opcode_realtime_connection_quality`. Recent Windows 11
    /// only; older builds return an error and the channel number stands.
    const OPCODE_REALTIME_QUALITY: i32 = 19;

    /// `WLAN_REALTIME_CONNECTION_QUALITY` up to and including the first
    /// link's centre frequency — all that is read.
    #[repr(C)]
    struct RealtimeHead {
        phy: u32,
        link_quality: u32,
        rx_rate: u32,
        tx_rate: u32,
        is_mlo: i32,
        num_links: u32,
        // WLAN_REALTIME_CONNECTION_QUALITY_LINK_INFO[0]
        link_id: u8,
        center_mhz: u32,
    }

    struct Lib(HMODULE);
    impl Drop for Lib {
        fn drop(&mut self) {
            // SAFETY: the module was loaded by us and nothing from it outlives this.
            unsafe {
                let _ = FreeLibrary(self.0);
            }
        }
    }

    fn parse_guid(s: &str) -> Option<GUID> {
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.len() != 32 {
            return None;
        }
        Some(GUID::from_u128(u128::from_str_radix(&hex, 16).ok()?))
    }

    pub fn query(guid: &str) -> Option<WlanRow> {
        let guid = parse_guid(guid)?;
        // SAFETY: a System32 DLL loaded by name; every pointer handed to it
        // is ours or one it returned, and returned memory goes back through
        // WlanFreeMemory.
        unsafe {
            let name: Vec<u16> = "wlanapi.dll\0".encode_utf16().collect();
            let lib =
                Lib(LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                    .ok()?);
            let get = |n: &[u8]| GetProcAddress(lib.0, PCSTR(n.as_ptr()));
            let open: OpenHandle = std::mem::transmute(get(b"WlanOpenHandle\0")?);
            let close: CloseHandle = std::mem::transmute(get(b"WlanCloseHandle\0")?);
            let query: QueryInterface = std::mem::transmute(get(b"WlanQueryInterface\0")?);
            let free: FreeMemory = std::mem::transmute(get(b"WlanFreeMemory\0")?);

            let mut version = 0u32;
            let mut h = HANDLE::default();
            if open(2, std::ptr::null(), &mut version, &mut h) != 0 {
                return None;
            }
            let mut row = WlanRow::default();
            let read = |op: i32, f: &mut dyn FnMut(*const u8, u32)| {
                let (mut size, mut data) = (0u32, std::ptr::null_mut::<c_void>());
                let rc = query(
                    h,
                    &guid,
                    op,
                    std::ptr::null(),
                    &mut size,
                    &mut data,
                    std::ptr::null_mut(),
                );
                if rc == 0 && !data.is_null() {
                    f(data as *const u8, size);
                    free(data);
                }
            };
            read(OPCODE_CHANNEL_NUMBER, &mut |p, size| {
                if size >= 4 {
                    row.channel =
                        Some(std::ptr::read_unaligned(p as *const u32)).filter(|c| *c > 0);
                }
            });
            read(OPCODE_REALTIME_QUALITY, &mut |p, size| {
                if size as usize >= std::mem::size_of::<RealtimeHead>() {
                    let q = std::ptr::read_unaligned(p as *const RealtimeHead);
                    // Sanity before trust: an opcode that meant something
                    // else on this build would not look like this.
                    if (1..=16).contains(&q.num_links) && q.phy <= 16 {
                        row.phy = Some(q.phy).filter(|p| *p > 0);
                        row.freq_mhz = Some(q.center_mhz).filter(|f| (2400..=7125).contains(f));
                    }
                }
            });
            close(h, std::ptr::null());
            (row != WlanRow::default()).then_some(row)
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn guids_parse_from_adapter_names() {
            let g = super::parse_guid("{4D36E972-E325-11CE-BFC1-08002BE10318}").unwrap();
            assert_eq!(g, windows::core::GUID::from_u128(0x4D36E972_E325_11CE_BFC1_08002BE10318));
            assert!(super::parse_guid("not-a-guid").is_none());
        }
    }
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

    /// Adapter tables in the shape `adapter_rows` produces, written from
    /// what real PCs report (addresses and GUIDs changed).
    const LAPTOP_WIFI: &str = include_str!("fixtures/adapters-laptop-wifi.json");
    const DESKTOP_WIRED: &str = include_str!("fixtures/adapters-desktop-wired.json");
    const DOCKED: &str = include_str!("fixtures/adapters-docked-both.json");

    fn rows(json: &str) -> Vec<AdapterRow> {
        serde_json::from_str(json).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_wifi_route_is_wifi_with_its_rate_and_band() {
        let r = rows(LAPTOP_WIFI);
        let wlan = WlanRow { channel: Some(149), phy: Some(8), freq_mhz: None };
        let info = classify(&r, ip("192.168.1.42"), Some(wlan));
        assert_eq!(info.kind, LinkKind::WiFi);
        assert_eq!(info.band, Some(Band::G5));
        assert_eq!(info.mbps, Some(866));
        assert_eq!(info.label(), "Wi-Fi (5 GHz, 866 Mb/s)");
        // Without wlanapi it still says what it can.
        assert_eq!(classify(&r, ip("192.168.1.42"), None).label(), "Wi-Fi (866 Mb/s)");
    }

    #[test]
    fn a_wired_route_is_wired_and_says_its_speed() {
        let r = rows(DESKTOP_WIRED);
        let info = classify(&r, ip("192.168.1.10"), None);
        assert_eq!(info.kind, LinkKind::Wired);
        assert_eq!(info.label(), "Wired (1 Gb/s)");
        assert!(info.kind.recommendation().is_none());
        let info = classify(&r, ip("fe80::1c2b:3a4d:5e6f:7081"), None);
        assert_eq!(info.kind, LinkKind::Wired, "IPv6 addresses match too");
    }

    #[test]
    fn with_both_adapters_up_the_route_decides() {
        let r = rows(DOCKED);
        assert_eq!(classify(&r, ip("10.0.0.20"), None).kind, LinkKind::Wired);
        let wifi = classify(&r, ip("10.0.0.21"), None);
        assert_eq!(wifi.kind, LinkKind::WiFi);
        assert_eq!(wifi.label(), "Wi-Fi (1.2 Gb/s)");
        assert_eq!(classify(&r, ip("127.0.0.1"), None).kind, LinkKind::Other, "loopback");
        assert_eq!(classify(&r, ip("203.0.113.7"), None), LinkInfo::unknown());
    }

    #[test]
    fn the_band_comes_from_frequency_first_then_the_channel() {
        let r = rows(DOCKED);
        let at = |w| classify(&r, ip("10.0.0.21"), Some(w));
        // Wi-Fi 6E on 6 GHz channel 5: the frequency settles it.
        let six = at(WlanRow { channel: Some(5), phy: Some(PHY_HE), freq_mhz: Some(5975) });
        assert_eq!((six.band, six.phy.as_deref()), (Some(Band::G6), Some("Wi-Fi 6")));
        assert_eq!(six.label(), "Wi-Fi (6 GHz, 1.2 Gb/s)");
        // Channel 5 with no frequency on a Wi-Fi 6 radio could be either.
        let unsure = at(WlanRow { channel: Some(5), phy: Some(PHY_HE), freq_mhz: None });
        assert_eq!(unsure.band, None);
        assert_eq!(unsure.label(), "Wi-Fi (Wi-Fi 6, 1.2 Gb/s)");
        // An 802.11n radio on channel 6 is 2.4 GHz.
        assert_eq!(
            at(WlanRow { channel: Some(6), phy: Some(PHY_HT), freq_mhz: None }).band,
            Some(Band::G2_4)
        );
        assert_eq!(
            at(WlanRow { channel: Some(36), phy: None, freq_mhz: None }).band,
            Some(Band::G5)
        );
        assert_eq!(at(WlanRow { channel: Some(37), phy: Some(PHY_HE), freq_mhz: None }).band, None);
        assert_eq!(
            at(WlanRow { freq_mhz: Some(2437), ..Default::default() }).band,
            Some(Band::G2_4)
        );
    }

    #[test]
    fn unknown_speeds_are_left_out() {
        let r = vec![AdapterRow {
            if_type: IF_TYPE_WIFI,
            addrs: vec![ip("10.1.1.1")],
            tx_bps: u64::MAX,
            rx_bps: 0,
            ..Default::default()
        }];
        assert_eq!(classify(&r, ip("10.1.1.1"), None).label(), "Wi-Fi");
    }

    #[test]
    fn link_info_round_trips_and_tolerates_missing_fields() {
        let info =
            LinkInfo { kind: LinkKind::WiFi, band: Some(Band::G5), phy: None, mbps: Some(866) };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(json, r#"{"kind":"wi_fi","band":"5","mbps":866}"#);
        assert_eq!(serde_json::from_str::<LinkInfo>(&json).unwrap(), info);
        let bare: LinkInfo = serde_json::from_str(r#"{"kind":"wired"}"#).unwrap();
        assert_eq!(bare.label(), "Wired");
    }

    #[test]
    fn the_note_names_which_end_and_never_appears_on_wired() {
        let wired = LinkInfo { kind: LinkKind::Wired, band: None, phy: None, mbps: Some(1000) };
        let wifi =
            LinkInfo { kind: LinkKind::WiFi, band: Some(Band::G5), phy: None, mbps: Some(866) };
        assert_eq!(wifi_note(&wired, Some(&wired)), None);
        assert_eq!(wifi_note(&wired, None), None, "an older peer that said nothing");
        assert!(wifi_note(&wifi, Some(&wired)).unwrap().starts_with("This PC is on Wi-Fi."));
        assert!(wifi_note(&wired, Some(&wifi)).unwrap().starts_with("The other PC"));
        let both = wifi_note(&wifi, Some(&wifi)).unwrap();
        assert!(both.contains("Ethernet or Wi-Fi 6E"), "{both}");
        for word in ["your", "you ", "fault", "bad"] {
            assert!(!both.to_lowercase().contains(word), "never about the user: {both}");
        }
    }

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
    fn this_pc_classifies_without_error() {
        // Whatever this machine has, reading it must work and loopback must
        // never read as Wi-Fi.
        let kind = link_kind_for("127.0.0.1".parse().unwrap()).unwrap();
        assert_ne!(kind, LinkKind::WiFi);
        assert_eq!(link_kind_for("203.0.113.7".parse().unwrap()).unwrap(), LinkKind::Unknown);
        let rows = adapter_rows().unwrap();
        assert!(rows.iter().any(|r| r.addrs.iter().any(|a| a.is_loopback())), "{rows:?}");
    }
}
