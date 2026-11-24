pub(crate) mod home;
pub(crate) mod hud;
pub(crate) mod modals;
pub(crate) mod settings;
pub(crate) mod theme;

pub(crate) use hud::render_streaming_ui;
pub(crate) use theme::apply_theme;

use crate::storage::{SavedConsole, StoredSettings};
use gnsh_proto::{ConsoleState, DiscoveredConsole, RegisteredConsole, resolve_host_ipv4};

pub(crate) type StreamSettings = StoredSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsTab {
    #[default]
    Video,
    Audio,
    Input,
}

impl SettingsTab {
    pub(crate) const fn prev(self) -> Self {
        match self {
            Self::Video => Self::Input,
            Self::Audio => Self::Video,
            Self::Input => Self::Audio,
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Video => Self::Audio,
            Self::Audio => Self::Input,
            Self::Input => Self::Video,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusBanner {
    pub(crate) kind: BannerKind,
    pub(crate) text: String,
}

impl StatusBanner {
    pub(crate) fn info(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Info,
            text: text.into(),
        }
    }

    pub(crate) fn success(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Success,
            text: text.into(),
        }
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Error,
            text: text.into(),
        }
    }
}

pub(crate) enum HomeAction {
    ConnectSaved(usize),
    ConnectSavedWan(usize),
    WakeSaved(usize),
    CreateShortcut(usize),
    EditSaved(usize),
    DeleteSaved(usize),
    OpenRegister(String),
}

pub(crate) struct RegisterModal {
    pub(crate) host: String,
    pub(crate) redirect_url: String,
    pub(crate) psn_account_id: String,
    pub(crate) pin_input: String,
    pub(crate) notice: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) exchange_rx:
        Option<crossbeam_channel::Receiver<std::result::Result<String, String>>>,
    pub(crate) register_rx:
        Option<crossbeam_channel::Receiver<std::result::Result<RegisteredConsole, String>>>,
}

impl RegisterModal {
    pub(crate) fn new(host: String) -> Self {
        Self {
            host,
            redirect_url: String::new(),
            psn_account_id: String::new(),
            pin_input: String::new(),
            notice: None,
            error: None,
            exchange_rx: None,
            register_rx: None,
        }
    }
}

pub(crate) struct EditConsoleModal {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) remote_host: String,
    pub(crate) error: Option<String>,
}

pub(crate) fn console_state_label(state: ConsoleState) -> &'static str {
    match state {
        ConsoleState::Ready => "Ready",
        ConsoleState::Standby => "Standby",
        ConsoleState::Unknown => "Unknown",
    }
}

pub(crate) fn format_mac_colon(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub(crate) fn format_mac_id(mac: [u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub(crate) fn saved_from_registered(
    reg: RegisteredConsole,
    discovered: &[DiscoveredConsole],
) -> SavedConsole {
    let mac_id = format_mac_id(reg.host_mac);
    let id = discovered
        .iter()
        .find(|d| d.addr.eq_ignore_ascii_case(reg.addr.trim()) && !d.host_id.is_empty())
        .map(|d| d.host_id.clone())
        .unwrap_or(mac_id);

    SavedConsole {
        id,
        name: reg.host_name,
        host: reg.addr,
        remote_host: None,
        mac: reg.host_mac,
        psn_account_id: reg.psn_account_id,
        regist_key: reg.regist_key,
        rp_key: reg.rp_key,
    }
}

pub(crate) fn upsert_saved_console(saved: &mut Vec<SavedConsole>, mut console: SavedConsole) {
    if let Some(existing) = saved.iter_mut().find(|s| {
        s.id.eq_ignore_ascii_case(&console.id)
            || (console.mac != [0; 6] && s.mac == console.mac)
            || s.host.eq_ignore_ascii_case(&console.host)
    }) {
        if console.remote_host.is_none() {
            console.remote_host = existing.remote_host.clone();
        }
        *existing = console;
    } else {
        saved.push(console);
    }
}

fn addr_matches_host(target: &str, disc_addr: &str) -> bool {
    let t = target.trim();
    let d = disc_addr.trim();
    if t.is_empty() || d.is_empty() {
        return false;
    }
    t.eq_ignore_ascii_case(d) || resolve_host_ipv4(t).is_some_and(|ip| ip.to_string() == d)
}

pub(crate) fn host_matches_remote_probe(saved: &SavedConsole, disc_addr: &str) -> bool {
    saved
        .remote_host
        .as_deref()
        .is_some_and(|remote| addr_matches_host(remote, disc_addr))
}

pub(crate) fn find_saved_index_for_discovered(
    saved: &[SavedConsole],
    disc: &DiscoveredConsole,
) -> Option<usize> {
    saved.iter().position(|s| {
        addr_matches_host(&s.host, &disc.addr)
            || host_matches_remote_probe(s, &disc.addr)
            || (!disc.host_id.is_empty() && s.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(s.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
}

pub(crate) fn find_discovered_for_saved<'a>(
    discovered: &'a [DiscoveredConsole],
    saved: &SavedConsole,
) -> Option<&'a DiscoveredConsole> {
    discovered.iter().find(|disc| {
        addr_matches_host(&saved.host, &disc.addr)
            || host_matches_remote_probe(saved, &disc.addr)
            || (!disc.host_id.is_empty() && saved.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(saved.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_saved() -> SavedConsole {
        SavedConsole {
            id: "001122334455".into(),
            name: "PS5-LivingRoom".into(),
            host: "192.168.1.50".into(),
            remote_host: Some("100.64.0.10".into()),
            mac: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        }
    }

    #[test]
    fn upsert_preserves_remote_host_and_lookup_matches_lan_and_wan() {
        let mut saved = vec![sample_saved()];
        let re_registered = SavedConsole {
            remote_host: None,
            host: "192.168.1.51".into(),
            ..sample_saved()
        };
        upsert_saved_console(&mut saved, re_registered);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].host, "192.168.1.51");
        assert_eq!(saved[0].remote_host.as_deref(), Some("100.64.0.10"));

        let lan_disc = DiscoveredConsole {
            host_id: "other-id".into(),
            host_name: "PS5".into(),
            addr: "192.168.1.51".into(),
            state: ConsoleState::Ready,
            is_ps5: true,
            system_version: "10.00".into(),
            running_app_name: String::new(),
        };
        let wan_disc = DiscoveredConsole {
            addr: "100.64.0.10".into(),
            ..lan_disc.clone()
        };
        let list = [lan_disc, wan_disc];
        assert_eq!(
            find_discovered_for_saved(&list[..1], &saved[0]).map(|d| d.addr.as_str()),
            Some("192.168.1.51")
        );
        assert_eq!(
            find_discovered_for_saved(&list[1..], &saved[0]).map(|d| d.addr.as_str()),
            Some("100.64.0.10")
        );
        assert!(host_matches_remote_probe(&saved[0], "100.64.0.10"));
        assert!(!host_matches_remote_probe(&saved[0], "192.168.1.51"));
    }

    #[test]
    fn settings_tab_prev_and_next_wrap_around() {
        assert_eq!(SettingsTab::Video.next(), SettingsTab::Audio);
        assert_eq!(SettingsTab::Audio.next(), SettingsTab::Input);
        assert_eq!(SettingsTab::Input.next(), SettingsTab::Video);

        assert_eq!(SettingsTab::Video.prev(), SettingsTab::Input);
        assert_eq!(SettingsTab::Input.prev(), SettingsTab::Audio);
        assert_eq!(SettingsTab::Audio.prev(), SettingsTab::Video);
    }
}
