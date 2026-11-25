use crate::video_decoder::MediaCodec;
use std::path::PathBuf;

#[repr(C)]
struct WinSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(lp_system_time: *mut WinSystemTime);
}

/// Iterates over Annex-B NAL unit slices `(start_code_offset, nal_body)` in `data`.
pub fn iter_annex_b_nals(data: &[u8]) -> Vec<(usize, &[u8])> {
    let mut offsets = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..].starts_with(&[0, 0, 0, 1]) {
            offsets.push((i, i + 4));
            i += 4;
        } else if data[i..].starts_with(&[0, 0, 1]) {
            offsets.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }

    let mut out = Vec::with_capacity(offsets.len());
    for (idx, &(sc_start, body_start)) in offsets.iter().enumerate() {
        let end = offsets
            .get(idx + 1)
            .map_or(data.len(), |&(next_sc, _)| next_sc);
        if body_start < end {
            out.push((sc_start, &data[body_start..end]));
        }
    }
    out
}

/// Returns `true` when `data` contains only non-VCL Annex-B parameter/SEI/delimiter NAL units.
pub fn is_header_only_annex_b(data: &[u8], codec: MediaCodec) -> bool {
    let nals = iter_annex_b_nals(data);
    if nals.is_empty() {
        return false;
    }
    for (_, body) in nals {
        let b = body[0];
        let is_header = match codec {
            MediaCodec::H264 => matches!(b & 0x1F, 6 | 7 | 8 | 9 | 13),
            MediaCodec::Hevc | MediaCodec::HevcHdr => {
                matches!((b >> 1) & 0x3F, 32 | 33 | 34 | 35 | 39 | 40)
            }
        };
        if !is_header {
            return false;
        }
    }
    true
}

/// Returns `true` when `data` contains an IDR/CRA keyframe video slice for `codec`.
pub fn contains_keyframe_nal(data: &[u8], codec: MediaCodec) -> bool {
    for (_, body) in iter_annex_b_nals(data) {
        let b = body[0];
        let is_key = match codec {
            MediaCodec::H264 => (b & 0x1F) == 5,
            MediaCodec::Hevc | MediaCodec::HevcHdr => matches!((b >> 1) & 0x3F, 19..=21),
        };
        if is_key {
            return true;
        }
    }
    false
}

/// Extracts H.264 (`SPS`/`PPS`) or HEVC (`VPS`/`SPS`/`PPS`) parameter sets normalized with 4-byte Annex-B start codes.
pub fn extract_parameter_sets(data: &[u8], codec: MediaCodec) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for (_, body) in iter_annex_b_nals(data) {
        let b = body[0];
        let is_param = match codec {
            MediaCodec::H264 => matches!(b & 0x1F, 7 | 8),
            MediaCodec::Hevc | MediaCodec::HevcHdr => matches!((b >> 1) & 0x3F, 32..=34),
        };
        if is_param {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(body);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn user_profile_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Returns the default screenshot folder (`%USERPROFILE%\Pictures\gnsh-play`).
pub fn default_screenshot_dir() -> PathBuf {
    user_profile_dir().join("Pictures").join("gnsh-play")
}

/// Returns the default video recording folder (`%USERPROFILE%\Videos\gnsh-play`).
pub fn default_video_dir() -> PathBuf {
    user_profile_dir().join("Videos").join("gnsh-play")
}

/// Builds a unique local-timestamped capture file path (`<prefix>_YYYYMMDD_HHMMSS_mmm.<ext>`).
pub fn build_capture_path(
    base_dir_override: Option<&str>,
    is_video: bool,
    prefix: &str,
    ext: &str,
) -> PathBuf {
    let dir = base_dir_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if is_video {
                default_video_dir()
            } else {
                default_screenshot_dir()
            }
        });

    let mut st = WinSystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        milliseconds: 0,
    };
    unsafe {
        GetLocalTime(&raw mut st);
    }

    let base_stem = format!(
        "{prefix}_{:04}{:02}{:02}_{:02}{:02}{:02}_{:03}",
        st.year, st.month, st.day, st.hour, st.minute, st.second, st.milliseconds
    );
    let candidate = dir.join(format!("{base_stem}.{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 1u32..1000 {
        let alt = dir.join(format!("{base_stem}_{n}.{ext}"));
        if !alt.exists() {
            return alt;
        }
    }
    candidate
}
