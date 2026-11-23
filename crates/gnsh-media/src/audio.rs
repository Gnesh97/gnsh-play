use crate::ffi;
use anyhow::{Context as _, Result, bail};
use std::ffi::CStr;
use std::os::raw::c_int;

const SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK: u32 = 0xFFFF_FFFF;
const SDL_AUDIO_DEVICE_DEFAULT_RECORDING: u32 = 0xFFFF_FFFE;
const MAX_AUDIO_LATENCY_MS: u32 = 30;
const HAPTICS_SAMPLE_RATE: u32 = 3_000;
const HAPTICS_CHANNELS: u32 = 4;
const HAPTICS_MAX_LATENCY_MS: u32 = 40;
const HAPTICS_MAX_QUEUED_BYTES: i32 =
    ((HAPTICS_SAMPLE_RATE * HAPTICS_CHANNELS * 2 * HAPTICS_MAX_LATENCY_MS) / 1000) as i32;
pub const MIC_FRAME_SAMPLES_STEREO: usize = 960;
const MIC_FRAME_BYTES: usize = MIC_FRAME_SAMPLES_STEREO * std::mem::size_of::<i16>();
const MIC_MAX_BACKLOG_BYTES: i32 = (MIC_FRAME_BYTES * 6) as i32;

/// Returns `true` when an SDL audio endpoint name matches a Sony DualSense USB controller.
pub fn is_dualsense_haptics_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("dualsense") || lower.contains("wireless controller")
}

/// Remixes stereo 3 kHz haptic PCM (`[L, R, ...]`) into 4-channel `[0, 0, L, R, ...]` where rear
/// channels 2 and 3 drive the DualSense USB voice-coil actuators while leaving headset silent.
pub fn remix_stereo_to_dualsense_4ch(stereo_s16: &[i16], out: &mut Vec<i16>) {
    out.clear();
    let pairs = stereo_s16.as_chunks::<2>().0;
    out.reserve(pairs.len() * 4);
    for &[left, right] in pairs {
        out.extend_from_slice(&[0, 0, left, right]);
    }
}

/// Converts a `0..=100` integer volume percentage into an SDL3 linear stream gain (`0.0..=1.0`).
pub fn volume_pct_to_gain(volume_pct: u8) -> f32 {
    f32::from(volume_pct.min(100)) / 100.0
}

