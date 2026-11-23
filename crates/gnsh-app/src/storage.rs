use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gnsh_input::KbmBindings;
use gnsh_media::ScaleMode;
use gnsh_proto::{VideoCodec, VideoPreset, clamp_bitrate_kbps};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct SavedConsole {
    pub id: String,
    pub name: String,
    pub host: String,
    pub remote_host: Option<String>,
    pub mac: [u8; 6],
    pub psn_account_id: String,
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
}

impl fmt::Debug for SavedConsole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedConsole")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("remote_host", &self.remote_host)
            .field("mac", &self.mac)
            .field("psn_account_id", &self.psn_account_id)
            .field("regist_key", &"<redacted>")
            .field("rp_key", &"<redacted>")
            .finish()
    }
}

impl SavedConsole {
    pub fn regist_key_hex(&self) -> String {
        let end = self
            .regist_key
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.regist_key.len());
        String::from_utf8_lossy(&self.regist_key[..end]).into_owned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredConsoleDisk {
    id: String,
    name: String,
    host: String,
    #[serde(default)]
    remote_host: Option<String>,
    mac: [u8; 6],
    psn_account_id_dpapi: String,
    regist_key_dpapi: String,
    rp_key_dpapi: String,
}

fn read_config_file(path: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("failed inspecting {}", path.display()))?;
    if meta.len() > MAX_CONFIG_FILE_BYTES {
        bail!(
            "refusing oversized config {} ({} bytes exceeds 1 MiB limit)",
            path.display(),
            meta.len()
        );
    }
    std::fs::read_to_string(path).with_context(|| format!("failed reading {}", path.display()))
}

fn normalize_opt_host(host: Option<&str>) -> Option<String> {
    host.map(gnsh_proto::normalize_console_host)
        .filter(|s| !s.is_empty())
}

pub struct ConsoleStore {
    path: PathBuf,
}

impl ConsoleStore {
    pub fn default_store() -> Result<Self> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Ok(Self::with_path(
            base.join("gnsh-play").join("consoles.json"),
        ))
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<SavedConsole>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let raw = read_config_file(&self.path)?;
        let entries: Vec<StoredConsoleDisk> =
            serde_json::from_str(&raw).context("failed parsing consoles.json")?;
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let psn_bytes = dpapi_unprotect_b64(&entry.psn_account_id_dpapi)?;
            let psn_account_id = String::from_utf8(psn_bytes).context("invalid UTF-8 psn id")?;
            let reg_bytes = dpapi_unprotect_b64(&entry.regist_key_dpapi)?;
            let rp_bytes = dpapi_unprotect_b64(&entry.rp_key_dpapi)?;
            let regist_key: [u8; 16] = reg_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("decrypted regist_key length != 16"))?;
            let rp_key: [u8; 16] = rp_bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("decrypted rp_key length != 16"))?;
            let host = gnsh_proto::validate_console_host(&entry.host)
                .context("invalid saved LAN host in consoles.json")?;
            let remote_host = match normalize_opt_host(entry.remote_host.as_deref()) {
                Some(r) => match gnsh_proto::validate_console_host(&r) {
                    Ok(valid) => Some(valid),
                    Err(err) => {
                        tracing::warn!(host = %r, "Ignoring unsafe remote_host in consoles.json: {err:#}");
                        None
                    }
                },
                None => None,
            };
            out.push(SavedConsole {
                id: entry.id,
                name: entry.name,
                host,
                remote_host,
                mac: entry.mac,
                psn_account_id,
                regist_key,
                rp_key,
            });
        }
        Ok(out)
    }

    pub fn save(&self, consoles: &[SavedConsole]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }
        let mut disk = Vec::with_capacity(consoles.len());
        for c in consoles {
            let valid_host = gnsh_proto::validate_console_host(&c.host)
                .context("cannot save console with invalid LAN host")?;
            let valid_remote = match normalize_opt_host(c.remote_host.as_deref()) {
                Some(r) => Some(
                    gnsh_proto::validate_console_host(&r)
                        .context("cannot save console with invalid remote host")?,
                ),
                None => None,
            };
            disk.push(StoredConsoleDisk {
                id: c.id.clone(),
                name: c.name.clone(),
                host: valid_host,
                remote_host: valid_remote,
                mac: c.mac,
                psn_account_id_dpapi: dpapi_protect_b64(c.psn_account_id.as_bytes())?,
                regist_key_dpapi: dpapi_protect_b64(&c.regist_key)?,
                rp_key_dpapi: dpapi_protect_b64(&c.rp_key)?,
            });
        }
        let json = serde_json::to_string_pretty(&disk)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("failed writing {}", self.path.display()))?;
        Ok(())
    }
}

