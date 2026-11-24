use crate::storage::SavedConsole;
use anyhow::{Context as _, Result, bail};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, IShellLinkW, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath, ShellLink,
};
use windows::core::{HSTRING, Interface as _, PWSTR};

const MAX_SHORTCUT_STEM_CHARS: usize = 48;
const MAX_CONSOLE_ID_LEN: usize = 64;

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn is_forbidden_filename_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
}

fn is_reserved_windows_name(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("");
    RESERVED_WINDOWS_NAMES
        .iter()
        .any(|reserved| base.eq_ignore_ascii_case(reserved))
}

/// Sanitizes a console display name into a safe Windows filename stem.
pub(crate) fn sanitize_shortcut_stem(name: &str) -> String {
    let filtered: String = name
        .chars()
        .filter(|&c| !c.is_control() && !is_forbidden_filename_char(c))
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut stem: String = collapsed.chars().take(MAX_SHORTCUT_STEM_CHARS).collect();
    stem = stem
        .trim_matches(|c: char| c == ' ' || c == '.')
        .to_string();
    if stem.is_empty() || is_reserved_windows_name(&stem) {
        "PS5".to_string()
    } else {
        stem
    }
}

/// Validates a console ID and formats direct-launch CLI arguments for a `.lnk` target.
pub(crate) fn build_shortcut_args(
    console_id: &str,
    use_wan: bool,
    fullscreen: bool,
) -> Result<String> {
    if console_id.is_empty() || console_id.len() > MAX_CONSOLE_ID_LEN {
        bail!("console id length must be between 1 and {MAX_CONSOLE_ID_LEN} characters");
    }
    let valid_id = console_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if !valid_id {
        bail!("console id contains unsafe characters for shortcut arguments");
    }

    let mut args = format!("--connect {console_id}");
    if use_wan {
        args.push_str(" --wan");
    }
    if fullscreen {
        args.push_str(" --fullscreen");
    }
    Ok(args)
}

/// Builds the `.lnk` filename for a saved console shortcut.
pub(crate) fn shortcut_filename(console_name: &str, use_wan: bool) -> String {
    let stem = sanitize_shortcut_stem(console_name);
    if use_wan {
        format!("gnsh-play - {stem} (WAN).lnk")
    } else {
        format!("gnsh-play - {stem}.lnk")
    }
}

struct ComGuard {
    should_uninit: bool,
}

impl ComGuard {
    fn acquire() -> Result<Self> {
        // SAFETY: Passing null reserved pointer initializes COM apartment on the calling thread.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self {
                should_uninit: true,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Self {
                should_uninit: false,
            })
        } else {
            hr.ok().context("CoInitializeEx failed")?;
            Ok(Self {
                should_uninit: false,
            })
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            // SAFETY: Matches a preceding S_OK/S_FALSE CoInitializeEx call on this thread.
            unsafe {
                CoUninitialize();
            }
        }
    }
}

struct CoTaskMemPwstr(PWSTR);

impl Drop for CoTaskMemPwstr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Pointer was allocated by SHGetKnownFolderPath via COM task allocator.
            unsafe {
                CoTaskMemFree(Some(self.0.0.cast()));
            }
        }
    }
}

fn resolve_desktop_dir() -> Result<PathBuf> {
    let raw = unsafe { SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None) }
        .context("SHGetKnownFolderPath(FOLDERID_Desktop) failed")?;
    let guard = CoTaskMemPwstr(raw);
    let path_str = unsafe { guard.0.to_string() }.context("Desktop path is not valid UTF-16")?;
    if path_str.is_empty() {
        bail!("Resolved Windows Desktop path is empty");
    }
    Ok(PathBuf::from(path_str))
}

fn write_lnk_file(
    lnk_path: &Path,
    exe_path: &Path,
    work_dir: Option<&Path>,
    args: &str,
    description: &str,
) -> Result<()> {
    let _com = ComGuard::acquire()?;

    // SAFETY: ShellLink is a standard in-proc COM class returning IShellLinkW on Windows.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .context("failed creating IShellLinkW COM instance")?;

    let exe_hstr = HSTRING::from(exe_path.as_os_str());
    let args_hstr = HSTRING::from(args);
    let desc_hstr = HSTRING::from(description);
    let lnk_hstr = HSTRING::from(lnk_path.as_os_str());

    unsafe {
        link.SetPath(&exe_hstr)
            .context("IShellLinkW::SetPath failed")?;
        link.SetArguments(&args_hstr)
            .context("IShellLinkW::SetArguments failed")?;
        if let Some(dir) = work_dir {
            let dir_hstr = HSTRING::from(dir.as_os_str());
            link.SetWorkingDirectory(&dir_hstr)
                .context("IShellLinkW::SetWorkingDirectory failed")?;
        }
        link.SetDescription(&desc_hstr)
            .context("IShellLinkW::SetDescription failed")?;
        link.SetIconLocation(&exe_hstr, 0)
            .context("IShellLinkW::SetIconLocation failed")?;
    }

    let persist: IPersistFile = link
        .cast()
        .context("IShellLinkW does not support IPersistFile")?;
    unsafe { persist.Save(&lnk_hstr, true) }.context("IPersistFile::Save failed")?;
    Ok(())
}