fn normalize_device_name(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Lists available SDL3 playback device names in system presentation order.
pub fn list_playback_devices() -> Vec<String> {
    if let Err(err) = ensure_sdl_audio_init() {
        tracing::debug!("SDL audio init failed during playback device enumeration: {err:#}");
        return Vec::new();
    }

    let mut count: c_int = 0;
    // SAFETY: count is a valid stack i32 pointer; SDL allocates the returned device ID array.
    let ids = unsafe { ffi::SDL_GetAudioPlaybackDevices(&mut count) };
    if ids.is_null() {
        return Vec::new();
    }

    let mut devices = Vec::new();
    if count > 0 {
        // SAFETY: ids is non-null and holds `count` elements reported by SDL_GetAudioPlaybackDevices.
        let slice = unsafe { std::slice::from_raw_parts(ids, count as usize) };
        for &dev_id in slice {
            let ptr = unsafe { ffi::SDL_GetAudioDeviceName(dev_id) };
            if ptr.is_null() {
                continue;
            }
            let name = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .trim()
                .to_string();
            if !name.is_empty() && !devices.contains(&name) {
                devices.push(name);
            }
        }
    }

    // SAFETY: ids was allocated by SDL_GetAudioPlaybackDevices and must always be freed.
    unsafe { ffi::SDL_free(ids.cast()) };
    devices
}

/// Low-latency PCM16 playback stream backed by SDL3.
pub struct AudioOutput {
    stream: *mut ffi::SDL_AudioStream,
    channels: u32,
    sample_rate: u32,
    device_name: Option<String>,
    volume: f32,
}

unsafe impl Send for AudioOutput {}

impl AudioOutput {
    pub fn new(channels: u32, sample_rate: u32) -> Result<Self> {
        Self::new_for_device(channels, sample_rate, None, 1.0)
    }

    /// Opens an SDL3 playback stream targeting `device_name` (or system default if `None`) with `volume`.
    pub fn new_for_device(
        channels: u32,
        sample_rate: u32,
        device_name: Option<&str>,
        volume: f32,
    ) -> Result<Self> {
        ensure_sdl_audio_init()?;
        let normalized = normalize_device_name(device_name);
        let gain = volume.clamp(0.0, 1.0);
        let stream =
            unsafe { open_playback_stream(channels, sample_rate, normalized.as_deref(), gain)? };
        Ok(Self {
            stream,
            channels,
            sample_rate,
            device_name: normalized,
            volume: gain,
        })
    }

    /// Updates live playback gain (`0.0..=1.0`) without recreating the audio stream.
    pub fn set_volume(&mut self, volume: f32) {
        let gain = volume.clamp(0.0, 1.0);
        self.volume = gain;
        if !self.stream.is_null()
            // SAFETY: self.stream is a valid playback SDL_AudioStream* owned by self.
            && unsafe { !ffi::SDL_SetAudioStreamGain(self.stream, gain) }
        {
            tracing::debug!("SDL_SetAudioStreamGain failed: {}", sdl_error());
        }
    }

    /// Switches playback output to `device_name` (or system default if `None`) preserving volume.
    pub fn switch_device(&mut self, device_name: Option<&str>) -> Result<()> {
        let normalized = normalize_device_name(device_name);
        let next_stream = unsafe {
            open_playback_stream(
                self.channels,
                self.sample_rate,
                normalized.as_deref(),
                self.volume,
            )?
        };
        let old = std::mem::replace(&mut self.stream, next_stream);
        if !old.is_null() {
            // SAFETY: `old` was replaced above and is uniquely owned by self.
            unsafe {
                ffi::SDL_DestroyAudioStream(old);
            }
        }
        self.device_name = normalized;
        Ok(())
    }

    pub fn push_pcm(&mut self, samples: &[i16], channels: u32, sample_rate: u32) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        validate_audio_params(channels, sample_rate)?;
        if self.stream.is_null() || channels != self.channels || sample_rate != self.sample_rate {
            let next_stream = unsafe {
                open_playback_stream(
                    channels,
                    sample_rate,
                    self.device_name.as_deref(),
                    self.volume,
                )?
            };
            let old = std::mem::replace(&mut self.stream, next_stream);
            if !old.is_null() {
                unsafe {
                    ffi::SDL_DestroyAudioStream(old);
                }
            }
            self.channels = channels;
            self.sample_rate = sample_rate;
        }

        let bytes_len = i32::try_from(std::mem::size_of_val(samples))
            .context("audio PCM buffer exceeds i32::MAX bytes")?;
        let max_queued = (self.sample_rate as usize
            * self.channels as usize
            * std::mem::size_of::<i16>()
            * MAX_AUDIO_LATENCY_MS as usize)
            / 1000;

        unsafe {
            let queued = ffi::SDL_GetAudioStreamQueued(self.stream);
            if queued > max_queued as i32 {
                let _ = ffi::SDL_ClearAudioStream(self.stream);
            }
            if !ffi::SDL_PutAudioStreamData(self.stream, samples.as_ptr().cast(), bytes_len) {
                bail!("SDL_PutAudioStreamData failed: {}", sdl_error());
            }
        }
        Ok(())
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: self.stream was created by SDL_OpenAudioDeviceStream and owned uniquely.
            unsafe {
                ffi::SDL_DestroyAudioStream(self.stream);
            }
            self.stream = std::ptr::null_mut();
        }
    }
}

/// 4-channel USB DualSense voice-coil haptics output stream fed with 3 kHz stereo PCM.
pub struct HapticsAudioOutput {
    stream: *mut ffi::SDL_AudioStream,
    device_name: String,
    scratch_4ch: Vec<i16>,
}

unsafe impl Send for HapticsAudioOutput {}

