use crate::storage::{SavedConsole, StoredSettings};
use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError, bounded, select};
use gnsh_input::GamepadManager;
use gnsh_media::{
    AudioOutput, DecodedFrame, DecoderHardware, HapticsAudioOutput, MediaCodec, MicCapture,
    MkvMuxer, ReplayClipSnapshot, ReplayRingBuffer, VideoDecoder, build_capture_path,
    contains_keyframe_nal, default_screenshot_dir, default_video_dir, extract_parameter_sets,
    extract_screenshot_pixels, is_header_only_annex_b, save_screenshot_png, volume_pct_to_gain,
};
use gnsh_proto::{Ps5Session, Ps5SessionConfig, SessionEvent, VideoCodec, VideoPreset};
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf, Prefix};
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
const CAPTURE_TOAST_DURATION: Duration = Duration::from_secs(4);
const RECORDER_PKT_CHANNEL_CAP: usize = 256;
const RECORDER_CMD_CHANNEL_CAP: usize = 32;

/// Validates that a custom capture folder is an absolute local drive path without UNC or `..`.
pub(crate) fn validate_capture_dir(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(char::is_control)
        || trimmed.starts_with("\\\\")
        || trimmed.starts_with("//")
    {
        return None;
    }
    let path = Path::new(trimmed);
    let mut comps = path.components();
    match (comps.next(), comps.next()) {
        (Some(Component::Prefix(prefix)), Some(Component::RootDir))
            if matches!(prefix.kind(), Prefix::Disk(_)) => {}
        _ => return None,
    }
    if comps.any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

pub(crate) fn resolve_capture_dir(override_dir: Option<&str>, is_video: bool) -> PathBuf {
    if let Some(valid) = override_dir.and_then(validate_capture_dir) {
        return valid;
    }
    if is_video {
        default_video_dir()
    } else {
        default_screenshot_dir()
    }
}

fn preset_dimensions(preset: VideoPreset) -> (u32, u32) {
    match preset {
        VideoPreset::P720Fps30 | VideoPreset::P720Fps60 => (1280, 720),
        VideoPreset::P1080Fps30 | VideoPreset::P1080Fps60 => (1920, 1080),
    }
}

enum RecorderPacket {
    Video {
        data: Vec<u8>,
        ts_us: i64,
    },
    Audio {
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u32,
        ts_us: i64,
    },
    ParamSetSeed(Vec<u8>),
}

enum RecorderCommand {
    StartRecording { path: PathBuf },
    StopRecording,
    SaveReplayClip { path: PathBuf },
    SetReplayConfig { enabled: bool, secs: u16 },
    SetVideoDims { width: u32, height: u32 },
    Shutdown,
}

enum CaptureNotice {
    ScreenshotSaved(PathBuf),
    RecordingStarted,
    RecordingSaved(PathBuf),
    RecordingAborted(String),
    ClipSaved(PathBuf, f32),
    Info(String),
    Error(String),
}

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
    pub(crate) expected_height: u32,
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
    pub(crate) capture_needed: Arc<AtomicBool>,
    pub(crate) replay_enabled: bool,
    pub(crate) recording_pending: bool,
    pub(crate) recording_started_at: Option<Instant>,
    pub(crate) screenshot_requested: bool,
    pub(crate) record_toggle_requested: bool,
    pub(crate) save_clip_requested: bool,
    pub(crate) capture_toast: Option<(String, bool, Instant)>,
    capture_cmd_tx: Option<Sender<RecorderCommand>>,
    capture_notice_rx: Receiver<CaptureNotice>,
    capture_notice_tx: Sender<CaptureNotice>,
    recorder_thread: Option<JoinHandle<()>>,
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
        let (init_w, init_h) = preset_dimensions(preset);
        let capture_needed = Arc::new(AtomicBool::new(settings.replay_buffer_enabled));
        let (recorder_pkt_tx, recorder_pkt_rx) = bounded(RECORDER_PKT_CHANNEL_CAP);
        let (capture_cmd_tx, capture_cmd_rx) = bounded(RECORDER_CMD_CHANNEL_CAP);
        let (capture_notice_tx, capture_notice_rx) = bounded(RECORDER_CMD_CHANNEL_CAP);
        let stream_start = Instant::now();

        let recorder_args = RecorderWorkerArgs {
            codec: media_codec,
            width: init_w,
            height: init_h,
            replay_enabled: settings.replay_buffer_enabled,
            replay_secs: settings.replay_buffer_secs,
            pkt_rx: recorder_pkt_rx,
            cmd_rx: capture_cmd_rx,
            notice_tx: capture_notice_tx.clone(),
        };
        let recorder_thread = thread::Builder::new()
            .name("gnsh-recorder".into())
            .spawn(move || run_recorder_loop(recorder_args))
            .context("failed spawning recorder worker thread")?;

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
            capture_needed: Arc::clone(&capture_needed),
            recorder_pkt_tx: recorder_pkt_tx.clone(),
            recorder_cmd_tx: capture_cmd_tx.clone(),
            initial_dims: (init_w, init_h),
            stream_start,
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
            capture_needed: Arc::clone(&capture_needed),
            recorder_pkt_tx,
            stream_start,
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
            expected_height: init_h,
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
            capture_needed,
            replay_enabled: settings.replay_buffer_enabled,
            recording_pending: false,
            recording_started_at: None,
            screenshot_requested: false,
            record_toggle_requested: false,
            save_clip_requested: false,
            capture_toast: None,
            capture_cmd_tx: Some(capture_cmd_tx),
            capture_notice_rx,
            capture_notice_tx,
            recorder_thread: Some(recorder_thread),
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

    fn sync_capture_needed(&self) {
        self.capture_needed.store(
            self.replay_enabled || self.recording_pending || self.recording_started_at.is_some(),
            Ordering::Release,
        );
    }

    pub(crate) fn set_toast(&mut self, msg: impl Into<String>, is_error: bool) {
        self.capture_toast = Some((
            msg.into(),
            is_error,
            Instant::now() + CAPTURE_TOAST_DURATION,
        ));
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.recording_pending || self.recording_started_at.is_some()
    }

    pub(crate) fn recording_elapsed_secs(&self) -> Option<u64> {
        if let Some(started) = self.recording_started_at {
            Some(started.elapsed().as_secs())
        } else if self.recording_pending {
            Some(0)
        } else {
            None
        }
    }

    pub(crate) fn active_toast(&self) -> Option<(&str, bool)> {
        let (msg, is_error, expires_at) = self.capture_toast.as_ref()?;
        if Instant::now() < *expires_at {
            Some((msg.as_str(), *is_error))
        } else {
            None
        }
    }

    pub(crate) fn has_active_toast(&self) -> bool {
        self.active_toast().is_some()
    }

    pub(crate) fn trigger_screenshot(&mut self, capture_dir: Option<&str>) {
        let pixels_res = if let Some(ref latest) = self.current_frame {
            extract_screenshot_pixels(&latest.frame)
        } else if let Ok(guard) = self.frame_sync.0.lock()
            && let Some(ref latest) = *guard
        {
            extract_screenshot_pixels(&latest.frame)
        } else {
            self.set_toast("No active video frame to capture", true);
            return;
        };

        let pixels = match pixels_res {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!("Screenshot capture failed: {err:#}");
                self.set_toast(format!("Screenshot failed: {err:#}"), true);
                return;
            }
        };

        let out_path = build_capture_path(capture_dir, false, &self.console_name, "png");
        let notice_tx = self.capture_notice_tx.clone();
        if let Err(err) = thread::Builder::new()
            .name("gnsh-screenshot".into())
            .spawn(move || match save_screenshot_png(&pixels, &out_path) {
                Ok(saved) => {
                    let _ = notice_tx.try_send(CaptureNotice::ScreenshotSaved(saved));
                }
                Err(err) => {
                    tracing::warn!("Screenshot save failed: {err:#}");
                    let _ = notice_tx.try_send(CaptureNotice::Error(format!(
                        "Screenshot save failed: {err:#}"
                    )));
                }
            })
        {
            self.set_toast(format!("Screenshot thread failed: {err:#}"), true);
        }
    }

    pub(crate) fn toggle_recording(&mut self, capture_dir: Option<&str>) {
        let Some(ref cmd_tx) = self.capture_cmd_tx else {
            return;
        };
        if self.is_recording() {
            self.recording_pending = false;
            self.recording_started_at = None;
            self.sync_capture_needed();
            let _ = cmd_tx.send(RecorderCommand::StopRecording);
        } else {
            let out_path = build_capture_path(capture_dir, true, &self.console_name, "mkv");
            self.recording_pending = true;
            self.recording_started_at = None;
            self.sync_capture_needed();
            self.idr_needed.store(true, Ordering::Relaxed);
            if let Err(err) = cmd_tx.send(RecorderCommand::StartRecording { path: out_path }) {
                self.recording_pending = false;
                self.sync_capture_needed();
                self.set_toast(format!("Recording unavailable: {err}"), true);
            } else {
                self.set_toast("Starting recording...", false);
            }
        }
    }

    pub(crate) fn save_replay_clip(&mut self, capture_dir: Option<&str>) {
        if !self.replay_enabled {
            self.set_toast("Replay Buffer is off (enable in Settings > Video)", true);
            return;
        }
        let Some(ref cmd_tx) = self.capture_cmd_tx else {
            return;
        };
        let out_path = build_capture_path(
            capture_dir,
            true,
            &format!("{}_clip", self.console_name),
            "mkv",
        );
        if let Err(err) = cmd_tx.send(RecorderCommand::SaveReplayClip { path: out_path }) {
            self.set_toast(format!("Replay clip unavailable: {err}"), true);
        } else {
            self.set_toast("Saving replay clip...", false);
        }
    }

    pub(crate) fn update_replay_settings(&mut self, enabled: bool, secs: u16) {
        self.replay_enabled = enabled;
        self.sync_capture_needed();
        if let Some(ref cmd_tx) = self.capture_cmd_tx {
            let _ = cmd_tx.send(RecorderCommand::SetReplayConfig { enabled, secs });
        }
    }

    pub(crate) fn poll_capture_notices(&mut self) -> bool {
        let mut changed = false;
        while let Ok(notice) = self.capture_notice_rx.try_recv() {
            changed = true;
            match notice {
                CaptureNotice::ScreenshotSaved(path) => {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("screenshot.png");
                    tracing::info!(path = %path.display(), "Saved screenshot");
                    self.set_toast(format!("Screenshot saved: {name}"), false);
                }
                CaptureNotice::RecordingStarted => {
                    self.recording_pending = false;
                    self.recording_started_at = Some(Instant::now());
                    self.sync_capture_needed();
                    self.set_toast("Recording started", false);
                }
                CaptureNotice::RecordingSaved(path) => {
                    self.recording_pending = false;
                    self.recording_started_at = None;
                    self.sync_capture_needed();
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("recording.mkv");
                    tracing::info!(path = %path.display(), "Saved MKV recording");
                    self.set_toast(format!("Recording saved: {name}"), false);
                }
                CaptureNotice::ClipSaved(path, secs) => {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("clip.mkv");
                    tracing::info!(path = %path.display(), secs, "Saved replay clip");
                    self.set_toast(format!("Clip saved ({secs:.0}s): {name}"), false);
                }
                CaptureNotice::Info(msg) => {
                    self.set_toast(msg, false);
                }
                CaptureNotice::RecordingAborted(err) => {
                    self.recording_pending = false;
                    self.recording_started_at = None;
                    self.sync_capture_needed();
                    self.set_toast(err, true);
                }
                CaptureNotice::Error(err) => {
                    self.set_toast(err, true);
                }
            }
        }
        if let Some((_, _, expires_at)) = self.capture_toast
            && Instant::now() >= expires_at
        {
            self.capture_toast = None;
            changed = true;
        }
        changed
    }

    pub(crate) fn shutdown(&mut self) {
        if self.session.is_none()
            && self.video_thread.is_none()
            && self.audio_thread.is_none()
            && self.recorder_thread.is_none()
            && self.mic_capture.is_none()
            && self.current_frame.is_none()
        {
            return;
        }
        tracing::info!(host = %self.console_host, "Shutting down active stream session");
        if let Some(cmd_tx) = self.capture_cmd_tx.take() {
            if self.is_recording() {
                let _ = cmd_tx.send(RecorderCommand::StopRecording);
            }
            let _ = cmd_tx.send(RecorderCommand::Shutdown);
        }
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
        if let Some(handle) = self.recorder_thread.take() {
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
    capture_needed: Arc<AtomicBool>,
    recorder_pkt_tx: Sender<RecorderPacket>,
    recorder_cmd_tx: Sender<RecorderCommand>,
    initial_dims: (u32, u32),
    stream_start: Instant,
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
    let mut param_sets_seeded = false;
    let mut last_reported_dims = args.initial_dims;

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

                if args.capture_needed.load(Ordering::Relaxed) {
                    let ts_us =
                        i64::try_from(args.stream_start.elapsed().as_micros()).unwrap_or(i64::MAX);
                    if let Err(TrySendError::Full(_)) =
                        args.recorder_pkt_tx.try_send(RecorderPacket::Video {
                            data: pkt.data.clone(),
                            ts_us,
                        })
                    {
                        tracing::debug!("Recorder video channel full; dropping capture packet");
                    }
                    param_sets_seeded = true;
                } else if !param_sets_seeded
                    && let Some(ps) = extract_parameter_sets(&pkt.data, args.codec)
                {
                    let _ = args
                        .recorder_pkt_tx
                        .try_send(RecorderPacket::ParamSetSeed(ps));
                    param_sets_seeded = true;
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
                            let dims = frame.dimensions();
                            if dims.0 > 0 && dims.1 > 0 && dims != last_reported_dims {
                                last_reported_dims = dims;
                                let _ =
                                    args.recorder_cmd_tx
                                        .try_send(RecorderCommand::SetVideoDims {
                                            width: dims.0,
                                            height: dims.1,
                                        });
                            }
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
    capture_needed: Arc<AtomicBool>,
    recorder_pkt_tx: Sender<RecorderPacket>,
    stream_start: Instant,
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
                    if args.capture_needed.load(Ordering::Relaxed) {
                        let ts_us = i64::try_from(args.stream_start.elapsed().as_micros())
                            .unwrap_or(i64::MAX);
                        let _ = args.recorder_pkt_tx.try_send(RecorderPacket::Audio {
                            samples: frame.samples.clone(),
                            sample_rate: frame.sample_rate,
                            channels: frame.channels,
                            ts_us,
                        });
                    }
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

struct RecorderWorkerArgs {
    codec: MediaCodec,
    width: u32,
    height: u32,
    replay_enabled: bool,
    replay_secs: u16,
    pkt_rx: Receiver<RecorderPacket>,
    cmd_rx: Receiver<RecorderCommand>,
    notice_tx: Sender<CaptureNotice>,
}

struct RecorderState {
    codec: MediaCodec,
    width: u32,
    height: u32,
    replay_enabled: bool,
    replay_buffer: ReplayRingBuffer,
    latest_param_sets: Vec<u8>,
    pending_recording: Option<PathBuf>,
    active_muxer: Option<MkvMuxer>,
    recording_base_ts_us: i64,
    recording_audio_rate: u32,
    recording_audio_channels: u32,
    audio_sample_rate: u32,
    audio_channels: u32,
    notice_tx: Sender<CaptureNotice>,
}

impl RecorderState {
    fn new(args: &RecorderWorkerArgs) -> Self {
        Self {
            codec: args.codec,
            width: args.width,
            height: args.height,
            replay_enabled: args.replay_enabled,
            replay_buffer: ReplayRingBuffer::new(args.codec, args.replay_secs),
            latest_param_sets: Vec::new(),
            pending_recording: None,
            active_muxer: None,
            recording_base_ts_us: 0,
            recording_audio_rate: 48_000,
            recording_audio_channels: 2,
            audio_sample_rate: 48_000,
            audio_channels: 2,
            notice_tx: args.notice_tx.clone(),
        }
    }

    fn handle_packet(&mut self, pkt: RecorderPacket) {
        match pkt {
            RecorderPacket::ParamSetSeed(ps) => {
                if !ps.is_empty() {
                    self.latest_param_sets = ps;
                }
            }
            RecorderPacket::Video { data, ts_us } => {
                self.handle_video_packet(data, ts_us);
            }
            RecorderPacket::Audio {
                samples,
                sample_rate,
                channels,
                ts_us,
            } => {
                self.handle_audio_packet(samples, sample_rate, channels, ts_us);
            }
        }
    }

    fn handle_video_packet(&mut self, data: Vec<u8>, ts_us: i64) {
        if let Some(ps) = extract_parameter_sets(&data, self.codec) {
            self.latest_param_sets = ps;
        }
        let header_only = is_header_only_annex_b(&data, self.codec);
        let is_kf = !header_only && contains_keyframe_nal(&data, self.codec);

        let recording_wants = self.active_muxer.is_some() || self.pending_recording.is_some();
        if self.replay_enabled {
            let for_rec = if recording_wants && !header_only {
                Some(data.clone())
            } else {
                None
            };
            self.replay_buffer.push_video(data, ts_us);
            let Some(rec_data) = for_rec else {
                return;
            };
            self.write_or_open_recording_video(&rec_data, ts_us, is_kf);
        } else if recording_wants && !header_only {
            self.write_or_open_recording_video(&data, ts_us, is_kf);
        }
    }

    fn write_or_open_recording_video(&mut self, data: &[u8], ts_us: i64, is_kf: bool) {
        if self.pending_recording.is_some() && is_kf {
            let Some(path) = self.pending_recording.take() else {
                return;
            };
            let extradata = if self.latest_param_sets.is_empty() {
                extract_parameter_sets(data, self.codec).unwrap_or_default()
            } else {
                self.latest_param_sets.clone()
            };
            match MkvMuxer::create(
                &path,
                self.codec,
                self.width,
                self.height,
                &extradata,
                self.audio_sample_rate,
                self.audio_channels,
            ) {
                Ok(mut muxer) => {
                    if let Err(err) = muxer.write_video_packet(data, 0, true) {
                        tracing::warn!("Initial recording frame write failed: {err:#}");
                        let _ = self
                            .notice_tx
                            .try_send(CaptureNotice::RecordingAborted(format!(
                                "Recording write failed: {err:#}"
                            )));
                        return;
                    }
                    self.recording_base_ts_us = ts_us;
                    self.recording_audio_rate = self.audio_sample_rate;
                    self.recording_audio_channels = self.audio_channels;
                    self.active_muxer = Some(muxer);
                    let _ = self.notice_tx.try_send(CaptureNotice::RecordingStarted);
                }
                Err(err) => {
                    tracing::warn!("MkvMuxer::create failed: {err:#}");
                    let _ = self
                        .notice_tx
                        .try_send(CaptureNotice::RecordingAborted(format!(
                            "Recording start failed: {err:#}"
                        )));
                }
            }
            return;
        }

        if let Some(muxer) = self.active_muxer.as_mut() {
            let rel_ts = ts_us.saturating_sub(self.recording_base_ts_us).max(0);
            if let Err(err) = muxer.write_video_packet(data, rel_ts, is_kf) {
                tracing::warn!("MkvMuxer::write_video_packet failed: {err:#}");
                self.active_muxer = None;
                let _ = self
                    .notice_tx
                    .try_send(CaptureNotice::RecordingAborted(format!(
                        "Recording write failed: {err:#}"
                    )));
            }
        }
    }

    fn handle_audio_packet(
        &mut self,
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u32,
        ts_us: i64,
    ) {
        if samples.is_empty() || sample_rate == 0 || channels == 0 {
            return;
        }
        self.audio_sample_rate = sample_rate;
        self.audio_channels = channels;

        if self.replay_enabled {
            let rec_copy = if self.active_muxer.is_some() {
                Some(samples.clone())
            } else {
                None
            };
            self.replay_buffer
                .push_audio(samples, sample_rate, channels, ts_us);
            if let Some(ref rec_samples) = rec_copy
                && sample_rate == self.recording_audio_rate
                && channels == self.recording_audio_channels
            {
                self.write_recording_audio(rec_samples, ts_us);
            }
        } else if self.active_muxer.is_some()
            && sample_rate == self.recording_audio_rate
            && channels == self.recording_audio_channels
        {
            self.write_recording_audio(&samples, ts_us);
        }
    }

    fn write_recording_audio(&mut self, samples: &[i16], ts_us: i64) {
        let Some(muxer) = self.active_muxer.as_mut() else {
            return;
        };
        let rel_ts = ts_us.saturating_sub(self.recording_base_ts_us).max(0);
        if let Err(err) = muxer.write_audio_pcm(samples, rel_ts) {
            tracing::warn!("MkvMuxer::write_audio_pcm failed: {err:#}");
            self.active_muxer = None;
            let _ = self
                .notice_tx
                .try_send(CaptureNotice::RecordingAborted(format!(
                    "Recording audio write failed: {err:#}"
                )));
        }
    }

    fn finish_recording(&mut self) {
        let had_pending = self.pending_recording.take().is_some();
        if let Some(muxer) = self.active_muxer.take() {
            match muxer.finish() {
                Ok(path) => {
                    let _ = self.notice_tx.try_send(CaptureNotice::RecordingSaved(path));
                }
                Err(err) => {
                    tracing::warn!("MkvMuxer::finish failed: {err:#}");
                    let _ = self
                        .notice_tx
                        .try_send(CaptureNotice::RecordingAborted(format!(
                            "Recording finalize failed: {err:#}"
                        )));
                }
            }
        } else if had_pending {
            let _ = self.notice_tx.try_send(CaptureNotice::Info(
                "Recording stopped before keyframe arrived".to_string(),
            ));
        }
    }

    fn save_replay_clip(&self, path: PathBuf) {
        if !self.replay_enabled {
            let _ = self.notice_tx.try_send(CaptureNotice::Error(
                "Replay Buffer is disabled in Settings > Video".to_string(),
            ));
            return;
        }
        let Some(snap) = self.replay_buffer.snapshot_for_clip() else {
            let _ = self.notice_tx.try_send(CaptureNotice::Info(
                "Replay Buffer has no buffered keyframe yet".to_string(),
            ));
            return;
        };
        let codec = self.codec;
        let width = self.width;
        let height = self.height;
        let notice_tx = self.notice_tx.clone();
        if let Err(err) = thread::Builder::new()
            .name("gnsh-clip-save".into())
            .spawn(
                move || match write_replay_clip_to_disk(&path, codec, width, height, snap) {
                    Ok((saved, secs)) => {
                        let _ = notice_tx.try_send(CaptureNotice::ClipSaved(saved, secs));
                    }
                    Err(err) => {
                        tracing::warn!("Replay clip save failed: {err:#}");
                        let _ = notice_tx.try_send(CaptureNotice::Error(format!(
                            "Replay clip save failed: {err:#}"
                        )));
                    }
                },
            )
        {
            let _ = self.notice_tx.try_send(CaptureNotice::Error(format!(
                "Replay clip thread failed: {err:#}"
            )));
        }
    }

    fn handle_command(&mut self, cmd: RecorderCommand, pkt_rx: &Receiver<RecorderPacket>) -> bool {
        match cmd {
            RecorderCommand::StartRecording { path } => {
                if self.active_muxer.is_some() {
                    self.finish_recording();
                }
                self.pending_recording = Some(path);
                true
            }
            RecorderCommand::StopRecording => {
                while let Ok(pkt) = pkt_rx.try_recv() {
                    self.handle_packet(pkt);
                }
                self.finish_recording();
                true
            }
            RecorderCommand::SaveReplayClip { path } => {
                while let Ok(pkt) = pkt_rx.try_recv() {
                    self.handle_packet(pkt);
                }
                self.save_replay_clip(path);
                true
            }
            RecorderCommand::SetReplayConfig { enabled, secs } => {
                self.replay_enabled = enabled;
                self.replay_buffer.set_duration_secs(secs);
                if !enabled {
                    self.replay_buffer.clear();
                }
                true
            }
            RecorderCommand::SetVideoDims { width, height } => {
                if width > 0 && height > 0 {
                    self.width = width;
                    self.height = height;
                }
                true
            }
            RecorderCommand::Shutdown => {
                while let Ok(pkt) = pkt_rx.try_recv() {
                    self.handle_packet(pkt);
                }
                self.finish_recording();
                false
            }
        }
    }
}

fn write_replay_clip_to_disk(
    path: &Path,
    codec: MediaCodec,
    width: u32,
    height: u32,
    snap: ReplayClipSnapshot,
) -> Result<(PathBuf, f32)> {
    let duration_secs = snap.duration_secs;
    let mut muxer = MkvMuxer::create(
        path,
        codec,
        width,
        height,
        &snap.extradata,
        snap.audio_sample_rate,
        snap.audio_channels,
    )?;
    for pkt in snap.packets {
        match pkt.kind {
            gnsh_media::BufferedPacketKind::Video {
                data, is_keyframe, ..
            } => {
                muxer.write_video_packet(&data, pkt.ts_us, is_keyframe)?;
            }
            gnsh_media::BufferedPacketKind::Audio {
                samples,
                sample_rate,
                channels,
            } => {
                if sample_rate == snap.audio_sample_rate && channels == snap.audio_channels {
                    muxer.write_audio_pcm(&samples, pkt.ts_us)?;
                }
            }
        }
    }
    let saved = muxer.finish()?;
    Ok((saved, duration_secs))
}

fn run_recorder_loop(args: RecorderWorkerArgs) {
    let mut state = RecorderState::new(&args);
    loop {
        select! {
            recv(args.cmd_rx) -> msg => match msg {
                Ok(cmd) => {
                    if !state.handle_command(cmd, &args.pkt_rx) {
                        break;
                    }
                }
                Err(_) => {
                    while let Ok(pkt) = args.pkt_rx.try_recv() {
                        state.handle_packet(pkt);
                    }
                    state.finish_recording();
                    break;
                }
            },
            recv(args.pkt_rx) -> msg => match msg {
                Ok(pkt) => state.handle_packet(pkt),
                Err(_) => {
                    state.finish_recording();
                    break;
                }
            },
        }
    }
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

    #[test]
    fn validates_custom_capture_directory_paths() {
        assert_eq!(
            validate_capture_dir("  C:\\Captures  "),
            Some(PathBuf::from("C:\\Captures"))
        );
        assert_eq!(
            validate_capture_dir("D:\\PS5\\Clips"),
            Some(PathBuf::from("D:\\PS5\\Clips"))
        );
        assert_eq!(validate_capture_dir(""), None);
        assert_eq!(validate_capture_dir("\\\\attacker\\share"), None);
        assert_eq!(validate_capture_dir("//attacker/share"), None);
        assert_eq!(validate_capture_dir("\\\\?\\C:\\Captures"), None);
        assert_eq!(validate_capture_dir("C:\\Captures\\..\\Windows"), None);
        assert_eq!(validate_capture_dir("captures\\relative"), None);
        assert_eq!(validate_capture_dir("C:\\Captures\0bad"), None);
    }
}
