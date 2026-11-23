use crate::log::ChiakiLogger;
use crate::{ProtoError, Result, check_chiaki, init_lib};
use gnsh_chiaki_sys::{
    ChiakiDiscoveryHost, ChiakiDiscoveryService, ChiakiDiscoveryServiceOptions,
    chiaki_discovery_host_is_ps5, chiaki_discovery_host_state_t_CHIAKI_DISCOVERY_HOST_STATE_READY,
    chiaki_discovery_host_state_t_CHIAKI_DISCOVERY_HOST_STATE_STANDBY,
    chiaki_discovery_service_fini, chiaki_discovery_service_init, chiaki_discovery_wakeup,
    sockaddr_storage,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::os::raw::c_void;
use std::sync::Mutex;
use std::time::Duration;
use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
use windows::Win32::NetworkManagement::IpHelper::{GetAdaptersInfo, IP_ADAPTER_INFO};

const IF_TYPE_ETHERNET: u32 = 6;
const IF_TYPE_IEEE80211: u32 = 71;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleState {
    Ready,
    Standby,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredConsole {
    pub host_id: String,
    pub host_name: String,
    pub addr: String,
    pub state: ConsoleState,
    pub is_ps5: bool,
    pub system_version: String,
    pub running_app_name: String,
}

#[repr(C)]
struct SockAddrIn {
    sin_family: i16,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

fn ipv4_to_sockaddr_storage(ip: Ipv4Addr) -> sockaddr_storage {
    let mut storage = sockaddr_storage::default();
    let addr_in = SockAddrIn {
        sin_family: 2, // AF_INET
        sin_port: 0,
        sin_addr: u32::from_ne_bytes(ip.octets()),
        sin_zero: [0; 8],
    };
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&raw const addr_in).cast::<u8>(),
            (&raw mut storage).cast::<u8>(),
            std::mem::size_of::<SockAddrIn>(),
        );
    }
    storage
}

fn ipv4_broadcast_addrs() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    unsafe {
        let mut size = std::mem::size_of::<IP_ADAPTER_INFO>() as u32;
        let mut buf = vec![0u8; size as usize];
        let mut ret = GetAdaptersInfo(Some(buf.as_mut_ptr().cast::<IP_ADAPTER_INFO>()), &mut size);
        if ret == ERROR_BUFFER_OVERFLOW.0 {
            buf.resize(size as usize, 0);
            ret = GetAdaptersInfo(Some(buf.as_mut_ptr().cast::<IP_ADAPTER_INFO>()), &mut size);
        }
        if ret != 0 || size == 0 {
            return out;
        }

        let mut adapter = buf.as_ptr().cast::<IP_ADAPTER_INFO>();
        while !adapter.is_null() {
            let info = &*adapter;
            if info.Type == IF_TYPE_ETHERNET || info.Type == IF_TYPE_IEEE80211 {
                let mut ip_node: *const _ = &raw const info.IpAddressList;
                while !ip_node.is_null() {
                    let entry = &*ip_node;
                    let ip_str =
                        CStr::from_ptr(entry.IpAddress.String.as_ptr().cast()).to_string_lossy();
                    let mask_str =
                        CStr::from_ptr(entry.IpMask.String.as_ptr().cast()).to_string_lossy();
                    if let (Ok(ip), Ok(mask)) =
                        (ip_str.parse::<Ipv4Addr>(), mask_str.parse::<Ipv4Addr>())
                        && !ip.is_unspecified()
                    {
                        let bcast = Ipv4Addr::from(u32::from(ip) | !u32::from(mask));
                        if seen.insert(bcast) {
                            out.push(bcast);
                        }
                    }
                    ip_node = entry.Next;
                }
            }
            adapter = info.Next;
        }
    }

    out
}

unsafe fn cstr_to_string(ptr: *const std::os::raw::c_char) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

const MAX_UNICAST_DISCOVERY_TARGETS: usize = 32;

/// Strips control characters (including ANSI escapes and CR/LF) from untrusted network text
/// and truncates cleanly to `max_chars`.
pub fn sanitize_network_text(raw: &str, max_chars: usize) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

unsafe extern "C" fn discovery_cb(
    hosts: *mut ChiakiDiscoveryHost,
    hosts_count: usize,
    user: *mut c_void,
) {
    if hosts.is_null() || user.is_null() {
        return;
    }
    let collected = unsafe { &*(user.cast::<Mutex<Vec<DiscoveredConsole>>>()) };
    let mut list = Vec::new();
    for i in 0..hosts_count {
        let host_ptr = unsafe { hosts.add(i) };
        let host = unsafe { &*host_ptr };
        let is_ps5 = unsafe { chiaki_discovery_host_is_ps5(host_ptr) };
        if !is_ps5 {
            continue;
        }
        let state = match host.state {
            x if x == chiaki_discovery_host_state_t_CHIAKI_DISCOVERY_HOST_STATE_READY => {
                ConsoleState::Ready
            }
            x if x == chiaki_discovery_host_state_t_CHIAKI_DISCOVERY_HOST_STATE_STANDBY => {
                ConsoleState::Standby
            }
            _ => ConsoleState::Unknown,
        };
        let host_id = sanitize_network_text(&unsafe { cstr_to_string(host.host_id) }, 64);
        let host_name = sanitize_network_text(&unsafe { cstr_to_string(host.host_name) }, 128);
        let addr = sanitize_network_text(&unsafe { cstr_to_string(host.host_addr) }, 64);
        let system_version =
            sanitize_network_text(&unsafe { cstr_to_string(host.system_version) }, 32);
        let running_app_name =
            sanitize_network_text(&unsafe { cstr_to_string(host.running_app_name) }, 128);
        list.push(DiscoveredConsole {
            host_id,
            host_name,
            addr,
            state,
            is_ps5,
            system_version,
            running_app_name,
        });
    }
    if let Ok(mut guard) = collected.lock() {
        *guard = list;
    }
}

