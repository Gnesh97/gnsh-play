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
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const MIN_BITRATE_KBPS: u32 = 2_000;
pub const MAX_BITRATE_KBPS: u32 = 100_000;
pub const UDP_RECV_BUF_BYTES: i32 = 4 * 1024 * 1024;

const SOL_SOCKET: i32 = 0xFFFF;
const SO_RCVBUF: i32 = 0x1002;
const INVALID_SOCKET: usize = usize::MAX;
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;
const RTT_PROBE_INTERVAL: Duration = Duration::from_secs(1);
const RTT_PROBE_TIMEOUT_MS: u32 = 500;

#[link(name = "ws2_32")]
unsafe extern "system" {
    fn setsockopt(s: usize, level: i32, optname: i32, optval: *const i8, optlen: i32) -> i32;
    fn getsockopt(s: usize, level: i32, optname: i32, optval: *mut i8, optlen: *mut i32) -> i32;
    fn WSAGetLastError() -> i32;
}

#[link(name = "iphlpapi")]
unsafe extern "system" {
    fn IcmpCreateFile() -> *mut c_void;
    fn IcmpCloseHandle(icmp_handle: *mut c_void) -> i32;
    fn IcmpSendEcho(
        icmp_handle: *mut c_void,
        destination_address: u32,
        request_data: *const c_void,
        request_size: u16,
        request_options: *const c_void,
        reply_buffer: *mut c_void,
        reply_size: u32,
        timeout: u32,
    ) -> u32;
}

/// Live network and video throughput metrics from the active Remote Play session.
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamTelemetry {
    pub rtt_ms: f32,
    pub measured_mbps: f32,
    pub packet_loss_pct: f32,
    pub video_bytes_total: u64,
}

/// Clamps a requested stream bitrate cleanly within PS5 Remote Play FEC bounds.
pub fn clamp_bitrate_kbps(kbps: u32) -> u32 {
    kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    video_bytes_total: AtomicU64,
}

pub struct Ps5Session {
    session: Box<ChiakiSession>,
    opus_decoder: Box<ChiakiOpusDecoder>,
    _logger: ChiakiLogger,
    ctx: *mut SessionCallbacksCtx,
    rtt_probe: RttProbe,
    started: bool,
}

unsafe impl Send for Ps5Session {}

