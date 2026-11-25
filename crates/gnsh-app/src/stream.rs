use crate::storage::{SavedConsole, StoredSettings};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, select};
use gnsh_input::GamepadManager;
use gnsh_media::{
    AudioOutput, DecodedFrame, DecoderHardware, HapticsAudioOutput, MediaCodec, MicCapture,
    VideoDecoder, volume_pct_to_gain,
};
use gnsh_proto::{Ps5Session, Ps5SessionConfig, SessionEvent, VideoCodec};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};

pub(crate) const IDR_COOLDOWN: Duration = Duration::from_millis(500);
const PIPELINE_HISTORY_LEN: usize = 120;

struct SleepInhibitor {
    high_res_timer_active: bool,
}

impl SleepInhibitor {
    fn acquire() -> Self {
        // SAFETY: SetThreadExecutionState updates only the calling UI thread's idle-timer state
        // until Drop restores ES_CONTINUOUS.
        let prev = unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
        };
        // SAFETY: 1 ms OS timer resolution sharpens Condvar wait_timeout scheduling for decode-paced
        // frame wakeups and high-rate controller polling; balanced cleanly by timeEndPeriod(1) in Drop.
        let timer_ok = unsafe { timeBeginPeriod(1) } == 0;
        tracing::debug!(
            prev_flags = prev.0,
            high_res_timer = timer_ok,
            "Acquired Win32 display/sleep inhibitor"
        );
        Self {
            high_res_timer_active: timer_ok,
        }
    }
}

impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        if self.high_res_timer_active {
            // SAFETY: Matches the successful timeBeginPeriod(1) call from acquire().
            unsafe {
                let _ = timeEndPeriod(1);
            }
        }
        // SAFETY: Restores default display/sleep policy on the UI thread after stream shutdown.
        let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        tracing::debug!(
            prev_flags = prev.0,
            "Released Win32 display/sleep inhibitor"
        );
    }
}

fn boost_current_thread_priority(worker_name: &str) {
    // SAFETY: GetCurrentThread returns a pseudo-handle valid for the calling worker thread.
    if let Err(err) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) }
    {
        tracing::debug!(worker = worker_name, "SetThreadPriority failed: {err:#}");
    }
}

fn compute_p50_p95(samples: &VecDeque<f32>) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sorted: Vec<f32> = samples
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .collect();
    if sorted.is_empty() {
        return (0.0, 0.0);
    }
    sorted.sort_by(f32::total_cmp);
    let n = sorted.len();
    let p50 = sorted[n / 2];
    let p95_idx = ((n * 95) / 100).min(n - 1);
    (p50, sorted[p95_idx])
}

pub(crate) struct LatestFrame {
    pub(crate) frame: DecodedFrame,
    pub(crate) decode_ms: f32,
    pub(crate) is_hw: bool,
    pub(crate) arrived_at: Instant,
}

pub(crate) struct LoginPinPrompt {
    pub(crate) pin_input: String,
    pub(crate) incorrect: bool,
}

pub(crate) struct OskPrompt {
    pub(crate) text: String,
    pub(crate) last_sent_text: String,
    pub(crate) needs_focus: bool,
    pub(crate) cancel_requested: bool,
}