fn dpapi_protect_b64(plain: &[u8]) -> Result<String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: plain.len() as u32,
        pbData: plain.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("CryptProtectData failed")?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
        let encoded = STANDARD.encode(slice);
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(encoded)
    }
}

fn dpapi_unprotect_b64(b64: &str) -> Result<Vec<u8>> {
    let cipher = STANDARD.decode(b64).context("invalid base64 DPAPI blob")?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("CryptUnprotectData failed")?;
        let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_roundtrips_saved_console() {
        let dir = std::env::temp_dir().join(format!("gnsh-play-test-{}", std::process::id()));
        let path = dir.join("consoles.json");
        let _ = std::fs::remove_dir_all(&dir);

        let store = ConsoleStore::with_path(&path);
        assert_eq!(store.path(), path);
        let sample = SavedConsole {
            id: "001122334455".into(),
            name: "PS5-LivingRoom".into(),
            host: "192.168.1.50".into(),
            remote_host: Some("ps5.example.com:9295".into()),
            mac: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        };

        let dbg_text = format!("{sample:?}");
        assert!(!dbg_text.contains("49")); // raw byte values from "1a2b..."
        assert!(!dbg_text.contains("170")); // raw byte value 0xAA from rp_key
        assert!(dbg_text.contains("<redacted>"));

        store.save(std::slice::from_ref(&sample)).expect("save");
        let raw = std::fs::read_to_string(&path).expect("read json");
        assert!(!raw.contains("1a2b3c4d"));
        assert!(!raw.contains("AQIDBAUGBwg="));

        let loaded = store.load().expect("load");
        let expected = SavedConsole {
            remote_host: Some("ps5.example.com".into()),
            ..sample
        };
        assert_eq!(loaded, vec![expected.clone()]);
        assert_eq!(loaded[0].regist_key_hex(), "1a2b3c4d");

        // Legacy consoles.json without `remote_host` must load cleanly with `remote_host: None`.
        let legacy_json = raw
            .replace("    \"remote_host\": \"ps5.example.com\",\r\n", "")
            .replace("    \"remote_host\": \"ps5.example.com\",\n", "");
        std::fs::write(&path, legacy_json).expect("write legacy json");
        let legacy_loaded = store.load().expect("load legacy");
        assert_eq!(
            legacy_loaded,
            vec![SavedConsole {
                remote_host: None,
                ..expected
            }]
        );

        // Oversized config files > 1 MiB must be rejected before parsing.
        std::fs::write(&path, vec![b' '; (MAX_CONFIG_FILE_BYTES as usize) + 16])
            .expect("write oversized file");
        assert!(store.load().is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_store_roundtrips_and_clamps_bitrate() {
        let dir = std::env::temp_dir().join(format!("gnsh-play-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);

        let store = SettingsStore::with_path(&path);
        assert_eq!(
            store.load().expect("default load"),
            StoredSettings::default()
        );

        let custom_bindings = KbmBindings {
            cross: gnsh_input::KbmInput::Key(0x6b), // 'k'
            ..KbmBindings::default()
        };
        let custom = StoredSettings {
            preset: VideoPreset::P1080Fps30,
            codec: VideoCodec::HevcHdr,
            bitrate_kbps: 500_000,
            vsync: false,
            touchpad_touch: true,
            sharpness_pct: 250,
            scale_mode: ScaleMode::Zoom,
            volume_pct: 220,
            audio_device: Some("  Speakers (Realtek Audio) ".into()),
            kbm_enabled: true,
            kbm_sensitivity_pct: 200,
            kbm_deadzone_pct: 90,
            kbm_curve_pct: 250,
            kbm_invert_y: true,
            kbm_bindings: custom_bindings.clone(),
        };
        store.save(&custom).expect("save custom");
        let loaded = store.load().expect("load custom");
        assert_eq!(loaded.preset, VideoPreset::P1080Fps30);
        assert_eq!(loaded.codec, VideoCodec::HevcHdr);
        assert_eq!(loaded.bitrate_kbps, 100_000);
        assert!(!loaded.vsync);
        assert!(loaded.touchpad_touch);
        assert_eq!(loaded.sharpness_pct, 100);
        assert_eq!(loaded.scale_mode, ScaleMode::Zoom);
        assert_eq!(loaded.volume_pct, 100);
        assert_eq!(
            loaded.audio_device.as_deref(),
            Some("Speakers (Realtek Audio)")
        );
        assert!(loaded.kbm_enabled);
        assert_eq!(loaded.kbm_sensitivity_pct, 100);
        assert_eq!(loaded.kbm_deadzone_pct, 40);
        assert_eq!(loaded.kbm_curve_pct, 150);
        assert!(loaded.kbm_invert_y);
        assert_eq!(loaded.kbm_bindings, custom_bindings);

        std::fs::write(
            &path,
            r#"{"bitrate_kbps": 999999, "sharpness_pct": 180, "volume_pct": 250, "audio_device": "   ", "kbm_sensitivity_pct": 0, "kbm_deadzone_pct": 80, "kbm_curve_pct": 10}"#,
        )
        .expect("write unclamped json");
        let clamped = store.load().expect("load clamped");
        assert_eq!(clamped.bitrate_kbps, 100_000);
        assert_eq!(clamped.sharpness_pct, 100);
        assert_eq!(clamped.volume_pct, 100);
        assert_eq!(clamped.audio_device, None);
        assert_eq!(clamped.preset, VideoPreset::P1080Fps60);
        assert_eq!(clamped.codec, VideoCodec::Hevc);
        assert_eq!(clamped.scale_mode, ScaleMode::Fit);
        assert!(!clamped.kbm_enabled);
        assert_eq!(clamped.kbm_sensitivity_pct, 1);
        assert_eq!(clamped.kbm_deadzone_pct, 40);
        assert_eq!(clamped.kbm_curve_pct, 50);
        assert!(!clamped.kbm_invert_y);

        std::fs::write(&path, r#"{"kbm_enabled": true}"#).expect("write missing curve json");
        let defaulted = store.load().expect("load missing curve");
        assert_eq!(defaulted.kbm_curve_pct, 100);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Persisted stream and controller preferences stored in `%APPDATA%\gnsh-play\settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoredSettings {
    pub preset: VideoPreset,
    pub codec: VideoCodec,
    pub bitrate_kbps: u32,
    pub vsync: bool,
    pub touchpad_touch: bool,
    pub sharpness_pct: u8,
    pub scale_mode: ScaleMode,
    pub volume_pct: u8,
    pub audio_device: Option<String>,
    pub kbm_enabled: bool,
    pub kbm_sensitivity_pct: u8,
    pub kbm_deadzone_pct: u8,
    pub kbm_curve_pct: u8,
    pub kbm_invert_y: bool,
    pub kbm_bindings: KbmBindings,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            preset: VideoPreset::P1080Fps60,
            codec: VideoCodec::Hevc,
            bitrate_kbps: 30_000,
            vsync: true,
            touchpad_touch: false,
            sharpness_pct: 0,
            scale_mode: ScaleMode::Fit,
            volume_pct: 100,
            audio_device: None,
            kbm_enabled: false,
            kbm_sensitivity_pct: 50,
            kbm_deadzone_pct: 14,
            kbm_curve_pct: 100,
            kbm_invert_y: false,
            kbm_bindings: KbmBindings::default(),
        }
    }
}

fn normalize_settings(settings: &mut StoredSettings) {
    settings.bitrate_kbps = clamp_bitrate_kbps(settings.bitrate_kbps);
    settings.sharpness_pct = settings.sharpness_pct.min(100);
    settings.volume_pct = settings.volume_pct.min(100);
    settings.kbm_sensitivity_pct = settings.kbm_sensitivity_pct.clamp(1, 100);
    settings.kbm_deadzone_pct = settings.kbm_deadzone_pct.min(40);
    if settings.kbm_curve_pct == 0 {
        settings.kbm_curve_pct = 100;
    }
    settings.kbm_curve_pct = settings.kbm_curve_pct.clamp(50, 150);
    settings.audio_device = settings
        .audio_device
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn default_store() -> Result<Self> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Ok(Self::with_path(
            base.join("gnsh-play").join("settings.json"),
        ))
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<StoredSettings> {
        if !self.path.exists() {
            return Ok(StoredSettings::default());
        }
        let raw = read_config_file(&self.path)?;
        let mut parsed: StoredSettings =
            serde_json::from_str(&raw).context("failed parsing settings.json")?;
        normalize_settings(&mut parsed);
        Ok(parsed)
    }

    pub fn save(&self, settings: &StoredSettings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }
        let mut clamped = settings.clone();
        normalize_settings(&mut clamped);
        let json = serde_json::to_string_pretty(&clamped)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("failed writing {}", self.path.display()))?;
        Ok(())
    }
}
