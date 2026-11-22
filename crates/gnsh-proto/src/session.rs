use crate::controller::ControllerState;
use crate::log::ChiakiLogger;
use crate::{ProtoError, Result, check_chiaki, init_lib};
use crossbeam_channel::{Receiver, Sender, TrySendError};
use gnsh_chiaki_sys::{
    ChiakiAudioSink, ChiakiCodec_CHIAKI_CODEC_H264, ChiakiCodec_CHIAKI_CODEC_H265,
    ChiakiConnectInfo, ChiakiConnectVideoProfile, ChiakiEvent,
    ChiakiEventType_CHIAKI_EVENT_CONNECTED, ChiakiEventType_CHIAKI_EVENT_LED_COLOR,
    ChiakiEventType_CHIAKI_EVENT_LOGIN_PIN_REQUEST, ChiakiEventType_CHIAKI_EVENT_NICKNAME_RECEIVED,
    ChiakiEventType_CHIAKI_EVENT_QUIT, ChiakiEventType_CHIAKI_EVENT_RUMBLE,
    ChiakiEventType_CHIAKI_EVENT_TRIGGER_EFFECTS, ChiakiOpusDecoder,
    ChiakiQuitReason_CHIAKI_QUIT_REASON_STOPPED,
    ChiakiQuitReason_CHIAKI_QUIT_REASON_STREAM_CONNECTION_REMOTE_SHUTDOWN, ChiakiSession,
    ChiakiVideoFPSPreset_CHIAKI_VIDEO_FPS_PRESET_60,
    ChiakiVideoResolutionPreset_CHIAKI_VIDEO_RESOLUTION_PRESET_720p,
    ChiakiVideoResolutionPreset_CHIAKI_VIDEO_RESOLUTION_PRESET_1080p,
    chiaki_connect_video_profile_preset, chiaki_opus_decoder_fini, chiaki_opus_decoder_get_sink,
    chiaki_opus_decoder_init, chiaki_quit_reason_string, chiaki_session_fini, chiaki_session_init,
    chiaki_session_join, chiaki_session_request_idr, chiaki_session_set_controller_state,
    chiaki_session_set_login_pin, chiaki_session_start, chiaki_session_stop,
};
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

pub const MIN_BITRATE_KBPS: u32 = 2_000;
pub const MAX_BITRATE_KBPS: u32 = 100_000;