pub(crate) struct ActiveStream {
    pub(crate) console_name: String,
    pub(crate) console_host: String,
    pub(crate) codec: VideoCodec,
    pub(crate) session: Option<Ps5Session>,
    pub(crate) events_rx: Receiver<SessionEvent>,
    pub(crate) frame_sync: Arc<(Mutex<Option<LatestFrame>>, Condvar)>,
    pub(crate) current_frame: Option<LatestFrame>,
    pub(crate) current_frame_pending_measure: bool,
    pub(crate) frames_decoded: Arc<AtomicU64>,
    pub(crate) decoder_lag_frames: Arc<AtomicU64>,
    pub(crate) frames_lost_total: Arc<AtomicU64>,
    pub(crate) idr_needed: Arc<AtomicBool>,
    pub(crate) video_init_error: Arc<Mutex<Option<String>>>,
    pub(crate) stop_flag: Arc<AtomicBool>,
    pub(crate) video_thread: Option<JoinHandle<()>>,
    pub(crate) audio_thread: Option<JoinHandle<()>>,
    pub(crate) usb_haptics_active: Arc<AtomicBool>,
    pub(crate) haptics_recheck: Arc<AtomicBool>,
    pub(crate) haptics_device_label: Arc<Mutex<Option<String>>>,
    pub(crate) volume_pct: Arc<AtomicU8>,
    pub(crate) audio_device: Arc<Mutex<Option<String>>>,
    pub(crate) audio_device_recheck: Arc<AtomicBool>,
    pub(crate) mic_capture: Option<MicCapture>,
    pub(crate) mic_device: Option<String>,
    pub(crate) mic_muted: bool,
    pub(crate) mic_toggle_requested: bool,
    pub(crate) mic_error: Option<String>,
    _sleep_inhibitor: SleepInhibitor,
    pub(crate) connected: bool,
    pub(crate) show_hud: bool,
    pub(crate) hud_drawer_open: bool,
    pub(crate) hud_focus_row: usize,
    pub(crate) pad_overlay_was_active: bool,
    pub(crate) mouse_lock_toggle_requested: bool,
    pub(crate) kbm_was_active: bool,
    pub(crate) disconnect_requested: bool,
    pub(crate) go_home_requested: bool,
    pub(crate) pin_prompt: Option<LoginPinPrompt>,
    pub(crate) osk_prompt: Option<OskPrompt>,
    pub(crate) fps: f32,
    pub(crate) fps_window_start: Instant,
    pub(crate) fps_last_count: u64,
    pub(crate) measured_mbps: f32,
    pub(crate) rtt_ms: f32,
    pub(crate) packet_loss_pct: f32,
    pub(crate) loss_window_start: Instant,
    pub(crate) loss_history: VecDeque<f32>,
    pub(crate) pipeline_history: VecDeque<f32>,
    pub(crate) pipe_p50_ms: f32,
    pub(crate) pipe_p95_ms: f32,
    pub(crate) render_ms: f32,
    pub(crate) logged_pipeline_diag: bool,
    pub(crate) bytes_last_count: u64,
    pub(crate) udp_rcvbuf_kib: Option<i32>,
    pub(crate) last_idr_request: Instant,
    pub(crate) last_seen_lost: u64,
    pub(crate) packet_loss_notice_until: Option<Instant>,
    pub(crate) rumble_active_until: Option<Instant>,
}

impl ActiveStream {
    #[allow(dead_code)]
    pub(crate) fn start(
        console: &SavedConsole,
        settings: &StoredSettings,
        hw: DecoderHardware,
    ) -> Result<Self> {
        Self::start_with_host(console, &console.host, settings, false, hw)
    }