impl HapticsAudioOutput {
    /// Scans SDL3 playback endpoints for a 4-channel DualSense USB audio device and opens it.
    pub fn try_open_dualsense() -> Option<Self> {
        if let Err(err) = ensure_sdl_audio_init() {
            tracing::debug!("SDL audio init failed during haptics probe: {err:#}");
            return None;
        }

        let mut count: c_int = 0;
        // SAFETY: count is a valid stack i32 pointer; SDL allocates the returned array.
        let ids = unsafe { ffi::SDL_GetAudioPlaybackDevices(&mut count) };
        if ids.is_null() {
            return None;
        }

        let mut opened: Option<Self> = None;
        if count > 0 {
            // SAFETY: ids is non-null and holds count elements as reported by SDL_GetAudioPlaybackDevices.
            let slice = unsafe { std::slice::from_raw_parts(ids, count as usize) };
            let src_spec = ffi::SDL_AudioSpec {
                format: ffi::SDL_AudioFormat_SDL_AUDIO_S16,
                channels: HAPTICS_CHANNELS as i32,
                freq: HAPTICS_SAMPLE_RATE as i32,
            };

            for &dev_id in slice {
                // SAFETY: dev_id comes from SDL_GetAudioPlaybackDevices for the active audio session.
                let name_ptr = unsafe { ffi::SDL_GetAudioDeviceName(dev_id) };
                if name_ptr.is_null() {
                    continue;
                }
                let name = unsafe { CStr::from_ptr(name_ptr) }
                    .to_string_lossy()
                    .into_owned();
                if !is_dualsense_haptics_device_name(&name) {
                    continue;
                }

                let mut dev_spec = ffi::SDL_AudioSpec::default();
                // SAFETY: dev_spec is stack-allocated and sample_frames output is optional (null).
                let has_fmt = unsafe {
                    ffi::SDL_GetAudioDeviceFormat(dev_id, &mut dev_spec, std::ptr::null_mut())
                };
                if !has_fmt || dev_spec.channels < HAPTICS_CHANNELS as i32 {
                    tracing::debug!(
                        device = %name,
                        has_fmt,
                        channels = dev_spec.channels,
                        "Skipping non-4ch DualSense audio endpoint"
                    );
                    continue;
                }

                // SAFETY: src_spec specifies 4ch 3 kHz S16 input; SDL3 handles resampling to device format.
                let stream = unsafe {
                    ffi::SDL_OpenAudioDeviceStream(dev_id, &src_spec, None, std::ptr::null_mut())
                };
                if stream.is_null() {
                    tracing::warn!(
                        device = %name,
                        "SDL_OpenAudioDeviceStream failed for DualSense haptics: {}",
                        sdl_error()
                    );
                    continue;
                }
                // SAFETY: stream was just returned non-null by SDL_OpenAudioDeviceStream.
                if !unsafe { ffi::SDL_ResumeAudioStreamDevice(stream) } {
                    tracing::warn!(
                        device = %name,
                        "SDL_ResumeAudioStreamDevice failed for DualSense haptics: {}",
                        sdl_error()
                    );
                    unsafe { ffi::SDL_DestroyAudioStream(stream) };
                    continue;
                }

                tracing::info!(
                    device = %name,
                    device_channels = dev_spec.channels,
                    device_freq = dev_spec.freq,
                    "Opened DualSense 4-channel USB haptics audio output"
                );
                opened = Some(Self {
                    stream,
                    device_name: name,
                    scratch_4ch: Vec::with_capacity(256),
                });
                break;
            }
        }

        // SAFETY: ids was allocated by SDL_GetAudioPlaybackDevices and must always be freed.
        unsafe { ffi::SDL_free(ids.cast()) };
        opened
    }

    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Remixes 3 kHz stereo haptic PCM into 4-channel actuator audio and queues it for playback.
    pub fn push_stereo_3khz(&mut self, stereo_samples: &[i16]) -> Result<()> {
        if stereo_samples.len() < 2 {
            return Ok(());
        }
        remix_stereo_to_dualsense_4ch(stereo_samples, &mut self.scratch_4ch);
        if self.scratch_4ch.is_empty() {
            return Ok(());
        }

        let bytes_len = i32::try_from(std::mem::size_of_val(self.scratch_4ch.as_slice()))
            .context("haptics PCM buffer exceeds i32::MAX bytes")?;
        // SAFETY: self.stream is a valid open SDL_AudioStream* owned by self, and scratch_4ch
        // provides initialized S16 bytes for the duration of SDL_PutAudioStreamData.
        unsafe {
            let queued = ffi::SDL_GetAudioStreamQueued(self.stream);
            if queued > HAPTICS_MAX_QUEUED_BYTES {
                let _ = ffi::SDL_ClearAudioStream(self.stream);
            }
            if !ffi::SDL_PutAudioStreamData(
                self.stream,
                self.scratch_4ch.as_ptr().cast(),
                bytes_len,
            ) {
                bail!("SDL_PutAudioStreamData(haptics) failed: {}", sdl_error());
            }
        }
        Ok(())
    }
}

impl Drop for HapticsAudioOutput {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: self.stream was created by SDL_OpenAudioDeviceStream and owned uniquely.
            unsafe {
                ffi::SDL_DestroyAudioStream(self.stream);
            }
            self.stream = std::ptr::null_mut();
        }
    }
}

/// Low-latency 48 kHz stereo PCM16 recording capture stream backed by SDL3.
pub struct MicCapture {
    stream: *mut ffi::SDL_AudioStream,
    muted: bool,
}