/// Clamps a requested stream bitrate cleanly within PS5 Remote Play FEC bounds.
pub fn clamp_bitrate_kbps(kbps: u32) -> u32 {
    kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoPreset {
    P720Fps60,
    P1080Fps60,
}

#[derive(Debug, Clone)]
pub struct Ps5SessionConfig {
    pub host: String,
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
    pub preset: VideoPreset,
    pub codec: VideoCodec,
    pub bitrate_kbps: u32,
    pub enable_dualsense: bool,
}

#[derive(Debug, Clone)]
pub struct VideoPacket {
    pub data: Vec<u8>,
    pub frames_lost: i32,
    pub recovered: bool,
}

#[derive(Debug, Clone)]
pub struct AudioPcmFrame {
    pub samples: Vec<i16>,
    pub channels: u32,
    pub sample_rate: u32,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected,
    Quit {
        reason: String,
        is_error: bool,
    },
    Rumble {
        left: u8,
        right: u8,
    },
    Led {
        r: u8,
        g: u8,
        b: u8,
    },
    TriggerEffects {
        type_left: u8,
        type_right: u8,
        left: [u8; 10],
        right: [u8; 10],
    },
    Nickname(String),
    LoginPinRequested {
        incorrect: bool,
    },
}

pub struct SessionChannels {
    pub video_rx: Receiver<VideoPacket>,
    pub audio_rx: Receiver<AudioPcmFrame>,
    pub events_rx: Receiver<SessionEvent>,
}

struct SessionCallbacksCtx {
    video_tx: Sender<VideoPacket>,
    audio_tx: Sender<AudioPcmFrame>,
    event_tx: Sender<SessionEvent>,
    audio_channels: AtomicU32,
    audio_rate: AtomicU32,
}

pub struct Ps5Session {
    session: Box<ChiakiSession>,
    opus_decoder: Box<ChiakiOpusDecoder>,
    _logger: ChiakiLogger,
    ctx: *mut SessionCallbacksCtx,
    started: bool,
}

unsafe impl Send for Ps5Session {}

impl Ps5Session {
    pub fn start(config: Ps5SessionConfig) -> Result<(Self, SessionChannels)> {
        init_lib()?;
        let c_host = CString::new(config.host.trim())
            .map_err(|_| ProtoError::InvalidArg("host contains interior NUL".into()))?;

        let mut logger = ChiakiLogger::new(false);

        let (video_tx, video_rx) = crossbeam_channel::bounded(8);
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(32);
        let (event_tx, events_rx) = crossbeam_channel::bounded(64);

        let ctx_ptr = Box::into_raw(Box::new(SessionCallbacksCtx {
            video_tx,
            audio_tx,
            event_tx,
            audio_channels: AtomicU32::new(2),
            audio_rate: AtomicU32::new(48_000),
        }));

        let mut video_profile = ChiakiConnectVideoProfile::default();
        let res_preset = match config.preset {
            VideoPreset::P720Fps60 => {
                ChiakiVideoResolutionPreset_CHIAKI_VIDEO_RESOLUTION_PRESET_720p
            }
            VideoPreset::P1080Fps60 => {
                ChiakiVideoResolutionPreset_CHIAKI_VIDEO_RESOLUTION_PRESET_1080p
            }
        };
        unsafe {
            chiaki_connect_video_profile_preset(
                &mut video_profile,
                res_preset,
                ChiakiVideoFPSPreset_CHIAKI_VIDEO_FPS_PRESET_60,
            );
        }
        video_profile.codec = match config.codec {
            VideoCodec::H264 => ChiakiCodec_CHIAKI_CODEC_H264,
            VideoCodec::Hevc => ChiakiCodec_CHIAKI_CODEC_H265,
        };
        if config.bitrate_kbps > 0 {
            video_profile.bitrate = clamp_bitrate_kbps(config.bitrate_kbps);
        }

        let mut regist_key_c = [0 as std::os::raw::c_char; 16];
        for (dst, &src) in regist_key_c.iter_mut().zip(config.regist_key.iter()) {
            *dst = src as std::os::raw::c_char;
        }

        let mut connect_info = ChiakiConnectInfo {
            ps5: true,
            host: c_host.as_ptr(),
            regist_key: regist_key_c,
            morning: config.rp_key,
            video_profile,
            video_profile_auto_downgrade: true,
            enable_keyboard: false,
            enable_dualsense: config.enable_dualsense,
            packet_loss_max: 0.05,
            enable_idr_on_fec_failure: true,
            ..Default::default()
        };

        let mut session = Box::new(ChiakiSession::default());
        let init_err = unsafe {
            chiaki_session_init(session.as_mut(), &mut connect_info, logger.as_mut_ptr())
        };
        if init_err != gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
            unsafe { drop(Box::from_raw(ctx_ptr)) };
            return check_chiaki(init_err).and_then(|_| unreachable!());
        }

        let mut opus_decoder = Box::new(ChiakiOpusDecoder::default());
        unsafe {
            chiaki_opus_decoder_init(opus_decoder.as_mut(), logger.as_mut_ptr());
            opus_decoder.settings_cb = Some(opus_settings_cb);
            opus_decoder.frame_cb = Some(opus_frame_cb);
            opus_decoder.cb_user = ctx_ptr.cast();

            let mut audio_sink = ChiakiAudioSink::default();
            chiaki_opus_decoder_get_sink(opus_decoder.as_mut(), &mut audio_sink);
            session.audio_sink = audio_sink;
            session.video_sample_cb = Some(video_sample_cb);
            session.video_sample_cb_user = ctx_ptr.cast();
            session.event_cb = Some(event_cb);
            session.event_cb_user = ctx_ptr.cast();
        }

        let start_err = unsafe { chiaki_session_start(session.as_mut()) };
        if start_err != gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS {
            unsafe {
                chiaki_session_fini(session.as_mut());
                chiaki_opus_decoder_fini(opus_decoder.as_mut());
                drop(Box::from_raw(ctx_ptr));
            }
            return check_chiaki(start_err).and_then(|_| unreachable!());
        }

        let handle = Self {
            session,
            opus_decoder,
            _logger: logger,
            ctx: ctx_ptr,
            started: true,
        };
        let channels = SessionChannels {
            video_rx,
            audio_rx,
            events_rx,
        };
        Ok((handle, channels))
    }

    pub fn send_controller_state(&mut self, state: &ControllerState) -> Result<()> {
        let mut raw = state.to_chiaki();
        let err = unsafe { chiaki_session_set_controller_state(self.session.as_mut(), &mut raw) };
        check_chiaki(err)
    }

    pub fn send_login_pin(&mut self, pin: &str) -> Result<()> {
        let bytes = pin.as_bytes();
        let err = unsafe {
            chiaki_session_set_login_pin(self.session.as_mut(), bytes.as_ptr(), bytes.len())
        };
        check_chiaki(err)
    }

    pub fn request_idr(&mut self) -> Result<()> {
        let err = unsafe { chiaki_session_request_idr(self.session.as_mut()) };
        check_chiaki(err)
    }

    pub fn stop(&mut self) {
        if self.started {
            unsafe {
                chiaki_session_stop(self.session.as_mut());
            }
        }
    }
}

impl Drop for Ps5Session {
    fn drop(&mut self) {
        if self.started {
            unsafe {
                chiaki_session_stop(self.session.as_mut());
                chiaki_session_join(self.session.as_mut());
            }
            self.started = false;
        }
        unsafe {
            chiaki_session_fini(self.session.as_mut());
            chiaki_opus_decoder_fini(self.opus_decoder.as_mut());
            if !self.ctx.is_null() {
                drop(Box::from_raw(self.ctx));
                self.ctx = std::ptr::null_mut();
            }
        }
    }
}

