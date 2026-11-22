use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gnsh_proto::{VideoCodec, VideoPreset, clamp_bitrate_kbps};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConsole {
    pub id: String,
    pub name: String,
    pub host: String,
    pub mac: [u8; 6],
    pub psn_account_id: String,
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
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
    mac: [u8; 6],
    psn_account_id_dpapi: String,
    regist_key_dpapi: String,
    rp_key_dpapi: String,
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
        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("failed reading {}", self.path.display()))?;
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
            out.push(SavedConsole {
                id: entry.id,
                name: entry.name,
                host: entry.host,
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
            disk.push(StoredConsoleDisk {
                id: c.id.clone(),
                name: c.name.clone(),
                host: c.host.clone(),
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
            mac: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        };

        store.save(std::slice::from_ref(&sample)).expect("save");
        let raw = std::fs::read_to_string(&path).expect("read json");
        assert!(!raw.contains("1a2b3c4d"));
        assert!(!raw.contains("AQIDBAUGBwg="));

        let loaded = store.load().expect("load");
        assert_eq!(loaded, vec![sample]);
        assert_eq!(loaded[0].regist_key_hex(), "1a2b3c4d");

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

        let custom = StoredSettings {
            preset: VideoPreset::P720Fps60,
            codec: VideoCodec::H264,
            bitrate_kbps: 500_000,
            vsync: false,
            touchpad_touch: true,
        };
        store.save(&custom).expect("save custom");
        let loaded = store.load().expect("load custom");
        assert_eq!(loaded.preset, VideoPreset::P720Fps60);
        assert_eq!(loaded.codec, VideoCodec::H264);
        assert_eq!(loaded.bitrate_kbps, 100_000);
        assert!(!loaded.vsync);
        assert!(loaded.touchpad_touch);

        std::fs::write(&path, r#"{"bitrate_kbps": 999999}"#).expect("write unclamped json");
        let clamped = store.load().expect("load clamped");
        assert_eq!(clamped.bitrate_kbps, 100_000);
        assert_eq!(clamped.preset, VideoPreset::P1080Fps60);
        assert_eq!(clamped.codec, VideoCodec::Hevc);

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
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            preset: VideoPreset::P1080Fps60,
            codec: VideoCodec::Hevc,
            bitrate_kbps: 30_000,
            vsync: true,
            touchpad_touch: false,
        }
    }
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
        let raw = std::fs::read_to_string(&self.path)
            .with_context(|| format!("failed reading {}", self.path.display()))?;
        let mut parsed: StoredSettings =
            serde_json::from_str(&raw).context("failed parsing settings.json")?;
        parsed.bitrate_kbps = clamp_bitrate_kbps(parsed.bitrate_kbps);
        Ok(parsed)
    }

    pub fn save(&self, settings: &StoredSettings) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }
        let mut clamped = settings.clone();
        clamped.bitrate_kbps = clamp_bitrate_kbps(clamped.bitrate_kbps);
        let json = serde_json::to_string_pretty(&clamped)?;
        std::fs::write(&self.path, json)
            .with_context(|| format!("failed writing {}", self.path.display()))?;
        Ok(())
    }
}