    pub(crate) fn start_with_host(
        console: &SavedConsole,
        host_override: &str,
        settings: &StoredSettings,
        use_wan: bool,
        hw: DecoderHardware,
    ) -> Result<Self> {
        let target_host = host_override.trim();
        let (preset, codec, bitrate_kbps) = settings.effective_stream_profile(use_wan);
        let config = Ps5SessionConfig {
            host: target_host.to_string(),
            regist_key: console.regist_key,
            rp_key: console.rp_key,
            preset,
            codec,
            bitrate_kbps,
            enable_dualsense: true,
        };
        let (session, channels) = Ps5Session::start(config)
            .with_context(|| format!("failed starting Remote Play session to {target_host}"))?;

        tracing::info!(
            host = %target_host,
            use_wan,
            ?preset,
            ?codec,
            bitrate_kbps,
            "Started Remote Play stream session"
        );

        let media_codec = match codec {
            VideoCodec::H264 => MediaCodec::H264,
            VideoCodec::Hevc => MediaCodec::Hevc,
            VideoCodec::HevcHdr => MediaCodec::HevcHdr,
        };

        let frame_sync = Arc::new((Mutex::new(None), Condvar::new()));
        let frames_decoded = Arc::new(AtomicU64::new(0));
        let decoder_lag_frames = Arc::new(AtomicU64::new(0));
        let frames_lost_total = Arc::new(AtomicU64::new(0));
        let idr_needed = Arc::new(AtomicBool::new(false));
        let video_init_error = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let usb_haptics_active = session.usb_haptics_flag();
        let haptics_recheck = Arc::new(AtomicBool::new(false));
        let haptics_device_label = Arc::new(Mutex::new(None));
        let sleep_inhibitor = SleepInhibitor::acquire();

        let video_args = VideoWorkerArgs {
            codec: media_codec,
            hw,
            video_rx: channels.video_rx,
            frame_sync: Arc::clone(&frame_sync),
            frames_decoded: Arc::clone(&frames_decoded),
            decoder_lag_frames: Arc::clone(&decoder_lag_frames),
            frames_lost_total: Arc::clone(&frames_lost_total),
            idr_needed: Arc::clone(&idr_needed),
            init_error: Arc::clone(&video_init_error),
            stop_flag: Arc::clone(&stop_flag),
        };
        let video_thread = thread::Builder::new()
            .name("gnsh-video-decode".into())
            .spawn(move || run_video_decode_loop(video_args))
            .context("failed spawning video decode thread")?;

        let volume_pct = Arc::new(AtomicU8::new(settings.volume_pct.min(100)));
        let audio_device = Arc::new(Mutex::new(settings.audio_device.clone()));
        let audio_device_recheck = Arc::new(AtomicBool::new(false));
        let audio_args = AudioWorkerArgs {
            audio_rx: channels.audio_rx,
            haptics_rx: channels.haptics_rx,
            usb_haptics_active: Arc::clone(&usb_haptics_active),
            haptics_recheck: Arc::clone(&haptics_recheck),
            haptics_device_label: Arc::clone(&haptics_device_label),
            volume_pct: Arc::clone(&volume_pct),
            audio_device: Arc::clone(&audio_device),
            audio_device_recheck: Arc::clone(&audio_device_recheck),
            stop_flag: Arc::clone(&stop_flag),
        };
        let audio_thread = thread::Builder::new()
            .name("gnsh-audio-out".into())
            .spawn(move || run_audio_output_loop(audio_args))
            .context("failed spawning audio output thread")?;

        Ok(Self {
            console_name: console.name.clone(),
            console_host: target_host.to_string(),
            codec,
            session: Some(session),
            events_rx: channels.events_rx,
            frame_sync,
            current_frame: None,
            current_frame_pending_measure: false,
            frames_decoded,
            decoder_lag_frames,
            frames_lost_total,
            idr_needed,
            video_init_error,
            stop_flag,
            video_thread: Some(video_thread),
            audio_thread: Some(audio_thread),
            usb_haptics_active,
            haptics_recheck,
            haptics_device_label,
            volume_pct,
            audio_device,
            audio_device_recheck,
            mic_capture: None,
            mic_device: settings.mic_device.clone(),
            mic_muted: true,
            mic_toggle_requested: false,
            mic_error: None,
            _sleep_inhibitor: sleep_inhibitor,
            connected: false,
            show_hud: true,
            hud_drawer_open: false,
            hud_focus_row: 0,
            pad_overlay_was_active: false,
            mouse_lock_toggle_requested: false,
            kbm_was_active: false,
            disconnect_requested: false,
            go_home_requested: false,
            pin_prompt: None,
            osk_prompt: None,
            fps: 0.0,
            fps_window_start: Instant::now(),
            fps_last_count: 0,
            measured_mbps: 0.0,
            rtt_ms: 0.0,
            packet_loss_pct: 0.0,
            loss_window_start: Instant::now(),
            loss_history: VecDeque::with_capacity(10),
            pipeline_history: VecDeque::with_capacity(PIPELINE_HISTORY_LEN),
            pipe_p50_ms: 0.0,
            pipe_p95_ms: 0.0,
            render_ms: 0.0,
            logged_pipeline_diag: false,
            bytes_last_count: 0,
            udp_rcvbuf_kib: None,
            last_idr_request: Instant::now() - IDR_COOLDOWN,
            last_seen_lost: 0,
            packet_loss_notice_until: None,
            rumble_active_until: None,
        })
    }