impl Ps5Session {
    pub fn start(config: Ps5SessionConfig) -> Result<(Self, SessionChannels)> {
        init_lib()?;
        let c_host = CString::new(config.host.trim())
            .map_err(|_| ProtoError::InvalidArg("host contains interior NUL".into()))?;

        let mut logger = ChiakiLogger::new(false);

        let (video_tx, video_rx) = crossbeam_channel::bounded(32);
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(32);
        let (event_tx, events_rx) = crossbeam_channel::bounded(64);

        let ctx_ptr = Box::into_raw(Box::new(SessionCallbacksCtx {
            video_tx,
            audio_tx,
            event_tx,
            audio_channels: AtomicU32::new(2),
            audio_rate: AtomicU32::new(48_000),
            video_bytes_total: AtomicU64::new(0),
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
            // Upstream chiaki-ng defaults this to false: requesting IDR on every FEC miss locks
            // libchiaki into waiting_for_idr when high-entropy IDR keyframes themselves drop units.
            enable_idr_on_fec_failure: false,
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

        let rtt_probe = RttProbe::start(config.host.trim());
        let handle = Self {
            session,
            opus_decoder,
            _logger: logger,
            ctx: ctx_ptr,
            rtt_probe,
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
        self.rtt_probe.stop();
        if self.started {
            unsafe {
                chiaki_session_stop(self.session.as_mut());
            }
        }
    }

    /// Bumps the underlying Takion UDP socket `SO_RCVBUF` at runtime without patching libchiaki.
    pub fn tune_udp_recv_buffer(&mut self, bytes: i32) -> Result<i32> {
        let sock = unsafe {
            std::ptr::addr_of!(self.session.stream_connection.takion.sock).read_volatile()
        } as usize;
        apply_udp_recv_buffer(sock, bytes)
    }

    /// Reads current RTT, packet loss, Chiaki bitrate estimate, and total decoded video bytes.
    pub fn telemetry(&self) -> StreamTelemetry {
        let senkusha_rtt_us = unsafe { std::ptr::addr_of!(self.session.rtt_us).read_volatile() };
        let live_rtt_us = self.rtt_probe.rtt_us();
        let rtt_us = if live_rtt_us > 0 {
            live_rtt_us as u64
        } else {
            senkusha_rtt_us
        };
        let measured_bitrate = unsafe {
            std::ptr::addr_of!(self.session.stream_connection.measured_bitrate).read_volatile()
        };
        let packet_loss = unsafe {
            std::ptr::addr_of!(
                self.session
                    .stream_connection
                    .congestion_control
                    .packet_loss
            )
            .read_volatile()
        };
        let video_bytes_total = if self.ctx.is_null() {
            0
        } else {
            unsafe { (*self.ctx).video_bytes_total.load(Ordering::Relaxed) }
        };

        StreamTelemetry {
            rtt_ms: (rtt_us as f32) / 1000.0,
            measured_mbps: if measured_bitrate.is_finite() && measured_bitrate >= 0.0 {
                measured_bitrate as f32
            } else {
                0.0
            },
            packet_loss_pct: if packet_loss.is_finite() && packet_loss >= 0.0 {
                (packet_loss * 100.0).clamp(0.0, 100.0) as f32
            } else {
                0.0
            },
            video_bytes_total,
        }
    }
}

struct RttProbe {
    live_rtt_us: Arc<AtomicU32>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl RttProbe {
    /// Starts a periodic ICMP RTT sampler for LAN hosts so HUD latency tracks live changes.
    fn start(host: &str) -> Self {
        let live_rtt_us = Arc::new(AtomicU32::new(0));
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let thread = resolve_ipv4(host).and_then(|addr| {
            let rtt_ref = Arc::clone(&live_rtt_us);
            let stop_ref = Arc::clone(&stop);
            thread::Builder::new()
                .name("gnsh-rtt-probe".into())
                .spawn(move || {
                    let handle = unsafe { IcmpCreateFile() };
                    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                        return;
                    }
                    loop {
                        if *stop_ref.0.lock().unwrap_or_else(|e| e.into_inner()) {
                            break;
                        }
                        if let Some(us) = icmp_ping_with_handle(handle, addr, RTT_PROBE_TIMEOUT_MS)
                        {
                            rtt_ref.store(us, Ordering::Relaxed);
                        }
                        let (lock, cvar) = &*stop_ref;
                        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        let (guard, _) = cvar
                            .wait_timeout_while(guard, RTT_PROBE_INTERVAL, |is_stopped| {
                                !*is_stopped
                            })
                            .unwrap_or_else(|e| e.into_inner());
                        if *guard {
                            break;
                        }
                    }
                    unsafe {
                        IcmpCloseHandle(handle);
                    }
                })
                .ok()
        });

        Self {
            live_rtt_us,
            stop,
            thread,
        }
    }

    fn rtt_us(&self) -> u32 {
        self.live_rtt_us.load(Ordering::Relaxed)
    }

    fn stop(&mut self) {
        {
            let (lock, cvar) = &*self.stop;
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            *guard = true;
            cvar.notify_all();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RttProbe {
    fn drop(&mut self) {
        self.stop();
    }
}

fn resolve_ipv4(host: &str) -> Option<Ipv4Addr> {
    let trimmed = host.trim();
    if let Ok(ip) = trimmed.parse::<Ipv4Addr>() {
        return Some(ip);
    }
    (trimmed, 9295u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|addr| match addr {
            SocketAddr::V4(v4) => Some(*v4.ip()),
            SocketAddr::V6(_) => None,
        })
}

fn icmp_ping_with_handle(handle: *mut c_void, addr: Ipv4Addr, timeout_ms: u32) -> Option<u32> {
    const PAYLOAD: [u8; 16] = *b"gnsh-play-rtt-01";
    #[repr(C, align(8))]
    struct ReplyBuf([u8; 128]);

    let mut reply = ReplyBuf([0u8; 128]);
    let dest_addr = u32::from_ne_bytes(addr.octets());
    let started = Instant::now();
    let count = unsafe {
        IcmpSendEcho(
            handle,
            dest_addr,
            PAYLOAD.as_ptr().cast::<c_void>(),
            PAYLOAD.len() as u16,
            std::ptr::null(),
            reply.0.as_mut_ptr().cast::<c_void>(),
            reply.0.len() as u32,
            timeout_ms,
        )
    };
    // Measure around IcmpSendEcho so sub-millisecond LAN hops retain microsecond precision.
    let elapsed_us = started.elapsed().as_micros().clamp(1, u32::MAX as u128) as u32;
    if count == 0 {
        return None;
    }
    let status = u32::from_ne_bytes(reply.0[4..8].try_into().ok()?);
    if status != 0 {
        return None;
    }
    Some(elapsed_us)
}

#[cfg(test)]
fn icmp_ping_ipv4(addr: Ipv4Addr, timeout_ms: u32) -> Option<u32> {
    let handle = unsafe { IcmpCreateFile() };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let res = icmp_ping_with_handle(handle, addr, timeout_ms);
    unsafe {
        IcmpCloseHandle(handle);
    }
    res
}

fn apply_udp_recv_buffer(sock: usize, bytes: i32) -> Result<i32> {
    if sock == 0 || sock == INVALID_SOCKET || bytes <= 0 {
        return Err(ProtoError::InvalidArg(
            "invalid UDP socket or buffer size".into(),
        ));
    }
    let val = bytes;
    let rc = unsafe {
        setsockopt(
            sock,
            SOL_SOCKET,
            SO_RCVBUF,
            (&val as *const i32).cast::<i8>(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if rc != 0 {
        let wsa = unsafe { WSAGetLastError() };
        return Err(ProtoError::Windows(format!(
            "setsockopt(SO_RCVBUF={bytes}) failed with WSA error {wsa}"
        )));
    }
    let mut actual: i32 = 0;
    let mut actual_len = std::mem::size_of::<i32>() as i32;
    let g_rc = unsafe {
        getsockopt(
            sock,
            SOL_SOCKET,
            SO_RCVBUF,
            (&mut actual as *mut i32).cast::<i8>(),
            &mut actual_len,
        )
    };
    if g_rc != 0 {
        return Ok(bytes);
    }
    Ok(actual)
}

impl Drop for Ps5Session {
    fn drop(&mut self) {
        self.rtt_probe.stop();
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
    ctx.video_bytes_total
        .fetch_add(buf_size as u64, Ordering::Relaxed);
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

    #[test]
    fn sets_udp_recv_buffer_to_4_mib() {
        use std::os::windows::io::AsRawSocket;

        let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP loopback socket");
        let actual =
            apply_udp_recv_buffer(sock.as_raw_socket() as usize, UDP_RECV_BUF_BYTES).expect("tune");
        assert!(actual >= UDP_RECV_BUF_BYTES);
    }

    #[test]
    fn icmp_ping_loopback_measures_rtt() {
        let rtt_us =
            icmp_ping_ipv4(Ipv4Addr::LOCALHOST, 500).expect("loopback ICMP echo must succeed");
        assert!(
            rtt_us > 0 && rtt_us < 500_000,
            "unexpected rtt_us: {rtt_us}"
        );
    }

    #[test]
    fn chiaki_session_layout_matches_packet_loss_max() {
        init_lib().expect("chiaki lib init");
        let mut logger = ChiakiLogger::new(false);
        let host = CString::new("127.0.0.1").unwrap();
        let mut connect_info = ChiakiConnectInfo {
            ps5: true,
            host: host.as_ptr(),
            packet_loss_max: 0.05,
            ..Default::default()
        };
        let mut session = Box::new(ChiakiSession::default());
        let err = unsafe {
            chiaki_session_init(session.as_mut(), &mut connect_info, logger.as_mut_ptr())
        };
        assert_eq!(err, gnsh_chiaki_sys::ChiakiErrorCode_CHIAKI_ERR_SUCCESS);
        let actual_max = unsafe {
            std::ptr::addr_of!(session.stream_connection.packet_loss_max).read_volatile()
        };
        unsafe {
            chiaki_session_fini(session.as_mut());
        }
        assert!(
            (actual_max - 0.05).abs() < 1e-6,
            "unexpected packet_loss_max: {actual_max}"
        );
    }
}