unsafe impl Send for MicCapture {}

impl MicCapture {
    /// Opens the system default recording device in 48 kHz stereo S16 format, initially paused.
    pub fn new_default() -> Result<Self> {
        ensure_sdl_audio_init()?;
        let dst_spec = ffi::SDL_AudioSpec {
            format: ffi::SDL_AudioFormat_SDL_AUDIO_S16,
            channels: 2,
            freq: 48_000,
        };
        // SAFETY: SDL_AUDIO_DEVICE_DEFAULT_RECORDING opens the OS default capture device with dst_spec output.
        let stream = unsafe {
            ffi::SDL_OpenAudioDeviceStream(
                SDL_AUDIO_DEVICE_DEFAULT_RECORDING,
                &dst_spec,
                None,
                std::ptr::null_mut(),
            )
        };
        if stream.is_null() {
            bail!(
                "SDL_OpenAudioDeviceStream(default mic) failed: {}",
                sdl_error()
            );
        }
        Ok(Self {
            stream,
            muted: true,
        })
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Pauses/clears or unpauses capture on the default recording device.
    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        if self.stream.is_null() {
            bail!("mic capture stream is not initialized");
        }
        // SAFETY: self.stream is a valid recording SDL_AudioStream* owned by self.
        unsafe {
            let _ = ffi::SDL_ClearAudioStream(self.stream);
            if muted {
                if !ffi::SDL_PauseAudioStreamDevice(self.stream) {
                    bail!("SDL_PauseAudioStreamDevice(mic) failed: {}", sdl_error());
                }
            } else if !ffi::SDL_ResumeAudioStreamDevice(self.stream) {
                bail!("SDL_ResumeAudioStreamDevice(mic) failed: {}", sdl_error());
            }
        }
        self.muted = muted;
        Ok(())
    }