/// Normalizes a LAN IP, hostname, or remote DDNS address by trimming whitespace and removing
/// optional `http(s)://` prefixes, path/query/fragment suffixes, and explicit `:port` suffixes.
pub fn normalize_console_host(raw: &str) -> String {
    let mut s = raw.trim();
    if s.get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    {
        s = &s[8..];
    } else if s
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
    {
        s = &s[7..];
    }
    if let Some((head, _)) = s.split_once(['/', '?', '#']) {
        s = head;
    }
    if let Some((host_part, port_part)) = s.rsplit_once(':')
        && !host_part.contains(':')
        && !port_part.is_empty()
        && port_part.chars().all(|c| c.is_ascii_digit())
    {
        s = host_part;
    }
    s.trim().to_string()
}

/// Validates and normalizes a console host (IPv4 or RFC 1123 hostname), rejecting userinfo `@`,
/// control/whitespace characters, injection delimiters, invalid labels, and dangerous IP ranges.
pub fn validate_console_host(raw: &str) -> Result<String> {
    if raw.chars().any(|c| c.is_control()) {
        return Err(ProtoError::InvalidArg(
            "host contains control characters".into(),
        ));
    }
    let host = normalize_console_host(raw);
    if host.is_empty() || host.len() > 253 {
        return Err(ProtoError::InvalidArg(
            "host must be 1..=253 characters".into(),
        ));
    }
    if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '@' || c == ';')
    {
        return Err(ProtoError::InvalidArg(
            "host contains disallowed characters".into(),
        ));
    }

    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        if ip.is_unspecified()
            || ip.is_broadcast()
            || ip.is_multicast()
            || ip == Ipv4Addr::new(169, 254, 169, 254)
        {
            return Err(ProtoError::InvalidArg(format!(
                "refusing unsafe or non-unicast IPv4 host: {ip}"
            )));
        }
        return Ok(host);
    }

    let hostname = host.strip_suffix('.').unwrap_or(&host);
    if hostname.is_empty() {
        return Err(ProtoError::InvalidArg("host cannot be empty".into()));
    }
    let labels: Vec<&str> = hostname.split('.').collect();
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(ProtoError::InvalidArg(format!(
                "invalid hostname syntax: {host}"
            )));
        }
    }
    if labels
        .last()
        .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(ProtoError::InvalidArg(format!(
            "invalid numeric host: {host}"
        )));
    }
    Ok(host)
}

/// Validates and resolves an IPv4 literal or hostname to an `Ipv4Addr`.
pub fn resolve_host_ipv4(host: &str) -> Option<Ipv4Addr> {
    let validated = validate_console_host(host).ok()?;
    if let Ok(ip) = validated.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    (validated.as_str(), 9302u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|addr| match addr {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            SocketAddr::V6(_) => None,
        })
}

/// Discovers PS5 consoles on LAN broadcasts plus any extra validated unicast LAN/WAN/VPN hosts.
pub fn discover_ps5_hosts(
    timeout: Duration,
    extra_hosts: &[String],
) -> Result<Vec<DiscoveredConsole>> {
    init_lib()?;
    let mut logger = ChiakiLogger::new(false);

    let collected = Box::new(Mutex::new(Vec::<DiscoveredConsole>::new()));
    let collected_ptr = Box::into_raw(collected);

    let mut target_ips = ipv4_broadcast_addrs();
    let mut seen: HashSet<Ipv4Addr> = target_ips.iter().copied().collect();
    let mut unicast_count = 0usize;
    for host in extra_hosts {
        if unicast_count >= MAX_UNICAST_DISCOVERY_TARGETS {
            break;
        }
        if let Some(ip) = resolve_host_ipv4(host)
            && !ip.is_unspecified()
            && seen.insert(ip)
        {
            target_ips.push(ip);
            unicast_count += 1;
        }
    }

    let mut send_addr = ipv4_to_sockaddr_storage(Ipv4Addr::BROADCAST);
    let mut bcast_addrs: Vec<sockaddr_storage> = target_ips
        .into_iter()
        .map(ipv4_to_sockaddr_storage)
        .collect();

    let mut options = ChiakiDiscoveryServiceOptions {
        hosts_max: 16,
        host_drop_pings: 6,
        ping_ms: 300,
        ping_initial_ms: 0,
        send_addr: &mut send_addr,
        send_addr_size: std::mem::size_of::<SockAddrIn>(),
        broadcast_addrs: if bcast_addrs.is_empty() {
            std::ptr::null_mut()
        } else {
            bcast_addrs.as_mut_ptr()
        },
        broadcast_num: bcast_addrs.len(),
        send_host: std::ptr::null_mut(),
        cb: Some(discovery_cb),
        cb_user: collected_ptr.cast(),
    };

    let mut service = Box::new(ChiakiDiscoveryService::default());
    let init_err = unsafe {
        chiaki_discovery_service_init(service.as_mut(), &mut options, logger.as_mut_ptr())
    };
    if init_err != gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
        unsafe { drop(Box::from_raw(collected_ptr)) };
        return check_chiaki(init_err).map(|_| Vec::new());
    }

    std::thread::sleep(timeout);

    // Joins discovery thread cleanly before reclaiming `collected_ptr`.
    unsafe {
        chiaki_discovery_service_fini(service.as_mut());
    }
    let result = unsafe { Box::from_raw(collected_ptr) };
    let hosts = result.into_inner().unwrap_or_default();
    Ok(hosts)
}