    pub(crate) fn wait_for_frame(&self, timeout: Duration) {
        let (lock, cvar) = &*self.frame_sync;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let _ = cvar.wait_timeout_while(guard, timeout, |slot| slot.is_none());
        }
    }

    pub(crate) fn take_latest_frame(&mut self) -> bool {
        let newer = {
            let (lock, _) = &*self.frame_sync;
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(frame) = newer {
            self.current_frame = Some(frame);
            self.current_frame_pending_measure = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn record_present_latency(&mut self) {
        if !self.current_frame_pending_measure {
            return;
        }
        self.current_frame_pending_measure = false;
        let Some(ref frame) = self.current_frame else {
            return;
        };
        let ms = frame.arrived_at.elapsed().as_secs_f32() * 1000.0;
        if !ms.is_finite() || ms < 0.0 {
            return;
        }
        self.pipeline_history.push_back(ms);
        while self.pipeline_history.len() > PIPELINE_HISTORY_LEN {
            self.pipeline_history.pop_front();
        }
        let (p50, p95) = compute_p50_p95(&self.pipeline_history);
        self.pipe_p50_ms = p50;
        self.pipe_p95_ms = p95;
    }

    pub(crate) fn toggle_mic(&mut self, gamepads: &GamepadManager) {
        if !self.connected {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let target_muted = !self.mic_muted;
        if !target_muted && self.mic_capture.is_none() {
            match MicCapture::new_for_device(self.mic_device.as_deref()) {
                Ok(cap) => self.mic_capture = Some(cap),
                Err(err) => {
                    let msg = format!("{err:#}");
                    tracing::warn!("Microphone initialization failed: {msg}");
                    self.mic_error = Some(msg);
                    return;
                }
            }
        }
        if let Some(cap) = self.mic_capture.as_mut()
            && let Err(err) = cap.set_muted(target_muted)
        {
            let msg = format!("{err:#}");
            tracing::warn!("Microphone capture state update failed: {msg}");
            self.mic_error = Some(msg);
            return;
        }
        if let Err(err) = session.set_mic_muted(target_muted) {
            let msg = format!("{err:#}");
            tracing::warn!("PS5 microphone state update failed: {msg}");
            if let Some(cap) = self.mic_capture.as_mut() {
                let _ = cap.set_muted(true);
            }
            self.mic_error = Some(msg);
            return;
        }
        self.mic_muted = target_muted;
        self.mic_error = None;
        gamepads.set_mic_led(target_muted);
        tracing::info!(muted = target_muted, "Stream microphone state changed");
    }

    pub(crate) fn set_mic_device(&mut self, device_name: Option<String>) {
        self.mic_device = device_name;
        if let Some(cap) = self.mic_capture.as_mut() {
            match cap.switch_device(self.mic_device.as_deref()) {
                Ok(()) => {
                    self.mic_error = None;
                    tracing::info!(
                        device = self.mic_device.as_deref().unwrap_or("System Default"),
                        "Switched microphone capture device"
                    );
                }
                Err(err) => {
                    let msg = format!("{err:#}");
                    tracing::warn!("Microphone capture device switch failed: {msg}");
                    self.mic_error = Some(msg);
                }
            }
        }
    }

    pub(crate) fn shutdown(&mut self) {
        if self.session.is_none()
            && self.video_thread.is_none()
            && self.audio_thread.is_none()
            && self.mic_capture.is_none()
            && self.current_frame.is_none()
        {
            return;
        }
        tracing::info!(host = %self.console_host, "Shutting down active stream session");
        self.stop_flag.store(true, Ordering::Relaxed);
        self.mic_capture = None;
        // Drop queued D3D11VA/AVFrame surfaces before joining the decode worker and destroying its context.
        self.current_frame = None;
        if let Ok(mut guard) = self.frame_sync.0.lock() {
            *guard = None;
        }
        self.frame_sync.1.notify_all();
        if let Some(mut session) = self.session.take() {
            session.stop();
            drop(session);
        }
        if let Some(handle) = self.video_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
        self.current_frame = None;
        if let Ok(mut guard) = self.frame_sync.0.lock() {
            *guard = None;
        }
        tracing::info!(host = %self.console_host, "Active stream shutdown complete");
    }
}

impl Drop for ActiveStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct VideoWorkerArgs {
    codec: MediaCodec,
    hw: DecoderHardware,
    video_rx: Receiver<gnsh_proto::VideoPacket>,
    frame_sync: Arc<(Mutex<Option<LatestFrame>>, Condvar)>,
    frames_decoded: Arc<AtomicU64>,
    decoder_lag_frames: Arc<AtomicU64>,
    frames_lost_total: Arc<AtomicU64>,
    idr_needed: Arc<AtomicBool>,
    init_error: Arc<Mutex<Option<String>>>,
    stop_flag: Arc<AtomicBool>,
}

fn run_video_decode_loop(args: VideoWorkerArgs) {
    boost_current_thread_priority("video");
    let mut decoder = match VideoDecoder::new_with_mode(args.codec, args.hw) {
        Ok(dec) => dec,
        Err(err) => {
            tracing::error!("VideoDecoder initialization failed: {err:#}");
            if let Ok(mut guard) = args.init_error.lock() {
                *guard = Some(format!("Video decoder initialization failed: {err:#}"));
            }
            return;
        }
    };

    tracing::info!(
        hw_active = decoder.is_hw_active(),
        codec = ?args.codec,
        "Video decoder ready"
    );

    let mut consecutive_errors: u32 = 0;
    let mut next_seq: i64 = 1;
    let mut seq_arrivals: VecDeque<(i64, Instant)> = VecDeque::with_capacity(32);

    while !args.stop_flag.load(Ordering::Relaxed) {
        match args.video_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(pkt) => {
                if pkt.frames_lost > 0 {
                    args.frames_lost_total
                        .fetch_add(pkt.frames_lost as u64, Ordering::Relaxed);
                    tracing::warn!(
                        frames_lost = pkt.frames_lost,
                        recovered = pkt.recovered,
                        "Video packet loss detected"
                    );
                    if !pkt.recovered {
                        args.idr_needed.store(true, Ordering::Relaxed);
                    }
                }

                let seq = next_seq;
                next_seq = next_seq.saturating_add(1);
                seq_arrivals.push_back((seq, pkt.arrived_at));
                while seq_arrivals.len() > 32 {
                    seq_arrivals.pop_front();
                }

                let t0 = Instant::now();
                match decoder.decode_with_pts(&pkt.data, seq) {
                    Ok(frames) => {
                        let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;
                        let is_hw = decoder.is_hw_active();
                        let count = frames.len() as u64;
                        if count > 0 {
                            args.frames_decoded.fetch_add(count, Ordering::Relaxed);
                            consecutive_errors = 0;
                        }
                        if let Some(frame) = frames.into_iter().last() {
                            let out_pts = frame.pts();
                            let matched_arrived = seq_arrivals
                                .iter()
                                .rev()
                                .find(|(s, _)| *s == out_pts)
                                .map(|(_, t)| *t)
                                .unwrap_or(pkt.arrived_at);
                            let lag = if out_pts > 0 && seq >= out_pts {
                                (seq - out_pts) as u64
                            } else {
                                0
                            };
                            args.decoder_lag_frames.store(lag, Ordering::Relaxed);

                            let prev = {
                                let mut guard =
                                    args.frame_sync.0.lock().unwrap_or_else(|e| e.into_inner());
                                guard.replace(LatestFrame {
                                    frame,
                                    decode_ms,
                                    is_hw,
                                    arrived_at: matched_arrived,
                                })
                            };
                            args.frame_sync.1.notify_one();
                            drop(prev);
                        }
                    }
                    Err(err) => {
                        consecutive_errors += 1;
                        tracing::warn!(
                            consecutive_errors,
                            frames_lost = pkt.frames_lost,
                            "Video decode error: {err:#}"
                        );
                        if consecutive_errors >= 3 {
                            tracing::warn!(
                                "Flushing video decoder after {consecutive_errors} consecutive decode errors"
                            );
                            decoder.flush();
                            seq_arrivals.clear();
                            consecutive_errors = 0;
                        }
                        args.idr_needed.store(true, Ordering::Relaxed);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn refresh_usb_haptics(
    haptics: &mut Option<HapticsAudioOutput>,
    usb_haptics_active: &AtomicBool,
    haptics_device_label: &Mutex<Option<String>>,
) {
    let prev_active = haptics.is_some();
    *haptics = None;
    *haptics = HapticsAudioOutput::try_open_dualsense();
    let active = haptics.is_some();
    usb_haptics_active.store(active, Ordering::Release);
    if let Ok(mut guard) = haptics_device_label.lock() {
        *guard = haptics.as_ref().map(|h| h.device_name().to_string());
    }
    if let Some(hap) = haptics.as_ref() {
        tracing::info!(
            device = %hap.device_name(),
            "DualSense USB 4-channel haptics active"
        );
    } else if prev_active {
        tracing::info!("DualSense USB audio removed, falling back to motor rumble haptics");
    } else {
        tracing::debug!("No 4-channel DualSense USB audio found; using motor rumble haptics");
    }
}

struct AudioWorkerArgs {
    audio_rx: Receiver<gnsh_proto::AudioPcmFrame>,
    haptics_rx: Receiver<gnsh_proto::HapticsPcmFrame>,
    usb_haptics_active: Arc<AtomicBool>,
    haptics_recheck: Arc<AtomicBool>,
    haptics_device_label: Arc<Mutex<Option<String>>>,
    volume_pct: Arc<AtomicU8>,
    audio_device: Arc<Mutex<Option<String>>>,
    audio_device_recheck: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
}

fn run_audio_output_loop(args: AudioWorkerArgs) {
    boost_current_thread_priority("audio");
    let mut last_vol = args.volume_pct.load(Ordering::Relaxed);
    let initial_dev = args.audio_device.lock().ok().and_then(|g| g.clone());
    let mut audio = match AudioOutput::new_for_device(
        2,
        48_000,
        initial_dev.as_deref(),
        volume_pct_to_gain(last_vol),
    ) {
        Ok(out) => Some(out),
        Err(err) => {
            tracing::warn!("AudioOutput init failed (continuing muted): {err:#}");
            None
        }
    };

    let mut haptics: Option<HapticsAudioOutput> = None;
    refresh_usb_haptics(
        &mut haptics,
        &args.usb_haptics_active,
        &args.haptics_device_label,
    );

    while !args.stop_flag.load(Ordering::Relaxed) {
        if args.haptics_recheck.swap(false, Ordering::AcqRel) {
            refresh_usb_haptics(
                &mut haptics,
                &args.usb_haptics_active,
                &args.haptics_device_label,
            );
        }

        if args.audio_device_recheck.swap(false, Ordering::AcqRel) {
            let desired = args.audio_device.lock().ok().and_then(|g| g.clone());
            let vol = args.volume_pct.load(Ordering::Relaxed);
            match audio.as_mut() {
                Some(out) => {
                    if let Err(err) = out.switch_device(desired.as_deref()) {
                        tracing::warn!("Audio output device switch failed: {err:#}");
                    } else {
                        tracing::info!(
                            device = desired.as_deref().unwrap_or("System Default"),
                            "Switched audio playback device"
                        );
                    }
                }
                None => {
                    match AudioOutput::new_for_device(
                        2,
                        48_000,
                        desired.as_deref(),
                        volume_pct_to_gain(vol),
                    ) {
                        Ok(out) => audio = Some(out),
                        Err(err) => tracing::warn!("AudioOutput open failed: {err:#}"),
                    }
                }
            }
            last_vol = vol;
        }

        let cur_vol = args.volume_pct.load(Ordering::Relaxed);
        if cur_vol != last_vol {
            if let Some(out) = audio.as_mut() {
                out.set_volume(volume_pct_to_gain(cur_vol));
            }
            last_vol = cur_vol;
        }

        select! {
            recv(args.audio_rx) -> msg => match msg {
                Ok(frame) => {
                    if let Some(out) = audio.as_mut()
                        && let Err(err) =
                            out.push_pcm(&frame.samples, frame.channels, frame.sample_rate)
                    {
                        tracing::warn!("Audio push_pcm failed: {err:#}");
                    }
                }
                Err(_) => break,
            },
            recv(args.haptics_rx) -> msg => match msg {
                Ok(frame) => {
                    if let Some(hap) = haptics.as_mut()
                        && let Err(err) = hap.push_stereo_3khz(&frame.samples)
                    {
                        tracing::warn!(
                            "DualSense USB haptics push failed, falling back to rumble: {err:#}"
                        );
                        haptics = None;
                        args.usb_haptics_active.store(false, Ordering::Release);
                        if let Ok(mut guard) = args.haptics_device_label.lock() {
                            *guard = None;
                        }
                    }
                }
                Err(_) => break,
            },
            default(Duration::from_millis(50)) => {}
        }
    }

    args.usb_haptics_active.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_p50_and_p95_pipeline_latency() {
        assert_eq!(compute_p50_p95(&VecDeque::new()), (0.0, 0.0));

        let mut samples = VecDeque::new();
        for ms in (1..=20).rev() {
            samples.push_back(ms as f32);
        }
        samples.push_back(f32::NAN);
        samples.push_back(-2.0);
        let (p50, p95) = compute_p50_p95(&samples);
        assert!((p50 - 11.0).abs() < 1e-5, "unexpected p50: {p50}");
        assert!((p95 - 20.0).abs() < 1e-5, "unexpected p95: {p95}");
    }
}