    /// Drains up to 4 captured 10 ms frames (960 stereo `i16` samples each) into `on_frame`.
    pub fn drain_frames<F: FnMut(&mut [i16; MIC_FRAME_SAMPLES_STEREO])>(
        &mut self,
        mut on_frame: F,
    ) {
        if self.muted || self.stream.is_null() {
            return;
        }
        // SAFETY: self.stream is a valid SDL_AudioStream* and pcm buffer length matches MIC_FRAME_BYTES.
        unsafe {
            let available = ffi::SDL_GetAudioStreamAvailable(self.stream);
            if available < 0 {
                return;
            }
            if available > MIC_MAX_BACKLOG_BYTES {
                let _ = ffi::SDL_ClearAudioStream(self.stream);
                return;
            }
            for _ in 0..4 {
                if ffi::SDL_GetAudioStreamAvailable(self.stream) < MIC_FRAME_BYTES as i32 {
                    break;
                }
                let mut pcm = [0i16; MIC_FRAME_SAMPLES_STEREO];
                let read = ffi::SDL_GetAudioStreamData(
                    self.stream,
                    pcm.as_mut_ptr().cast(),
                    MIC_FRAME_BYTES as i32,
                );
                if read == MIC_FRAME_BYTES as i32 {
                    on_frame(&mut pcm);
                } else {
                    break;
                }
            }
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        if !self.stream.is_null() {
            // SAFETY: self.stream was created by SDL_OpenAudioDeviceStream and owned uniquely.
            unsafe {
                ffi::SDL_DestroyAudioStream(self.stream);
            }
            self.stream = std::ptr::null_mut();
        }
    }
}

fn ensure_sdl_audio_init() -> Result<()> {
    // SAFETY: SDL_InitSubSystem is thread-safe/refcounted in SDL3 for SDL_INIT_AUDIO.
    if unsafe { !ffi::SDL_InitSubSystem(ffi::SDL_INIT_AUDIO) } {
        bail!("SDL_InitSubSystem(AUDIO) failed: {}", sdl_error());
    }
    Ok(())
}

fn find_playback_device_id(target_name: &str) -> Option<u32> {
    let mut count: c_int = 0;
    let ids = unsafe { ffi::SDL_GetAudioPlaybackDevices(&mut count) };
    if ids.is_null() {
        return None;
    }

    let mut matched = None;
    if count > 0 {
        let slice = unsafe { std::slice::from_raw_parts(ids, count as usize) };
        for &dev_id in slice {
            let ptr = unsafe { ffi::SDL_GetAudioDeviceName(dev_id) };
            if ptr.is_null() {
                continue;
            }
            let name = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
            if name.trim() == target_name {
                matched = Some(dev_id);
                break;
            }
        }
    }

    unsafe { ffi::SDL_free(ids.cast()) };
    matched
}

fn validate_audio_params(channels: u32, sample_rate: u32) -> Result<(i32, i32)> {
    if !(1..=8).contains(&channels) {
        bail!("audio channel count {channels} out of valid 1..=8 range");
    }
    if !(8_000..=192_000).contains(&sample_rate) {
        bail!("audio sample rate {sample_rate} Hz out of valid 8000..=192000 range");
    }
    Ok((channels as i32, sample_rate as i32))
}

unsafe fn open_playback_stream(
    channels: u32,
    sample_rate: u32,
    device_name: Option<&str>,
    volume: f32,
) -> Result<*mut ffi::SDL_AudioStream> {
    let (valid_channels, valid_rate) = validate_audio_params(channels, sample_rate)?;
    let dev_id = match normalize_device_name(device_name) {
        Some(name) => match find_playback_device_id(&name) {
            Some(id) => id,
            None => {
                tracing::warn!(
                    device = %name,
                    "Selected playback device unavailable; falling back to system default"
                );
                SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK
            }
        },
        None => SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
    };
    let spec = ffi::SDL_AudioSpec {
        format: ffi::SDL_AudioFormat_SDL_AUDIO_S16,
        channels: valid_channels,
        freq: valid_rate,
    };
    let stream =
        unsafe { ffi::SDL_OpenAudioDeviceStream(dev_id, &spec, None, std::ptr::null_mut()) };
    if stream.is_null() {
        bail!("SDL_OpenAudioDeviceStream failed: {}", sdl_error());
    }
    if unsafe { !ffi::SDL_SetAudioStreamGain(stream, volume.clamp(0.0, 1.0)) } {
        tracing::warn!("SDL_SetAudioStreamGain failed on open: {}", sdl_error());
    }
    if !unsafe { ffi::SDL_ResumeAudioStreamDevice(stream) } {
        unsafe { ffi::SDL_DestroyAudioStream(stream) };
        bail!("SDL_ResumeAudioStreamDevice failed: {}", sdl_error());
    }
    Ok(stream)
}

fn sdl_error() -> String {
    let ptr = unsafe { ffi::SDL_GetError() };
    if ptr.is_null() {
        "unknown SDL error".to_string()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dualsense_haptics_device_name_matching() {
        assert!(is_dualsense_haptics_device_name(
            "Speakers (DualSense Wireless Controller)"
        ));
        assert!(is_dualsense_haptics_device_name(
            "Headset Earphone (DualSense Edge Wireless Controller)"
        ));
        assert!(is_dualsense_haptics_device_name("Wireless Controller"));
        assert!(!is_dualsense_haptics_device_name(
            "Speakers (Realtek High Definition Audio)"
        ));
        assert!(!is_dualsense_haptics_device_name(
            "NVIDIA High Definition Audio"
        ));
    }

    #[test]
    fn remix_stereo_to_dualsense_4ch_maps_to_rear_channels() {
        let mut out = vec![999];
        remix_stereo_to_dualsense_4ch(&[120, -340, 560, 780, 999], &mut out);
        assert_eq!(out, vec![0, 0, 120, -340, 0, 0, 560, 780]);
    }

    #[test]
    fn volume_pct_clamps_to_unit_gain_and_device_list_runs() {
        assert!((volume_pct_to_gain(0) - 0.0).abs() < 1e-6);
        assert!((volume_pct_to_gain(50) - 0.5).abs() < 1e-6);
        assert!((volume_pct_to_gain(100) - 1.0).abs() < 1e-6);
        assert!((volume_pct_to_gain(250) - 1.0).abs() < 1e-6);

        let devices = list_playback_devices();
        assert!(devices.iter().all(|name| !name.is_empty()));
    }

    #[test]
    fn rejects_out_of_range_audio_stream_parameters() {
        assert_eq!(validate_audio_params(2, 48_000).unwrap(), (2, 48_000));
        assert!(validate_audio_params(0, 48_000).is_err());
        assert!(validate_audio_params(9, 48_000).is_err());
        assert!(validate_audio_params(2, 4_000).is_err());
        assert!(validate_audio_params(2, 384_000).is_err());
        assert!(unsafe { open_playback_stream(0, 48_000, None, 1.0) }.is_err());
        assert!(unsafe { open_playback_stream(2, 1_000, None, 1.0) }.is_err());
    }
}