/// Discovers PS5 consoles on the local IPv4 LAN within `timeout`.
pub fn discover_ps5_lan(timeout: Duration) -> Result<Vec<DiscoveredConsole>> {
    discover_ps5_hosts(timeout, &[])
}

/// Sends a DDP wake-up packet to a PS5 console host (LAN or port-forwarded WAN) using its hex registration key.
pub fn wake_ps5(host: &str, regist_key_hex: &str) -> Result<()> {
    init_lib()?;
    let validated = validate_console_host(host)?;
    let c_host = CString::new(validated.as_str())
        .map_err(|_| ProtoError::InvalidArg("host contains interior NUL".into()))?;
    let credential = parse_regist_key_credential(regist_key_hex)?;

    let mut logger = ChiakiLogger::new(false);
    let err = unsafe {
        chiaki_discovery_wakeup(
            logger.as_mut_ptr(),
            std::ptr::null_mut(),
            c_host.as_ptr(),
            credential,
            true,
        )
    };
    check_chiaki(err)
}

pub(crate) fn parse_regist_key_credential(regist_key_hex: &str) -> Result<u64> {
    let trimmed = regist_key_hex.trim_matches(['\0', ' ', '\t', '\r', '\n']);
    if trimmed.is_empty() || trimmed.len() > 16 {
        return Err(ProtoError::InvalidArg(
            "regist_key must be 1..=16 hex characters".into(),
        ));
    }
    u64::from_str_radix(trimmed, 16)
        .map_err(|e| ProtoError::InvalidArg(format!("invalid hex regist_key: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_resolves_and_rejects_malicious_hosts_and_escape_payloads() {
        assert_eq!(normalize_console_host("192.168.1.50:9295"), "192.168.1.50");
        assert_eq!(
            normalize_console_host("http://my-ps5.duckdns.org:987/?q=1#frag"),
            "my-ps5.duckdns.org"
        );
        assert_eq!(normalize_console_host("100.64.0.1"), "100.64.0.1");
        assert_eq!(
            resolve_host_ipv4("127.0.0.1:9295"),
            Some(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(resolve_host_ipv4("   "), None);

        assert!(validate_console_host("my-ps5.duckdns.org").is_ok());
        assert!(validate_console_host("192.168.1.50").is_ok());
        assert!(validate_console_host("user:pass@evil.com").is_err());
        assert!(validate_console_host("192.168.1.1;cmd").is_err());
        assert!(validate_console_host("192.168.1.1 foo").is_err());
        assert!(validate_console_host("192.168.1.1\nFAKE").is_err());
        assert!(validate_console_host("0.0.0.0").is_err());
        assert!(validate_console_host("255.255.255.255").is_err());
        assert!(validate_console_host("224.0.0.1").is_err());
        assert!(validate_console_host("169.254.169.254").is_err());
        assert!(validate_console_host("-bad.host").is_err());
        let long_host = format!("{}.example.com", "a".repeat(250));
        assert!(validate_console_host(&long_host).is_err());

        assert_eq!(
            sanitize_network_text("PS5\x1b[31m\nFAKE LOG\r\n", 32),
            "PS5[31mFAKE LOG"
        );
        assert_eq!(sanitize_network_text("abcdef", 3), "abc");
    }

    #[test]
    fn validates_regist_key_hex_for_wakeup() {
        assert_eq!(
            parse_regist_key_credential("1a2b3c4d\0\0").unwrap(),
            0x1a2b3c4d
        );
        assert!(parse_regist_key_credential("").is_err());
        assert!(parse_regist_key_credential("zzzz").is_err());
    }

    #[test]
    fn discovery_service_smoke_lifecycle() {
        let hosts = discover_ps5_lan(Duration::from_millis(50)).expect("discovery init/fini works");
        assert!(hosts.iter().all(|h| h.is_ps5));
    }
}
