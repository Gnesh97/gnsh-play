use crate::ffi;
use anyhow::{Result, bail};
use std::ffi::CStr;

const SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK: u32 = 0xFFFF_FFFF;
const MAX_AUDIO_LATENCY_MS: u32 = 60;

/// Low-latency PCM16 playback stream backed by SDL3.
pub struct AudioOutput {
    stream: *mut ffi::SDL_AudioStream,
    channels: u32,
    sample_rate: u32,
}

unsafe impl Send for AudioOutput {}

impl AudioOutput {
    pub fn new(channels: u32, sample_rate: u32) -> Result<Self> {
        unsafe {
            if !ffi::SDL_InitSubSystem(ffi::SDL_INIT_AUDIO) {
                bail!("SDL_InitSubSystem(AUDIO) failed: {}", sdl_error());
            }
            let stream = open_stream(channels, sample_rate)?;
            Ok(Self {
                stream,
                channels,
                sample_rate,
            })
        }
    }

    pub fn push_pcm(&mut self, samples: &[i16], channels: u32, sample_rate: u32) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        if channels != self.channels || sample_rate != self.sample_rate {
            unsafe {
                if !self.stream.is_null() {
                    ffi::SDL_DestroyAudioStream(self.stream);
                }
                self.stream = open_stream(channels, sample_rate)?;
            }
            self.channels = channels;
            self.sample_rate = sample_rate;
        }

        let bytes_len = std::mem::size_of_val(samples) as i32;
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
            unsafe {
                ffi::SDL_DestroyAudioStream(self.stream);
            }
            self.stream = std::ptr::null_mut();
        }
    }
}

unsafe fn open_stream(channels: u32, sample_rate: u32) -> Result<*mut ffi::SDL_AudioStream> {
    let spec = ffi::SDL_AudioSpec {
        format: ffi::SDL_AudioFormat_SDL_AUDIO_S16,
        channels: channels as i32,
        freq: sample_rate as i32,
    };
    let stream = unsafe {
        ffi::SDL_OpenAudioDeviceStream(
            SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK,
            &spec,
            None,
            std::ptr::null_mut(),
        )
    };
    if stream.is_null() {
        bail!("SDL_OpenAudioDeviceStream failed: {}", sdl_error());
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