unsafe extern "C" fn video_sample_cb(
    buf: *mut u8,
    buf_size: usize,
    frames_lost: i32,
    frame_recovered: bool,
    user: *mut c_void,
) -> bool {
    if buf.is_null() || buf_size == 0 || user.is_null() {
        return true;
    }
    let ctx = unsafe { &*(user.cast::<SessionCallbacksCtx>()) };
    let data = unsafe { std::slice::from_raw_parts(buf, buf_size) }.to_vec();
    match ctx.video_tx.try_send(VideoPacket {
        data,
        frames_lost,
        recovered: frame_recovered,
    }) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => false,
        Err(TrySendError::Disconnected(_)) => false,
    }
}

unsafe extern "C" fn opus_settings_cb(channels: u32, rate: u32, user: *mut c_void) {
    if user.is_null() {
        return;
    }
    let ctx = unsafe { &*(user.cast::<SessionCallbacksCtx>()) };
    ctx.audio_channels.store(channels.max(1), Ordering::Relaxed);
    ctx.audio_rate.store(rate.max(8_000), Ordering::Relaxed);
}

unsafe extern "C" fn opus_frame_cb(buf: *mut i16, samples_count: usize, user: *mut c_void) {
    if buf.is_null() || samples_count == 0 || user.is_null() {
        return;
    }
    let ctx = unsafe { &*(user.cast::<SessionCallbacksCtx>()) };
    let channels = ctx.audio_channels.load(Ordering::Relaxed);
    let sample_rate = ctx.audio_rate.load(Ordering::Relaxed);
    let total_samples = samples_count.saturating_mul(channels as usize);
    let samples = unsafe { std::slice::from_raw_parts(buf, total_samples) }.to_vec();
    let _ = ctx.audio_tx.try_send(AudioPcmFrame {
        samples,
        channels,
        sample_rate,
    });
}

unsafe extern "C" fn event_cb(event: *mut ChiakiEvent, user: *mut c_void) {
    if event.is_null() || user.is_null() {
        return;
    }
    let ctx = unsafe { &*(user.cast::<SessionCallbacksCtx>()) };
    let ev = unsafe { &*event };

    let mapped = match ev.type_ {
        x if x == ChiakiEventType_CHIAKI_EVENT_CONNECTED => Some(SessionEvent::Connected),
        x if x == ChiakiEventType_CHIAKI_EVENT_QUIT => unsafe {
            let q = ev.__bindgen_anon_1.quit;
            let base = CStr::from_ptr(chiaki_quit_reason_string(q.reason))
                .to_string_lossy()
                .into_owned();
            let detail = if q.reason_str.is_null() {
                base
            } else {
                let extra = CStr::from_ptr(q.reason_str).to_string_lossy();
                if extra.is_empty() {
                    base
                } else {
                    format!("{base}: {extra}")
                }
            };
            let is_error = q.reason != ChiakiQuitReason_CHIAKI_QUIT_REASON_STOPPED
                && q.reason
                    != ChiakiQuitReason_CHIAKI_QUIT_REASON_STREAM_CONNECTION_REMOTE_SHUTDOWN;
            Some(SessionEvent::Quit {
                reason: detail,
                is_error,
            })
        },
        x if x == ChiakiEventType_CHIAKI_EVENT_RUMBLE => unsafe {
            let r = ev.__bindgen_anon_1.rumble;
            Some(SessionEvent::Rumble {
                left: r.left,
                right: r.right,
            })
        },
        x if x == ChiakiEventType_CHIAKI_EVENT_LED_COLOR => unsafe {
            let led = ev.__bindgen_anon_1.led_state;
            Some(SessionEvent::Led {
                r: led[0],
                g: led[1],
                b: led[2],
            })
        },
        x if x == ChiakiEventType_CHIAKI_EVENT_TRIGGER_EFFECTS => unsafe {
            let t = ev.__bindgen_anon_1.trigger_effects;
            Some(SessionEvent::TriggerEffects {
                type_left: t.type_left,
                type_right: t.type_right,
                left: t.left,
                right: t.right,
            })
        },
        x if x == ChiakiEventType_CHIAKI_EVENT_NICKNAME_RECEIVED => unsafe {
            let nick = CStr::from_ptr(ev.__bindgen_anon_1.server_nickname.as_ptr())
                .to_string_lossy()
                .into_owned();
            Some(SessionEvent::Nickname(nick))
        },
        x if x == ChiakiEventType_CHIAKI_EVENT_LOGIN_PIN_REQUEST => unsafe {
            Some(SessionEvent::LoginPinRequested {
                incorrect: ev.__bindgen_anon_1.login_pin_request.pin_incorrect,
            })
        },
        _ => None,
    };

    if let Some(item) = mapped {
        let _ = ctx.event_tx.try_send(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_bitrate_within_ps5_fec_limits() {
        assert_eq!(clamp_bitrate_kbps(500), MIN_BITRATE_KBPS);
        assert_eq!(clamp_bitrate_kbps(50_000), 50_000);
        assert_eq!(clamp_bitrate_kbps(250_000), MAX_BITRATE_KBPS);
    }
}