/// Creates a Windows Desktop `.lnk` shortcut that directly launches fullscreen streaming.
pub(crate) fn create_desktop_shortcut(console: &SavedConsole, use_wan: bool) -> Result<PathBuf> {
    if use_wan && console.remote_host.is_none() {
        bail!("No Remote / WAN host configured for this console");
    }
    let args = build_shortcut_args(&console.id, use_wan, true)?;
    let exe_path = std::env::current_exe().context("failed resolving current executable path")?;
    let work_dir = exe_path.parent();
    let desktop_dir = resolve_desktop_dir()?;
    let lnk_path = desktop_dir.join(shortcut_filename(&console.name, use_wan));
    let description = format!(
        "Stream {} via gnsh-play",
        sanitize_shortcut_stem(&console.name)
    );

    write_lnk_file(&lnk_path, &exe_path, work_dir, &args, &description)?;
    Ok(lnk_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn sanitizes_shortcut_filenames_and_rejects_reserved_device_names() {
        assert_eq!(
            sanitize_shortcut_stem("  PS5: Living/Room* "),
            "PS5 LivingRoom"
        );
        assert_eq!(sanitize_shortcut_stem("...con..."), "PS5");
        assert_eq!(sanitize_shortcut_stem("COM1"), "PS5");
        assert_eq!(sanitize_shortcut_stem("LPT9.lnk"), "PS5");
        assert_eq!(sanitize_shortcut_stem("   <>|?   "), "PS5");
        assert_eq!(sanitize_shortcut_stem("a\x07b\nc"), "abc");

        let long = "X".repeat(80);
        assert_eq!(sanitize_shortcut_stem(&long).len(), MAX_SHORTCUT_STEM_CHARS);

        assert_eq!(
            shortcut_filename("PS5 Pro", false),
            "gnsh-play - PS5 Pro.lnk"
        );
        assert_eq!(
            shortcut_filename("PS5 Pro", true),
            "gnsh-play - PS5 Pro (WAN).lnk"
        );
    }

    #[test]
    fn validates_and_builds_safe_shortcut_arguments() {
        assert_eq!(
            build_shortcut_args("001122334455", false, true).expect("valid hex id"),
            "--connect 001122334455 --fullscreen"
        );
        assert_eq!(
            build_shortcut_args("ps5-01:aa.bb_cc", true, true).expect("valid id chars"),
            "--connect ps5-01:aa.bb_cc --wan --fullscreen"
        );
        assert_eq!(
            build_shortcut_args("001122334455", false, false).expect("valid id no flags"),
            "--connect 001122334455"
        );

        assert!(build_shortcut_args("", false, true).is_err());
        assert!(build_shortcut_args("0011 2233", false, true).is_err());
        assert!(build_shortcut_args("0011\"2233", false, true).is_err());
        assert!(build_shortcut_args("0011&calc", false, true).is_err());
        assert!(build_shortcut_args("0011\ncalc", false, true).is_err());
        assert!(build_shortcut_args(&"a".repeat(65), false, true).is_err());
    }

    #[test]
    fn write_lnk_file_writes_valid_windows_shell_link_header() {
        let exe = std::env::current_exe().expect("resolve test executable");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock monotonic after epoch")
            .as_nanos();
        let lnk_path =
            std::env::temp_dir().join(format!("gnsh-play-test-{}-{stamp}.lnk", std::process::id()));

        let res = write_lnk_file(
            &lnk_path,
            &exe,
            exe.parent(),
            "--connect 001122334455 --fullscreen",
            "gnsh-play shortcut test",
        );
        assert!(res.is_ok(), "write_lnk_file failed: {res:?}");

        let bytes = std::fs::read(&lnk_path).expect("read generated .lnk file");
        let _ = std::fs::remove_file(&lnk_path);
        assert!(bytes.len() >= 76, "lnk file unexpectedly short");
        // Windows Shell Link (.lnk) binary header magic (HeaderSize = 0x0000004C little-endian).
        assert_eq!(&bytes[..4], &[0x4C, 0x00, 0x00, 0x00]);
    }
}
