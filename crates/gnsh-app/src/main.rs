#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod egui_d3d11;
mod storage;

use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError, select};
use egui_d3d11::EguiD3d11Painter;
use gnsh_input::{AppWindow, GamepadManager, KbmConfig, KbmEmulator, merge_controller_states};
use gnsh_media::{
    AudioOutput, D3D11Renderer, DecodedFrame, HapticsAudioOutput, MediaCodec, MicCapture,
    ScaleMode, VideoDecoder, list_playback_devices, volume_pct_to_gain,
};
use gnsh_proto::{
    ConsoleState, ControllerState, DiscoveredConsole, MAX_BITRATE_KBPS, MIN_BITRATE_KBPS,
    Ps5Session, Ps5SessionConfig, RegisteredConsole, SessionEvent, UDP_RECV_BUF_BYTES, VideoCodec,
    VideoPreset, decode_account_id_b64, discover_ps5_lan, exchange_redirect_url_for_account_id,
    init_lib, open_psn_login_in_browser, register_ps5, wake_ps5,
};
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use storage::{ConsoleStore, SavedConsole, SettingsStore, StoredSettings};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::HSTRING;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
const IDR_COOLDOWN: Duration = Duration::from_millis(500);
const LOW_LATENCY_INPUT_WAIT: Duration = Duration::from_millis(1);
const IDLE_UI_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const PIPELINE_HISTORY_LEN: usize = 120;
const MAX_LOG_FILE_BYTES: u64 = 2 * 1024 * 1024;
const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
const FILE_TYPE_UNKNOWN: u32 = 0;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn AttachConsole(dw_process_id: u32) -> i32;
    fn GetStdHandle(n_std_handle: u32) -> *mut std::ffi::c_void;
    fn GetFileType(h_file: *mut std::ffi::c_void) -> u32;
}

fn attach_parent_console() {
    // SAFETY: Querying STD_OUTPUT_HANDLE and calling AttachConsole(ATTACH_PARENT_PROCESS)
    // when stdout is unattached preserves redirected pipes/files while restoring interactive CLI output.
    unsafe {
        let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        let invalid = (-1isize) as *mut std::ffi::c_void;
        if stdout.is_null() || stdout == invalid || GetFileType(stdout) == FILE_TYPE_UNKNOWN {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliAction {
    RunGui,
    ExitSuccess,
}

fn main() -> Result<()> {
    match handle_cli()? {
        CliAction::ExitSuccess => return Ok(()),
        CliAction::RunGui => {}
    }
    run_gui()
}

fn handle_cli() -> Result<CliAction> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        attach_parent_console();
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_usage();
        return Ok(CliAction::ExitSuccess);
    }
    if args.as_slice() == ["--discover"] {
        init_lib().context("failed to initialize Remote Play library")?;
        let consoles = discover_ps5_lan(DISCOVERY_TIMEOUT).context("PS5 LAN discovery failed")?;
        if consoles.is_empty() {
            println!("No PS5 consoles found on LAN.");
        } else {
            for console in &consoles {
                let state = console_state_label(console.state);
                let app = if console.running_app_name.is_empty() {
                    String::new()
                } else {
                    format!(" | running: {}", console.running_app_name)
                };
                println!(
                    "{} ({}) [{}] | id={} | fw={}{}",
                    console.host_name,
                    console.addr,
                    state,
                    console.host_id,
                    console.system_version,
                    app
                );
            }
        }
        return Ok(CliAction::ExitSuccess);
    }
    if !args.is_empty() {
        eprintln!("Unknown arguments: {}", args.join(" "));
        print_usage();
        std::process::exit(2);
    }
    Ok(CliAction::RunGui)
}

fn print_usage() {
    println!(
        "gnsh-play - Native Windows PS5 Remote Play client\n\n\
         USAGE:\n    gnsh-play [OPTIONS]\n\n\
         OPTIONS:\n    -h, --help       Print this help message and exit\n        \
         --discover   Discover PS5 consoles on LAN and exit"
    );
}

struct AsyncLogMakeWriter {
    tx: crossbeam_channel::Sender<Vec<u8>>,
}

struct AsyncLogWriter {
    tx: crossbeam_channel::Sender<Vec<u8>>,
    buf: Vec<u8>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for AsyncLogMakeWriter {
    type Writer = AsyncLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        AsyncLogWriter {
            tx: self.tx.clone(),
            buf: Vec::with_capacity(256),
        }
    }
}

impl std::io::Write for AsyncLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
        Ok(())
    }
}

impl Drop for AsyncLogWriter {
    fn drop(&mut self) {
        if !self.buf.is_empty() {
            let _ = self.tx.try_send(std::mem::take(&mut self.buf));
        }
    }
}

fn default_log_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("gnsh-play").join("gnsh-play.log")
}

/// Initializes non-blocking stderr + `%APPDATA%\gnsh-play\gnsh-play.log` tracing and panic capture.
fn init_logging() -> PathBuf {
    let log_path = default_log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(meta) = std::fs::metadata(&log_path)
        && meta.len() > MAX_LOG_FILE_BYTES
    {
        let _ = std::fs::File::create(&log_path);
    }

    // Offload log I/O off time-critical UDP threads so console/disk latency never stalls Takion receive.
    let (log_tx, log_rx) = crossbeam_channel::bounded::<Vec<u8>>(1024);
    let writer_path = log_path.clone();
    let _ = thread::Builder::new()
        .name("gnsh-log-writer".into())
        .spawn(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&writer_path)
                .ok();
            let mut stderr = std::io::stderr();
            while let Ok(chunk) = log_rx.recv() {
                let _ = stderr.write_all(&chunk);
                if let Some(ref mut f) = file {
                    let _ = f.write_all(&chunk);
                }
            }
        });

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_ansi(false)
        .with_writer(AsyncLogMakeWriter { tx: log_tx })
        .try_init();

    let panic_path = log_path.clone();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "unknown panic payload"
        };
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        tracing::error!("PANIC at {location}: {payload}");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&panic_path)
        {
            let _ = writeln!(f, "PANIC at {location}: {payload}");
        }
        prev_hook(info);
    }));

    tracing::info!(
        "gnsh-play {} starting; log file: {}",
        env!("CARGO_PKG_VERSION"),
        log_path.display()
    );
    log_path
}

fn run_gui() -> Result<()> {
    let _log_path = init_logging();

    init_lib().context("failed to initialize Remote Play library")?;

    let store = ConsoleStore::default_store().context("failed to resolve console store path")?;
    let (saved_consoles, mut initial_banner) = match store.load() {
        Ok(list) => (list, None),
        Err(err) => (
            Vec::new(),
            Some(StatusBanner::error(format!(
                "Failed loading saved consoles ({}): {err:#}",
                store.path().display()
            ))),
        ),
    };

    let settings_store =
        SettingsStore::default_store().context("failed to resolve settings store path")?;
    let initial_settings = match settings_store.load() {
        Ok(loaded) => loaded,
        Err(err) => {
            tracing::warn!("Failed loading settings.json, using defaults: {err:#}");
            if initial_banner.is_none() {
                initial_banner = Some(StatusBanner::info(format!(
                    "Settings reset to defaults after read error: {err:#}"
                )));
            }
            StoredSettings::default()
        }
    };

    let mut window = AppWindow::new("gnsh-play", DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .context("failed creating application window")?;
    let (init_w, init_h) = window.size();
    let mut renderer = D3D11Renderer::new(window.hwnd(), init_w, init_h)
        .context("failed initializing D3D11 renderer")?;
    let mut painter = EguiD3d11Painter::new(renderer.device())
        .context("failed initializing egui D3D11 painter")?;
    let mut gamepads = GamepadManager::new().context("failed initializing SDL3 gamepad manager")?;
    let mut kbm = KbmEmulator::default();

    let egui_ctx = egui::Context::default();
    apply_theme(&egui_ctx);

    let mut app = AppState::new(
        store,
        settings_store,
        saved_consoles,
        initial_settings,
        initial_banner,
    );
    app.start_discovery();

    let mut raw_input = egui::RawInput::default();
    let start_time = Instant::now();
    let mut was_streaming = false;
    let mut prev_want_hdr = false;
    let mut last_present = Instant::now();

    loop {
        let waited_for_latency = if matches!(app.mode, AppMode::Streaming(_)) && !app.settings.vsync
        {
            if let AppMode::Streaming(ref stream) = app.mode {
                stream.wait_for_frame(LOW_LATENCY_INPUT_WAIT);
            }
            false
        } else {
            renderer.wait_for_frame_latency(100);
            true
        };

        let summary = window.poll_events(&mut gamepads, &mut kbm, &mut raw_input);
        if summary.quit {
            break;
        }
        if let Some((nw, nh)) = summary.resized
            && let Err(err) = renderer.resize(nw, nh)
        {
            tracing::warn!("D3D11 resize({nw}, {nh}) failed: {err:#}");
        }
        if summary.toggle_fullscreen
            && let Err(err) = window.toggle_fullscreen()
        {
            app.status_banner = Some(StatusBanner::error(format!(
                "Failed toggling fullscreen: {err:#}"
            )));
        }

        let prev_mouse_locked = window.is_relative_mouse_mode();
        let mut escape_for_shortcuts = summary.escape_pressed;
        if summary.escape_pressed && prev_mouse_locked {
            window.set_relative_mouse_mode(false);
            kbm.clear_state();
            escape_for_shortcuts = false;
        }
        if summary.toggle_mouse_lock && matches!(app.mode, AppMode::Streaming(_)) {
            if !app.settings.kbm_enabled {
                app.settings.kbm_enabled = true;
                app.persist_settings();
                window.set_relative_mouse_mode(true);
            } else {
                let next = !window.is_relative_mouse_mode();
                window.set_relative_mouse_mode(next);
                if !next {
                    kbm.clear_state();
                }
            }
        }

        app.handle_shortcuts(
            summary.toggle_hud,
            escape_for_shortcuts,
            summary.toggle_mic,
            summary.go_home,
        );
        if summary.audio_devices_changed {
            app.audio_devices = list_playback_devices();
            if let AppMode::Streaming(ref stream) = app.mode {
                stream.haptics_recheck.store(true, Ordering::Release);
                stream.audio_device_recheck.store(true, Ordering::Release);
            }
        }
        let got_new_frame =
            app.poll_background(&mut gamepads, &mut kbm, window.is_relative_mouse_mode());

        let is_streaming = matches!(app.mode, AppMode::Streaming(_));
        if was_streaming && !is_streaming {
            renderer.clear_video_cache();
        }
        was_streaming = is_streaming;

        let lock_state_changed = window.is_relative_mouse_mode() != prev_mouse_locked;

        let want_hdr = matches!(&app.mode, AppMode::Streaming(s) if s.codec == VideoCodec::HevcHdr);
        let hdr_mode_changed = want_hdr != prev_want_hdr;
        if hdr_mode_changed
            || (want_hdr && (summary.resized.is_some() || summary.toggle_fullscreen))
        {
            match renderer.set_hdr_output(want_hdr) {
                Ok(active) => {
                    if hdr_mode_changed {
                        tracing::info!(
                            want_hdr,
                            hdr_output_active = active,
                            "Updated D3D11 HDR output mode"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!("D3D11 HDR output transition failed: {err:#}");
                }
            }
            prev_want_hdr = want_hdr;
        }

        let should_render = !is_streaming
            || app.settings.vsync
            || got_new_frame
            || hdr_mode_changed
            || lock_state_changed
            || !raw_input.events.is_empty()
            || summary.resized.is_some()
            || summary.toggle_fullscreen
            || summary.toggle_hud
            || summary.toggle_mouse_lock
            || summary.toggle_mic
            || summary.go_home
            || summary.escape_pressed
            || last_present.elapsed() >= IDLE_UI_REFRESH_INTERVAL;
        if !should_render {
            continue;
        }

        if !waited_for_latency {
            renderer.wait_for_frame_latency(100);
        }

        raw_input.time = Some(start_time.elapsed().as_secs_f64());

        let clear_rgba = if is_streaming {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            [0.06, 0.07, 0.10, 1.0]
        };
        renderer.begin_frame(clear_rgba);

        let sharpness = f32::from(app.settings.sharpness_pct.min(100)) / 100.0;
        if let AppMode::Streaming(ref stream) = app.mode
            && let Some(ref latest) = stream.current_frame
            && let Err(err) =
                renderer.draw_video_frame(&latest.frame, sharpness, app.settings.scale_mode)
        {
            tracing::warn!("draw_video_frame failed: {err:#}");
        }

        let draw_egui = !is_streaming
            || matches!(
                &app.mode,
                AppMode::Streaming(s) if s.show_hud || s.pin_prompt.is_some() || s.osk_prompt.is_some()
            );
        let hdr_output_active = renderer.is_hdr_output_active();
        let mouse_locked = window.is_relative_mouse_mode();
        if draw_egui {
            renderer.bind_backbuffer();

            let gamepad_name = gamepads.connected_name().map(str::to_owned);
            let device_for_connect = renderer.device().clone();
            let full_output = egui_ctx.run_ui(raw_input.take(), |ui| {
                app.ui(
                    ui,
                    gamepad_name.as_deref(),
                    &device_for_connect,
                    hdr_output_active,
                    mouse_locked,
                );
            });

            handle_platform_commands(&window, &full_output.platform_output.commands);

            let clipped = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
            if let Err(err) = painter.update_textures(
                renderer.device(),
                renderer.context(),
                &full_output.textures_delta,
            ) {
                tracing::warn!("egui texture upload failed: {err:#}");
            }
            if let Err(err) = painter.paint(
                renderer.device(),
                renderer.context(),
                renderer.viewport_size(),
                full_output.pixels_per_point,
                hdr_output_active,
                &clipped,
            ) {
                tracing::warn!("egui paint failed: {err:#}");
            }
            painter.free_textures(&full_output.textures_delta);
        } else {
            let _ = raw_input.take();
        }

        let vsync = if is_streaming {
            app.settings.vsync
        } else {
            true
        };
        match renderer.present(vsync) {
            Ok(()) => {
                last_present = Instant::now();
                if let AppMode::Streaming(ref mut stream) = app.mode {
                    stream.record_present_latency();
                }
            }
            Err(err) => {
                tracing::warn!("D3D11 present failed: {err:#}");
            }
        }

        app.process_post_ui(&mut gamepads);

        if let AppMode::Streaming(ref mut stream) = app.mode
            && std::mem::take(&mut stream.mouse_lock_toggle_requested)
            && app.settings.kbm_enabled
            && stream.pin_prompt.is_none()
            && stream.osk_prompt.is_none()
        {
            let next = !window.is_relative_mouse_mode();
            window.set_relative_mouse_mode(next);
            if !next {
                kbm.clear_state();
            }
        }
        let allow_mouse_lock = app.settings.kbm_enabled
            && matches!(
                &app.mode,
                AppMode::Streaming(s) if s.pin_prompt.is_none() && s.osk_prompt.is_none()
            );
        if !allow_mouse_lock && window.is_relative_mouse_mode() {
            window.set_relative_mouse_mode(false);
            kbm.clear_state();
        }
    }

    window.set_relative_mouse_mode(false);
    kbm.clear_state();
    app.stop_stream(&mut gamepads, None);
    let _ = renderer.set_hdr_output(false);
    renderer.clear_video_cache();
    drop(app);
    drop(painter);
    drop(renderer);
    drop(window);
    Ok(())
}

fn handle_platform_commands(window: &AppWindow, commands: &[egui::OutputCommand]) {
    for cmd in commands {
        match cmd {
            egui::OutputCommand::CopyText(text) => {
                window.set_clipboard_text(text);
            }
            egui::OutputCommand::OpenUrl(open) => {
                open_url(&open.url);
            }
            _ => {}
        }
    }
}

fn open_url(url: &str) {
    let op = HSTRING::from("open");
    let target = HSTRING::from(url);
    let ret = unsafe {
        ShellExecuteW(
            Some(HWND::default()),
            &op,
            &target,
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if (ret.0 as isize) <= 32 {
        tracing::warn!(
            url,
            code = ret.0 as isize,
            "ShellExecuteW failed opening URL"
        );
    }
}

fn apply_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(15, 18, 25);
    visuals.window_fill = egui::Color32::from_rgb(22, 26, 36);
    visuals.extreme_bg_color = egui::Color32::from_rgb(10, 13, 19);
    visuals.faint_bg_color = egui::Color32::from_rgb(24, 29, 40);
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 57, 77));
    visuals.selection.bg_fill = egui::Color32::from_rgb(0, 112, 209);
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(96, 175, 255));
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(218, 224, 236));
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(33, 40, 54);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(33, 40, 54);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(46, 56, 77);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(46, 56, 77);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0, 112, 209);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0, 112, 209);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
    ctx.set_visuals(visuals);
}

type StreamSettings = StoredSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
struct StatusBanner {
    kind: BannerKind,
    text: String,
}

impl StatusBanner {
    fn info(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Info,
            text: text.into(),
        }
    }

    fn success(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Success,
            text: text.into(),
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Error,
            text: text.into(),
        }
    }
}

struct RegisterModal {
    host: String,
    redirect_url: String,
    psn_account_id: String,
    pin_input: String,
    notice: Option<String>,
    error: Option<String>,
    exchange_rx: Option<Receiver<std::result::Result<String, String>>>,
    register_rx: Option<Receiver<std::result::Result<RegisteredConsole, String>>>,
}

impl RegisterModal {
    fn new(host: String) -> Self {
        Self {
            host,
            redirect_url: String::new(),
            psn_account_id: String::new(),
            pin_input: String::new(),
            notice: None,
            error: None,
            exchange_rx: None,
            register_rx: None,
        }
    }
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
        // frame wakeups and 250 Hz controller polling; balanced cleanly by timeEndPeriod(1) in Drop.
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

struct LatestFrame {
    frame: DecodedFrame,
    decode_ms: f32,
    is_hw: bool,
    arrived_at: Instant,
}

struct LoginPinPrompt {
    pin_input: String,
    incorrect: bool,
}

struct OskPrompt {
    text: String,
    last_sent_text: String,
    needs_focus: bool,
    cancel_requested: bool,
}

struct ActiveStream {
    console_name: String,
    console_host: String,
    codec: VideoCodec,
    session: Option<Ps5Session>,
    events_rx: Receiver<SessionEvent>,
    frame_sync: Arc<(Mutex<Option<LatestFrame>>, Condvar)>,
    current_frame: Option<LatestFrame>,
    current_frame_pending_measure: bool,
    frames_decoded: Arc<AtomicU64>,
    decoder_lag_frames: Arc<AtomicU64>,
    frames_lost_total: Arc<AtomicU64>,
    idr_needed: Arc<AtomicBool>,
    video_init_error: Arc<Mutex<Option<String>>>,
    stop_flag: Arc<AtomicBool>,
    video_thread: Option<JoinHandle<()>>,
    audio_thread: Option<JoinHandle<()>>,
    usb_haptics_active: Arc<AtomicBool>,
    haptics_recheck: Arc<AtomicBool>,
    haptics_device_label: Arc<Mutex<Option<String>>>,
    volume_pct: Arc<AtomicU8>,
    audio_device: Arc<Mutex<Option<String>>>,
    audio_device_recheck: Arc<AtomicBool>,
    mic_capture: Option<MicCapture>,
    mic_muted: bool,
    mic_toggle_requested: bool,
    mic_error: Option<String>,
    _sleep_inhibitor: SleepInhibitor,
    connected: bool,
    show_hud: bool,
    mouse_lock_toggle_requested: bool,
    kbm_was_active: bool,
    disconnect_requested: bool,
    go_home_requested: bool,
    pin_prompt: Option<LoginPinPrompt>,
    osk_prompt: Option<OskPrompt>,
    fps: f32,
    fps_window_start: Instant,
    fps_last_count: u64,
    measured_mbps: f32,
    rtt_ms: f32,
    packet_loss_pct: f32,
    loss_window_start: Instant,
    loss_history: VecDeque<f32>,
    pipeline_history: VecDeque<f32>,
    pipe_p50_ms: f32,
    pipe_p95_ms: f32,
    bytes_last_count: u64,
    udp_rcvbuf_kib: Option<i32>,
    last_idr_request: Instant,
    last_seen_lost: u64,
    packet_loss_notice_until: Option<Instant>,
    rumble_active_until: Option<Instant>,
}

impl ActiveStream {
    fn start(
        console: &SavedConsole,
        settings: &StreamSettings,
        device: ID3D11Device,
    ) -> Result<Self> {
        let config = Ps5SessionConfig {
            host: console.host.clone(),
            regist_key: console.regist_key,
            rp_key: console.rp_key,
            preset: settings.preset,
            codec: settings.codec,
            bitrate_kbps: settings.bitrate_kbps,
            enable_dualsense: true,
        };
        let (session, channels) = Ps5Session::start(config)
            .with_context(|| format!("failed starting Remote Play session to {}", console.host))?;

        tracing::info!(
            host = %console.host,
            preset = ?settings.preset,
            codec = ?settings.codec,
            bitrate_kbps = settings.bitrate_kbps,
            "Started Remote Play stream session"
        );

        let media_codec = match settings.codec {
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
            device,
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
            console_host: console.host.clone(),
            codec: settings.codec,
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
            mic_muted: true,
            mic_toggle_requested: false,
            mic_error: None,
            _sleep_inhibitor: sleep_inhibitor,
            connected: false,
            show_hud: true,
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
            bytes_last_count: 0,
            udp_rcvbuf_kib: None,
            last_idr_request: Instant::now() - IDR_COOLDOWN,
            last_seen_lost: 0,
            packet_loss_notice_until: None,
            rumble_active_until: None,
        })
    }

    fn wait_for_frame(&self, timeout: Duration) {
        let (lock, cvar) = &*self.frame_sync;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let _ = cvar.wait_timeout_while(guard, timeout, |slot| slot.is_none());
        }
    }

    fn take_latest_frame(&mut self) -> bool {
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

    fn record_present_latency(&mut self) {
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

    fn toggle_mic(&mut self, gamepads: &GamepadManager) {
        if !self.connected {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let target_muted = !self.mic_muted;
        if !target_muted && self.mic_capture.is_none() {
            match MicCapture::new_default() {
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

    fn shutdown(&mut self) {
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
    device: ID3D11Device,
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
    let mut decoder = match VideoDecoder::new(args.codec, Some(args.device)) {
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

enum AppMode {
    Home,
    Streaming(Box<ActiveStream>),
}

enum HomeAction {
    ConnectSaved(usize),
    WakeSaved(usize),
    DeleteSaved(usize),
    OpenRegister(String),
}

struct AppState {
    store: ConsoleStore,
    settings_store: SettingsStore,
    saved_consoles: Vec<SavedConsole>,
    discovered_consoles: Vec<DiscoveredConsole>,
    audio_devices: Vec<String>,
    discovery_rx: Option<Receiver<std::result::Result<Vec<DiscoveredConsole>, String>>>,
    wake_rx: Option<Receiver<std::result::Result<String, String>>>,
    settings: StreamSettings,
    status_banner: Option<StatusBanner>,
    register_modal: Option<RegisterModal>,
    mode: AppMode,
    pending_home_action: Option<HomeAction>,
    pending_stream_connect: Option<usize>,
}

impl AppState {
    fn new(
        store: ConsoleStore,
        settings_store: SettingsStore,
        saved_consoles: Vec<SavedConsole>,
        settings: StreamSettings,
        status_banner: Option<StatusBanner>,
    ) -> Self {
        Self {
            store,
            settings_store,
            saved_consoles,
            discovered_consoles: Vec::new(),
            audio_devices: list_playback_devices(),
            discovery_rx: None,
            wake_rx: None,
            settings,
            status_banner,
            register_modal: None,
            mode: AppMode::Home,
            pending_home_action: None,
            pending_stream_connect: None,
        }
    }

    fn persist_settings(&self) {
        if let Err(err) = self.settings_store.save(&self.settings) {
            tracing::warn!("Failed saving settings.json: {err:#}");
        }
    }

    fn start_discovery(&mut self) {
        if self.discovery_rx.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::Builder::new()
            .name("gnsh-lan-discover".into())
            .spawn(move || {
                let res = discover_ps5_lan(DISCOVERY_TIMEOUT).map_err(|e| format!("{e:#}"));
                let _ = tx.send(res);
            })
            .ok();
        self.discovery_rx = Some(rx);
    }

    fn start_wake(&mut self, console: &SavedConsole) {
        if self.wake_rx.is_some() {
            return;
        }
        let host = console.host.clone();
        let name = console.name.clone();
        let regist_key_hex = console.regist_key_hex();
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::Builder::new()
            .name("gnsh-wake".into())
            .spawn(move || {
                let res = wake_ps5(&host, &regist_key_hex)
                    .map(|()| format!("Wake-up packet sent to {name} ({host})."))
                    .map_err(|e| format!("Wake failed for {name}: {e:#}"));
                let _ = tx.send(res);
            })
            .ok();
        self.wake_rx = Some(rx);
    }

    fn handle_shortcuts(
        &mut self,
        toggle_hud: bool,
        escape_pressed: bool,
        toggle_mic: bool,
        go_home: bool,
    ) {
        match &mut self.mode {
            AppMode::Home => {
                if escape_pressed && self.register_modal.is_some() {
                    self.register_modal = None;
                }
            }
            AppMode::Streaming(stream) => {
                if toggle_hud {
                    stream.show_hud = !stream.show_hud;
                }
                if toggle_mic {
                    stream.mic_toggle_requested = true;
                }
                if go_home && stream.osk_prompt.is_none() && stream.pin_prompt.is_none() {
                    stream.go_home_requested = true;
                }
                if escape_pressed {
                    if let Some(osk) = stream.osk_prompt.as_mut() {
                        osk.cancel_requested = true;
                    } else {
                        stream.disconnect_requested = true;
                    }
                }
            }
        }
    }

    fn poll_background(
        &mut self,
        gamepads: &mut GamepadManager,
        kbm: &mut KbmEmulator,
        mouse_locked: bool,
    ) -> bool {
        let mut got_new_frame = false;
        let mut ui_dirty = false;
        if let Some(rx) = &self.discovery_rx
            && let Ok(res) = rx.try_recv()
        {
            self.discovery_rx = None;
            match res {
                Ok(list) => {
                    self.discovered_consoles = list;
                    let mut updated_ip = false;
                    for disc in &self.discovered_consoles {
                        if !disc.addr.is_empty()
                            && !disc.host_id.is_empty()
                            && let Some(saved) = self.saved_consoles.iter_mut().find(|s| {
                                s.id.eq_ignore_ascii_case(disc.host_id.trim())
                                    || format_mac_id(s.mac)
                                        .eq_ignore_ascii_case(disc.host_id.trim())
                            })
                            && saved.host != disc.addr
                        {
                            saved.host = disc.addr.clone();
                            updated_ip = true;
                        }
                    }
                    if updated_ip && let Err(err) = self.store.save(&self.saved_consoles) {
                        tracing::warn!("Failed persisting updated console IP: {err:#}");
                    }

                    if matches!(
                        self.status_banner,
                        Some(StatusBanner {
                            kind: BannerKind::Info,
                            ..
                        })
                    ) {
                        self.status_banner = if self.discovered_consoles.is_empty() {
                            Some(StatusBanner::info(
                                "LAN scan finished: no PS5 consoles found.",
                            ))
                        } else {
                            None
                        };
                    }
                }
                Err(err) => {
                    self.status_banner =
                        Some(StatusBanner::error(format!("LAN discovery failed: {err}")));
                }
            }
        }

        if let Some(rx) = &self.wake_rx
            && let Ok(res) = rx.try_recv()
        {
            self.wake_rx = None;
            self.status_banner = Some(match res {
                Ok(msg) => StatusBanner::success(msg),
                Err(err) => StatusBanner::error(err),
            });
        }

        let mut registered_console: Option<RegisteredConsole> = None;
        if let Some(modal) = &mut self.register_modal {
            if let Some(rx) = &modal.exchange_rx
                && let Ok(res) = rx.try_recv()
            {
                modal.exchange_rx = None;
                match res {
                    Ok(acct_id) => {
                        modal.psn_account_id = acct_id;
                        modal.error = None;
                        modal.notice = Some("PSN Account ID retrieved.".into());
                    }
                    Err(err) => {
                        modal.notice = None;
                        modal.error = Some(format!("Account exchange failed: {err}"));
                    }
                }
            }

            if let Some(rx) = &modal.register_rx
                && let Ok(res) = rx.try_recv()
            {
                modal.register_rx = None;
                match res {
                    Ok(reg) => {
                        registered_console = Some(reg);
                    }
                    Err(err) => {
                        modal.notice = None;
                        modal.error = Some(format!("Registration failed: {err}"));
                    }
                }
            }
        }

        if let Some(reg) = registered_console {
            let saved = saved_from_registered(reg, &self.discovered_consoles);
            let label = format!("{} ({})", saved.name, saved.host);
            upsert_saved_console(&mut self.saved_consoles, saved);
            match self.store.save(&self.saved_consoles) {
                Ok(()) => {
                    self.register_modal = None;
                    self.status_banner = Some(StatusBanner::success(format!(
                        "Registered and saved {label}."
                    )));
                }
                Err(err) => {
                    if let Some(modal) = &mut self.register_modal {
                        modal.error = Some(format!("Registered, but save failed: {err:#}"));
                    }
                }
            }
        }

        let mut stream_quit_banner: Option<StatusBanner> = None;
        if let AppMode::Streaming(ref mut stream) = self.mode {
            if let Ok(mut err_guard) = stream.video_init_error.lock()
                && let Some(err_msg) = err_guard.take()
            {
                stream_quit_banner = Some(StatusBanner::error(err_msg));
            }

            while let Ok(ev) = stream.events_rx.try_recv() {
                match ev {
                    SessionEvent::Connected => {
                        stream.connected = true;
                        tracing::info!(host = %stream.console_host, "Stream session connected");
                        gamepads.set_mic_led(stream.mic_muted);
                        if let Some(session) = stream.session.as_mut() {
                            match session.tune_udp_recv_buffer(UDP_RECV_BUF_BYTES) {
                                Ok(actual) => {
                                    tracing::info!(
                                        "Tuned Takion UDP SO_RCVBUF to {} KiB",
                                        actual / 1024
                                    );
                                    stream.udp_rcvbuf_kib = Some(actual / 1024);
                                }
                                Err(err) => {
                                    tracing::warn!("Failed tuning UDP SO_RCVBUF: {err:#}");
                                }
                            }
                        }
                    }
                    SessionEvent::Nickname(nick) => {
                        if !nick.trim().is_empty() {
                            stream.console_name = nick;
                        }
                    }
                    SessionEvent::Rumble { left, right } => {
                        gamepads.set_rumble(left, right);
                        stream.rumble_active_until = if left > 0 || right > 0 {
                            Some(Instant::now() + Duration::from_millis(250))
                        } else {
                            None
                        };
                    }
                    SessionEvent::Led { r, g, b } => {
                        gamepads.set_led(r, g, b);
                    }
                    SessionEvent::TriggerEffects {
                        type_left,
                        type_right,
                        left,
                        right,
                    } => {
                        gamepads.set_trigger_effects(type_left, &left, type_right, &right);
                    }
                    SessionEvent::LoginPinRequested { incorrect } => {
                        stream.pin_prompt = Some(LoginPinPrompt {
                            pin_input: String::new(),
                            incorrect,
                        });
                        ui_dirty = true;
                    }
                    SessionEvent::KeyboardOpen { text } => {
                        stream.osk_prompt = Some(OskPrompt {
                            last_sent_text: text.clone(),
                            text,
                            needs_focus: true,
                            cancel_requested: false,
                        });
                        ui_dirty = true;
                    }
                    SessionEvent::KeyboardTextChange { text } => {
                        if let Some(osk) = stream.osk_prompt.as_mut()
                            && text != osk.last_sent_text
                        {
                            osk.text = text.clone();
                            osk.last_sent_text = text;
                            ui_dirty = true;
                        }
                    }
                    SessionEvent::KeyboardRemoteClose => {
                        if stream.osk_prompt.take().is_some() {
                            ui_dirty = true;
                        }
                    }
                    SessionEvent::Quit { reason, is_error } => {
                        if is_error {
                            tracing::warn!(host = %stream.console_host, "Stream session ended with error: {reason}");
                        } else {
                            tracing::info!(host = %stream.console_host, "Stream session disconnected: {reason}");
                        }
                        stream_quit_banner = Some(if is_error {
                            StatusBanner::error(format!("Session ended: {reason}"))
                        } else {
                            StatusBanner::info(format!("Disconnected: {reason}"))
                        });
                    }
                }
            }

            if stream_quit_banner.is_none() {
                if std::mem::take(&mut stream.go_home_requested)
                    && stream.connected
                    && let Some(session) = stream.session.as_mut()
                    && let Err(err) = session.go_home()
                {
                    tracing::debug!("go_home failed: {err:#}");
                }

                if stream
                    .osk_prompt
                    .as_ref()
                    .is_some_and(|o| o.cancel_requested)
                {
                    if let Some(session) = stream.session.as_mut()
                        && let Err(err) = session.keyboard_reject()
                    {
                        tracing::debug!("keyboard_reject failed: {err:#}");
                    }
                    stream.osk_prompt = None;
                    ui_dirty = true;
                }

                let gamepad_mic_toggle = gamepads.take_mic_toggle();
                let ui_mic_toggle = std::mem::take(&mut stream.mic_toggle_requested);
                if gamepad_mic_toggle || ui_mic_toggle {
                    stream.toggle_mic(gamepads);
                }

                if stream.connected
                    && !stream.mic_muted
                    && let Some(cap) = stream.mic_capture.as_mut()
                    && let Some(session) = stream.session.as_mut()
                {
                    cap.drain_frames(|pcm| session.send_mic_pcm(pcm));
                }

                if stream
                    .rumble_active_until
                    .is_some_and(|until| Instant::now() >= until)
                {
                    gamepads.set_rumble(0, 0);
                    stream.rumble_active_until = None;
                }

                let lost_total = stream.frames_lost_total.load(Ordering::Relaxed);
                if lost_total > stream.last_seen_lost {
                    stream.last_seen_lost = lost_total;
                    stream.packet_loss_notice_until = Some(Instant::now() + Duration::from_secs(3));
                }

                if stream.idr_needed.swap(false, Ordering::Relaxed)
                    && stream.last_idr_request.elapsed() >= IDR_COOLDOWN
                    && let Some(session) = stream.session.as_mut()
                {
                    if let Err(err) = session.request_idr() {
                        tracing::debug!("request_idr failed: {err:#}");
                    }
                    stream.last_idr_request = Instant::now();
                }

                got_new_frame = stream.take_latest_frame();

                if stream.loss_window_start.elapsed().as_secs_f32() >= 0.2
                    && let Some(session) = stream.session.as_ref()
                {
                    let t = session.telemetry();
                    stream.loss_history.push_back(t.packet_loss_pct);
                    if stream.loss_history.len() > 10 {
                        stream.loss_history.pop_front();
                    }
                    let sum: f32 = stream.loss_history.iter().sum();
                    stream.packet_loss_pct = sum / (stream.loss_history.len() as f32);
                    stream.rtt_ms = t.rtt_ms;
                    stream.loss_window_start = Instant::now();
                }

                let elapsed = stream.fps_window_start.elapsed().as_secs_f32();
                if elapsed >= 0.5 {
                    let count = stream.frames_decoded.load(Ordering::Relaxed);
                    stream.fps = (count.saturating_sub(stream.fps_last_count)) as f32 / elapsed;
                    stream.fps_last_count = count;
                    stream.fps_window_start = Instant::now();

                    if let Some(session) = stream.session.as_ref() {
                        let t = session.telemetry();
                        let delta_bytes =
                            t.video_bytes_total.saturating_sub(stream.bytes_last_count);
                        stream.bytes_last_count = t.video_bytes_total;
                        let rolling_mbps = (delta_bytes as f32 * 8.0) / (elapsed * 1_000_000.0);
                        stream.measured_mbps = if rolling_mbps > 0.05 {
                            rolling_mbps
                        } else {
                            t.measured_mbps
                        };
                    }
                }

                let pad_state = gamepads.poll_state(self.settings.touchpad_touch);
                let kbm_active = self.settings.kbm_enabled
                    && mouse_locked
                    && stream.pin_prompt.is_none()
                    && stream.osk_prompt.is_none();
                let kbm_state = if kbm_active {
                    Some(kbm.sample_state(
                        KbmConfig {
                            sensitivity_pct: self.settings.kbm_sensitivity_pct,
                            deadzone_pct: self.settings.kbm_deadzone_pct,
                            invert_y: self.settings.kbm_invert_y,
                        },
                        &self.settings.kbm_bindings,
                    ))
                } else {
                    None
                };
                let combined = match (pad_state, kbm_state.as_ref()) {
                    (Some(pad), Some(kb)) => Some(merge_controller_states(&pad, kb)),
                    (Some(pad), None) => Some(pad),
                    (None, Some(_)) => kbm_state,
                    (None, None) => {
                        if stream.kbm_was_active {
                            Some(ControllerState::default())
                        } else {
                            None
                        }
                    }
                };
                stream.kbm_was_active = kbm_active;

                if let Some(ctrl_state) = combined
                    && let Some(session) = stream.session.as_mut()
                    && let Err(err) = session.send_controller_state(&ctrl_state)
                {
                    tracing::debug!("send_controller_state failed: {err:#}");
                }
            }
        }

        if let Some(banner) = stream_quit_banner {
            self.stop_stream(gamepads, Some(banner));
        }
        got_new_frame || ui_dirty
    }

    fn process_post_ui(&mut self, gamepads: &mut GamepadManager) {
        if let AppMode::Streaming(ref stream) = self.mode
            && stream.disconnect_requested
        {
            self.stop_stream(
                gamepads,
                Some(StatusBanner::info("Disconnected from console.")),
            );
            return;
        }

        if let Some(action) = self.pending_home_action.take() {
            match action {
                HomeAction::ConnectSaved(idx) => {
                    self.pending_stream_connect = Some(idx);
                }
                HomeAction::WakeSaved(idx) => {
                    if let Some(console) = self.saved_consoles.get(idx).cloned() {
                        self.start_wake(&console);
                    }
                }
                HomeAction::DeleteSaved(idx) => {
                    if idx < self.saved_consoles.len() {
                        let removed = self.saved_consoles.remove(idx);
                        match self.store.save(&self.saved_consoles) {
                            Ok(()) => {
                                self.status_banner = Some(StatusBanner::info(format!(
                                    "Deleted saved console {}.",
                                    removed.name
                                )));
                            }
                            Err(err) => {
                                self.saved_consoles.insert(idx, removed);
                                self.status_banner = Some(StatusBanner::error(format!(
                                    "Failed saving console store: {err:#}"
                                )));
                            }
                        }
                    }
                }
                HomeAction::OpenRegister(host) => {
                    self.register_modal = Some(RegisterModal::new(host));
                }
            }
        }
    }

    fn stop_stream(&mut self, gamepads: &mut GamepadManager, banner: Option<StatusBanner>) {
        if let AppMode::Streaming(mut stream) = std::mem::replace(&mut self.mode, AppMode::Home) {
            stream.shutdown();
            gamepads.set_rumble(0, 0);
            gamepads.set_led(0, 0, 0);
            gamepads.set_mic_led(false);
            gamepads.clear_trigger_effects();
            if let Some(b) = banner {
                self.status_banner = Some(b);
            }
        }
    }

    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        gamepad_name: Option<&str>,
        device: &ID3D11Device,
        hdr_output_active: bool,
        mouse_locked: bool,
    ) {
        if let Some(idx) = self.pending_stream_connect.take() {
            self.begin_stream(idx, device.clone());
        }

        let prev_settings = self.settings.clone();
        match &mut self.mode {
            AppMode::Home => {
                self.render_home_ui(ui, gamepad_name);
            }
            AppMode::Streaming(stream) => {
                render_streaming_ui(
                    ui,
                    stream,
                    gamepad_name,
                    &mut self.settings,
                    &self.audio_devices,
                    hdr_output_active,
                    mouse_locked,
                );
            }
        }
        if self.settings != prev_settings {
            if let AppMode::Streaming(ref stream) = self.mode {
                if self.settings.volume_pct != prev_settings.volume_pct {
                    stream
                        .volume_pct
                        .store(self.settings.volume_pct.min(100), Ordering::Relaxed);
                }
                if self.settings.audio_device != prev_settings.audio_device {
                    if let Ok(mut guard) = stream.audio_device.lock() {
                        *guard = self.settings.audio_device.clone();
                    }
                    stream.audio_device_recheck.store(true, Ordering::Release);
                }
            }
            self.persist_settings();
        }
    }

    fn begin_stream(&mut self, saved_idx: usize, device: ID3D11Device) {
        let Some(console) = self.saved_consoles.get(saved_idx) else {
            self.status_banner = Some(StatusBanner::error("Selected console no longer exists."));
            return;
        };
        match ActiveStream::start(console, &self.settings, device) {
            Ok(stream) => {
                self.status_banner = None;
                self.register_modal = None;
                self.mode = AppMode::Streaming(Box::new(stream));
            }
            Err(err) => {
                tracing::warn!(
                    "Unable to connect to {} ({}): {err:#}",
                    console.name,
                    console.host
                );
                self.status_banner = Some(StatusBanner::error(format!(
                    "Unable to connect to {}: {err:#}",
                    console.name
                )));
            }
        }
    }

    fn render_home_ui(&mut self, ui: &mut egui::Ui, gamepad_name: Option<&str>) {
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(20, 25, 35))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(38, 46, 64)))
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("gnsh-play")
                            .size(20.0)
                            .strong()
                            .color(egui::Color32::from_rgb(240, 245, 255)),
                    );
                    ui.add_space(6.0);

                    let (badge_bg, badge_fg, badge_text) = match gamepad_name {
                        Some(name) => (
                            egui::Color32::from_rgb(22, 66, 46),
                            egui::Color32::from_rgb(138, 236, 182),
                            format!("Controller: {name}"),
                        ),
                        None => (
                            egui::Color32::from_rgb(36, 42, 56),
                            egui::Color32::from_rgb(165, 176, 198),
                            "No Controller".to_string(),
                        ),
                    };
                    egui::Frame::new()
                        .fill(badge_bg)
                        .corner_radius(egui::CornerRadius::same(12))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(badge_text).small().color(badge_fg));
                        });

                    ui.separator();

                    let scanning = self.discovery_rx.is_some();
                    if scanning {
                        ui.spinner();
                    }
                    if ui
                        .add_enabled(!scanning, egui::Button::new("Discover LAN"))
                        .clicked()
                    {
                        self.status_banner =
                            Some(StatusBanner::info("Scanning LAN for PS5 consoles..."));
                        self.start_discovery();
                    }

                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("+ Register PS5")
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            )
                            .fill(egui::Color32::from_rgb(0, 112, 209)),
                        )
                        .clicked()
                    {
                        self.register_modal = Some(RegisterModal::new(String::new()));
                    }

                    ui.separator();

                    render_preset_controls(ui, &mut self.settings.preset);

                    ui.separator();

                    ui.label(egui::RichText::new("Codec:").weak());
                    ui.selectable_value(&mut self.settings.codec, VideoCodec::Hevc, "HEVC");
                    ui.selectable_value(
                        &mut self.settings.codec,
                        VideoCodec::HevcHdr,
                        "HEVC HDR10",
                    )
                    .on_hover_text(
                        "10-bit HEVC HDR10: native passthrough on Windows HDR displays, automatic ACES tone-mapping on SDR monitors.",
                    );
                    ui.selectable_value(&mut self.settings.codec, VideoCodec::H264, "H264");

                    ui.separator();
                    render_scale_selector(ui, &mut self.settings.scale_mode);

                    ui.separator();

                    ui.add(
                        egui::Slider::new(
                            &mut self.settings.bitrate_kbps,
                            MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS,
                        )
                        .step_by(1_000.0)
                        .clamping(egui::SliderClamping::Always)
                        .suffix(" kbps")
                        .text("Bitrate"),
                    )
                    .on_hover_text("PS5 Remote Play FEC limit: 2,000-100,000 kbps (100 Mbps max)");

                    ui.scope(|ui| {
                        ui.spacing_mut().slider_width = 70.0;
                        ui.add(
                            egui::Slider::new(&mut self.settings.sharpness_pct, 0..=100)
                                .clamping(egui::SliderClamping::Always)
                                .suffix("%")
                                .text("CAS"),
                        )
                        .on_hover_text(
                            "Contrast Adaptive Sharpening (single-pass HLSL, 0% = off)",
                        );
                        ui.add(
                            egui::Slider::new(&mut self.settings.volume_pct, 0..=100)
                                .clamping(egui::SliderClamping::Always)
                                .suffix("%")
                                .text("Vol"),
                        )
                        .on_hover_text("Stream audio playback volume (0-100%)");
                    });

                    ui.label(egui::RichText::new("Audio:").weak());
                    render_audio_device_combo(
                        ui,
                        "home_audio_device",
                        &self.audio_devices,
                        &mut self.settings.audio_device,
                    );

                    ui.checkbox(&mut self.settings.vsync, "VSync");
                    ui.checkbox(&mut self.settings.touchpad_touch, "Touchpad Touch")
                        .on_hover_text(
                            "Off: click-only (prevents touch registering as click in GTA 6). On: capacitive swipe/touch.",
                        );

                    ui.separator();
                    render_kbm_controls(ui, &mut self.settings, None);
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(15, 18, 25))
                    .inner_margin(egui::Margin::symmetric(18, 14)),
            )
            .show(ui, |ui| {
                if let Some(banner) = self.status_banner.clone() {
                    let dismiss = render_status_banner(ui, &banner);
                    if dismiss {
                        self.status_banner = None;
                    }
                    ui.add_space(10.0);
                }

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Saved Consoles");
                            ui.label(
                                egui::RichText::new(format!(
                                    "({})  {}",
                                    self.saved_consoles.len(),
                                    self.store.path().display()
                                ))
                                .small()
                                .weak(),
                            );
                        });
                        ui.add_space(6.0);

                        if self.saved_consoles.is_empty() {
                            card_frame(ui.style()).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    egui::RichText::new(
                                        "No saved PS5 consoles yet. Click \"Discover LAN\" or \"+ Register PS5\" to link a console.",
                                    )
                                    .weak(),
                                );
                            });
                        } else {
                            let waking = self.wake_rx.is_some();
                            for (idx, saved) in self.saved_consoles.iter().enumerate() {
                                let live_match =
                                    find_discovered_for_saved(&self.discovered_consoles, saved);
                                render_saved_console_card(
                                    ui,
                                    idx,
                                    saved,
                                    live_match,
                                    waking,
                                    &mut self.pending_home_action,
                                );
                                ui.add_space(8.0);
                            }
                        }

                        ui.add_space(16.0);
                        ui.horizontal(|ui| {
                            ui.heading("Discovered on LAN");
                            if self.discovery_rx.is_some() {
                                ui.spinner();
                                ui.label(egui::RichText::new("Scanning LAN...").weak());
                            }
                        });
                        ui.add_space(6.0);

                        if self.discovered_consoles.is_empty() && self.discovery_rx.is_none() {
                            card_frame(ui.style()).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    egui::RichText::new(
                                        "No PS5 consoles found on the local network. Ensure Remote Play is enabled on your PS5 and click \"Discover LAN\".",
                                    )
                                    .weak(),
                                );
                            });
                        } else {
                            for disc in &self.discovered_consoles {
                                let saved_idx =
                                    find_saved_index_for_discovered(&self.saved_consoles, disc);
                                render_discovered_console_card(
                                    ui,
                                    disc,
                                    saved_idx,
                                    &mut self.pending_home_action,
                                );
                                ui.add_space(8.0);
                            }
                        }
                    });
            });

        if self.register_modal.is_some() {
            let mut keep_open = true;
            let resp = egui::Window::new("Register PS5")
                .open(&mut keep_open)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .collapsible(false)
                .resizable(false)
                .default_width(500.0)
                .show(ui.ctx(), |ui| {
                    self.register_modal
                        .as_mut()
                        .is_some_and(|modal| render_register_modal_contents(ui, modal))
                });
            let cancel_clicked = resp.and_then(|inner| inner.inner).unwrap_or(false);
            if !keep_open || cancel_clicked {
                self.register_modal = None;
            }
        }
    }
}

fn render_status_banner(ui: &mut egui::Ui, banner: &StatusBanner) -> bool {
    let (bg, border, fg) = match banner.kind {
        BannerKind::Info => (
            egui::Color32::from_rgb(20, 40, 68),
            egui::Color32::from_rgb(48, 108, 184),
            egui::Color32::from_rgb(185, 220, 255),
        ),
        BannerKind::Success => (
            egui::Color32::from_rgb(18, 56, 38),
            egui::Color32::from_rgb(46, 152, 98),
            egui::Color32::from_rgb(168, 245, 200),
        ),
        BannerKind::Error => (
            egui::Color32::from_rgb(72, 24, 28),
            egui::Color32::from_rgb(185, 62, 68),
            egui::Color32::from_rgb(255, 195, 198),
        ),
    };
    let mut dismissed = false;
    egui::Frame::new()
        .fill(bg)
        .stroke(egui::Stroke::new(1.0, border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&banner.text).color(fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Dismiss").clicked() {
                        dismissed = true;
                    }
                });
            });
        });
    dismissed
}

fn card_frame(style: &egui::Style) -> egui::Frame {
    egui::Frame::group(style)
        .fill(egui::Color32::from_rgb(22, 27, 37))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 51, 70)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 12))
}

fn state_badge(ui: &mut egui::Ui, state: ConsoleState) {
    let (bg, fg, label) = match state {
        ConsoleState::Ready => (
            egui::Color32::from_rgb(20, 66, 44),
            egui::Color32::from_rgb(138, 242, 184),
            "Ready",
        ),
        ConsoleState::Standby => (
            egui::Color32::from_rgb(72, 52, 18),
            egui::Color32::from_rgb(255, 208, 122),
            "Standby",
        ),
        ConsoleState::Unknown => (
            egui::Color32::from_rgb(40, 46, 60),
            egui::Color32::from_rgb(180, 190, 210),
            "Unknown",
        ),
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).small().strong().color(fg));
        });
}

fn render_saved_console_card(
    ui: &mut egui::Ui,
    idx: usize,
    saved: &SavedConsole,
    live: Option<&DiscoveredConsole>,
    waking: bool,
    pending_action: &mut Option<HomeAction>,
) {
    card_frame(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&saved.name)
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::from_rgb(240, 244, 255)),
                    );
                    if let Some(disc) = live {
                        state_badge(ui, disc.state);
                        if !disc.running_app_name.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Playing: {}", disc.running_app_name))
                                    .small()
                                    .color(egui::Color32::from_rgb(156, 196, 255)),
                            );
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "Host: {}   |   ID: {}   |   MAC: {}",
                        saved.host,
                        saved.id,
                        format_mac_colon(saved.mac)
                    ))
                    .small()
                    .weak(),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Delete")
                                .color(egui::Color32::from_rgb(255, 175, 175)),
                        )
                        .fill(egui::Color32::from_rgb(66, 28, 34)),
                    )
                    .clicked()
                {
                    *pending_action = Some(HomeAction::DeleteSaved(idx));
                }

                if ui.add_enabled(!waking, egui::Button::new("Wake")).clicked() {
                    *pending_action = Some(HomeAction::WakeSaved(idx));
                }

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Connect")
                                .color(egui::Color32::WHITE)
                                .strong(),
                        )
                        .fill(egui::Color32::from_rgb(0, 112, 209)),
                    )
                    .clicked()
                {
                    *pending_action = Some(HomeAction::ConnectSaved(idx));
                }
            });
        });
    });
}

fn render_discovered_console_card(
    ui: &mut egui::Ui,
    disc: &DiscoveredConsole,
    saved_idx: Option<usize>,
    pending_action: &mut Option<HomeAction>,
) {
    card_frame(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&disc.host_name)
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::from_rgb(240, 244, 255)),
                    );
                    state_badge(ui, disc.state);
                    if saved_idx.is_some() {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgb(24, 52, 82))
                            .corner_radius(egui::CornerRadius::same(10))
                            .inner_margin(egui::Margin::symmetric(8, 2))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new("Saved")
                                        .small()
                                        .color(egui::Color32::from_rgb(154, 208, 255)),
                                );
                            });
                    }
                });

                let app_part = if disc.running_app_name.is_empty() {
                    String::new()
                } else {
                    format!("   |   App: {}", disc.running_app_name)
                };
                ui.label(
                    egui::RichText::new(format!(
                        "IP: {}   |   ID: {}   |   FW: {}{}",
                        disc.addr, disc.host_id, disc.system_version, app_part
                    ))
                    .small()
                    .weak(),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(idx) = saved_idx {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Connect")
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            )
                            .fill(egui::Color32::from_rgb(0, 112, 209)),
                        )
                        .clicked()
                    {
                        *pending_action = Some(HomeAction::ConnectSaved(idx));
                    }
                } else if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Register")
                                .color(egui::Color32::WHITE)
                                .strong(),
                        )
                        .fill(egui::Color32::from_rgb(44, 96, 168)),
                    )
                    .clicked()
                {
                    *pending_action = Some(HomeAction::OpenRegister(disc.addr.clone()));
                }
            });
        });
    });
}

fn render_register_modal_contents(ui: &mut egui::Ui, modal: &mut RegisterModal) -> bool {
    let mut request_close = false;
    let is_linking = modal.register_rx.is_some();
    let is_exchanging = modal.exchange_rx.is_some();

    ui.label("1. Enter your PS5 IP address and authenticate with PSN (or paste your Base64 Account ID directly).");
    ui.add_space(8.0);

    ui.label("Target PS5 IP:");
    ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.host)
            .desired_width(f32::INFINITY)
            .hint_text("192.168.1.50"),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !is_linking && !is_exchanging,
                egui::Button::new("Open PSN Login in Browser"),
            )
            .clicked()
        {
            match open_psn_login_in_browser() {
                Ok(()) => {
                    modal.error = None;
                    modal.notice = Some(
                        "Browser opened. Sign in to PSN, then copy the final redirect URL and paste it below."
                            .into(),
                    );
                }
                Err(err) => {
                    modal.notice = None;
                    modal.error = Some(format!("Failed opening browser: {err:#}"));
                }
            }
        }
        if is_exchanging {
            ui.spinner();
            ui.label(egui::RichText::new("Exchanging URL...").weak());
        }
    });
    ui.add_space(6.0);

    ui.label("PSN Redirect URL:");
    ui.horizontal(|ui| {
        ui.add_enabled(
            !is_linking && !is_exchanging,
            egui::TextEdit::singleline(&mut modal.redirect_url)
                .desired_width(275.0)
                .hint_text("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=..."),
        );
        if ui
            .add_enabled(
                !is_linking && !is_exchanging,
                egui::Button::new("Exchange URL -> Account ID"),
            )
            .clicked()
        {
            let url = modal.redirect_url.trim().to_string();
            if url.is_empty() {
                modal.notice = None;
                modal.error = Some("Paste the PSN redirect URL first.".into());
            } else {
                let (tx, rx) = crossbeam_channel::bounded(1);
                thread::Builder::new()
                    .name("gnsh-psn-exchange".into())
                    .spawn(move || {
                        let res = exchange_redirect_url_for_account_id(&url)
                            .map_err(|e| format!("{e:#}"));
                        let _ = tx.send(res);
                    })
                    .ok();
                modal.exchange_rx = Some(rx);
                modal.error = None;
                modal.notice = Some("Exchanging OAuth code for PSN Account ID...".into());
            }
        }
    });
    ui.add_space(8.0);

    ui.label("PSN Account ID (Base64, 8 bytes):");
    ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.psn_account_id)
            .desired_width(f32::INFINITY)
            .hint_text("Base64 Account ID (filled automatically by Exchange or pasted directly)"),
    );
    ui.add_space(8.0);

    ui.label("PS5 8-Digit Remote Play PIN (Settings -> System -> Remote Play -> Link Device):");
    let pin_resp = ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.pin_input)
            .desired_width(200.0)
            .hint_text("12345678"),
    );
    let submit_on_enter =
        !is_linking && pin_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

    if let Some(notice) = &modal.notice {
        ui.add_space(8.0);
        ui.colored_label(egui::Color32::from_rgb(148, 224, 178), notice);
    }
    if let Some(err) = &modal.error {
        ui.add_space(8.0);
        ui.colored_label(egui::Color32::from_rgb(255, 148, 148), err);
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        if is_linking {
            ui.spinner();
            ui.label("Linking with PS5 (30s timeout)...");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let link_clicked = ui
                .add_enabled(
                    !is_linking,
                    egui::Button::new(
                        egui::RichText::new("Link PS5")
                            .color(egui::Color32::WHITE)
                            .strong(),
                    )
                    .fill(egui::Color32::from_rgb(0, 112, 209)),
                )
                .clicked();
            if link_clicked || submit_on_enter {
                trigger_register_worker(modal);
            }
            if ui.button("Cancel").clicked() {
                request_close = true;
            }
        });
    });

    request_close
}

fn trigger_register_worker(modal: &mut RegisterModal) {
    let host = modal.host.trim().to_string();
    if host.is_empty() {
        modal.notice = None;
        modal.error = Some("Enter the target PS5 IP address.".into());
        return;
    }

    let account_id = modal.psn_account_id.trim().to_string();
    if account_id.is_empty() {
        modal.notice = None;
        modal.error = Some("Provide a Base64 PSN Account ID or exchange the redirect URL.".into());
        return;
    }
    if let Err(err) = decode_account_id_b64(&account_id) {
        modal.notice = None;
        modal.error = Some(format!("{err:#}"));
        return;
    }

    let normalized_pin: String = modal
        .pin_input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if normalized_pin.is_empty()
        || normalized_pin.len() > 8
        || !normalized_pin.chars().all(|c| c.is_ascii_digit())
    {
        modal.notice = None;
        modal.error = Some("Enter a valid 8-digit numeric PIN from PS5 Settings.".into());
        return;
    }
    let Ok(pin) = normalized_pin.parse::<u32>() else {
        modal.notice = None;
        modal.error = Some("Enter a valid 8-digit numeric PIN from PS5 Settings.".into());
        return;
    };

    let (tx, rx) = crossbeam_channel::bounded(1);
    thread::Builder::new()
        .name("gnsh-register-ps5".into())
        .spawn(move || {
            let res = register_ps5(&host, &account_id, pin, REGISTER_TIMEOUT)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(res);
        })
        .ok();
    modal.register_rx = Some(rx);
    modal.error = None;
    modal.notice = Some("Connecting to PS5 for registration...".into());
}

fn render_preset_controls(ui: &mut egui::Ui, preset: &mut VideoPreset) {
    let mut is_1080p = matches!(preset, VideoPreset::P1080Fps30 | VideoPreset::P1080Fps60);
    let mut is_60fps = matches!(preset, VideoPreset::P720Fps60 | VideoPreset::P1080Fps60);

    ui.label(egui::RichText::new("Res:").weak());
    ui.selectable_value(&mut is_1080p, true, "1080p");
    ui.selectable_value(&mut is_1080p, false, "720p");

    ui.separator();

    ui.label(egui::RichText::new("FPS:").weak());
    ui.selectable_value(&mut is_60fps, true, "60");
    ui.selectable_value(&mut is_60fps, false, "30");

    *preset = match (is_1080p, is_60fps) {
        (true, true) => VideoPreset::P1080Fps60,
        (true, false) => VideoPreset::P1080Fps30,
        (false, true) => VideoPreset::P720Fps60,
        (false, false) => VideoPreset::P720Fps30,
    };
}

fn render_scale_selector(ui: &mut egui::Ui, scale_mode: &mut ScaleMode) {
    ui.label(egui::RichText::new("Scale:").weak());
    ui.selectable_value(scale_mode, ScaleMode::Fit, "Fit")
        .on_hover_text("Preserve 16:9 aspect ratio (letterbox/pillarbox black bars)");
    ui.selectable_value(scale_mode, ScaleMode::Zoom, "Zoom")
        .on_hover_text(
            "Fill screen preserving aspect ratio by cropping outer edges (no black bars on 16:10 / 21:9)",
        );
    ui.selectable_value(scale_mode, ScaleMode::Stretch, "Stretch")
        .on_hover_text("Stretch frame to fill entire window without cropping");
}

fn render_audio_device_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    devices: &[String],
    selected: &mut Option<String>,
) {
    let selected_label = match selected.as_deref() {
        None => "System Default".to_string(),
        Some(name) if devices.iter().any(|d| d == name) => name.to_string(),
        Some(name) => format!("{name} (unavailable)"),
    };
    egui::ComboBox::from_id_salt(id_salt)
        .width(160.0)
        .selected_text(selected_label)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "System Default");
            for dev in devices {
                ui.selectable_value(selected, Some(dev.clone()), dev);
            }
        });
}

fn render_kbm_controls(
    ui: &mut egui::Ui,
    settings: &mut StreamSettings,
    mouse_lock: Option<(bool, &mut bool)>,
) {
    ui.checkbox(&mut settings.kbm_enabled, "KB+M")
        .on_hover_text(settings.kbm_bindings.summary_tooltip());
    if !settings.kbm_enabled {
        return;
    }

    if let Some((locked, toggle_requested)) = mouse_lock {
        let (label, fill) = if locked {
            ("Mouse: LOCKED (F8)", egui::Color32::from_rgb(38, 134, 82))
        } else {
            ("Lock Mouse (F8)", egui::Color32::from_rgb(0, 112, 209))
        };
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(label)
                        .small()
                        .color(egui::Color32::WHITE),
                )
                .fill(fill),
            )
            .on_hover_text(
                "Toggle cursor capture for right-stick camera look (F8 or Esc to unlock)",
            )
            .clicked()
        {
            *toggle_requested = true;
        }
    }

    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 65.0;
        ui.add(
            egui::Slider::new(&mut settings.kbm_sensitivity_pct, 1..=100)
                .clamping(egui::SliderClamping::Always)
                .suffix("%")
                .text("Sens"),
        )
        .on_hover_text("Mouse-to-right-stick sensitivity (1-100%)");
        ui.add(
            egui::Slider::new(&mut settings.kbm_deadzone_pct, 0..=40)
                .clamping(egui::SliderClamping::Always)
                .suffix("%")
                .text("DZ"),
        )
        .on_hover_text("Radial deadzone boost to overcome the game's inner analog deadzone");
    });
    ui.checkbox(&mut settings.kbm_invert_y, "Inv Y")
        .on_hover_text("Invert vertical mouse camera axis");
}

fn render_streaming_ui(
    ui: &mut egui::Ui,
    stream: &mut ActiveStream,
    gamepad_name: Option<&str>,
    settings: &mut StreamSettings,
    audio_devices: &[String],
    hdr_output_active: bool,
    mouse_locked: bool,
) {
    if stream.show_hud {
        egui::Area::new(egui::Id::unique("stream_hud"))
            .anchor(egui::Align2::LEFT_TOP, [14.0, 14.0])
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(12, 16, 24, 212))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(56, 68, 92)))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&stream.console_name)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            );
                            if stream.connected {
                                ui.label(
                                    egui::RichText::new("Connected")
                                        .small()
                                        .color(egui::Color32::from_rgb(138, 242, 184)),
                                );
                            } else {
                                ui.spinner();
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Connecting ({})...",
                                        stream.console_host
                                    ))
                                    .small()
                                    .color(egui::Color32::from_rgb(255, 208, 122)),
                                );
                            }

                            ui.separator();

                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Disconnect")
                                            .small()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(156, 44, 52)),
                                )
                                .clicked()
                            {
                                stream.disconnect_requested = true;
                            }

                            ui.separator();

                            let (mic_label, mic_fill) = if stream.mic_muted {
                                ("Mic: Muted", egui::Color32::from_rgb(56, 66, 86))
                            } else {
                                ("Mic: LIVE", egui::Color32::from_rgb(38, 134, 82))
                            };
                            let mic_btn = ui
                                .add_enabled(
                                    stream.connected,
                                    egui::Button::new(
                                        egui::RichText::new(mic_label)
                                            .small()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(mic_fill),
                                )
                                .on_hover_text(
                                    stream.mic_error.as_deref().unwrap_or(
                                        "Toggle microphone uplink (Ctrl+M or DualSense Mute button).",
                                    ),
                                );
                            if mic_btn.clicked() {
                                stream.mic_toggle_requested = true;
                            }

                            if ui
                                .add_enabled(
                                    stream.connected,
                                    egui::Button::new(
                                        egui::RichText::new("PS Home")
                                            .small()
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(0, 112, 209)),
                                )
                                .on_hover_text("Open PS5 Home screen (Home key).")
                                .clicked()
                            {
                                stream.go_home_requested = true;
                            }

                            ui.separator();
                            ui.checkbox(&mut settings.touchpad_touch, "Touchpad Touch")
                                .on_hover_text("Off: click-only. On: capacitive touch/swipe.");

                            ui.separator();
                            ui.checkbox(&mut settings.vsync, "VSync").on_hover_text(
                                "Off: lowest latency (decode-paced + tearing, 1000 Hz input). On: tear-free VSync (1-frame DXGI queue).",
                            );
                        });

                        ui.horizontal(|ui| {
                            render_scale_selector(ui, &mut settings.scale_mode);

                            ui.separator();
                            ui.scope(|ui| {
                                ui.spacing_mut().slider_width = 70.0;
                                ui.add(
                                    egui::Slider::new(&mut settings.sharpness_pct, 0..=100)
                                        .clamping(egui::SliderClamping::Always)
                                        .suffix("%")
                                        .text("CAS"),
                                )
                                .on_hover_text(
                                    "Contrast Adaptive Sharpening (single-pass HLSL, 0% = off)",
                                );
                                ui.add(
                                    egui::Slider::new(&mut settings.volume_pct, 0..=100)
                                        .clamping(egui::SliderClamping::Always)
                                        .suffix("%")
                                        .text("Vol"),
                                )
                                .on_hover_text("Stream audio playback volume (0-100%)");
                            });

                            ui.separator();
                            ui.label(egui::RichText::new("Audio:").weak());
                            render_audio_device_combo(
                                ui,
                                "hud_audio_device",
                                audio_devices,
                                &mut settings.audio_device,
                            );
                        });

                        ui.horizontal(|ui| {
                            render_kbm_controls(
                                ui,
                                settings,
                                Some((mouse_locked, &mut stream.mouse_lock_toggle_requested)),
                            );
                        });

                        let (decode_ms_text, decoder_badge) = match &stream.current_frame {
                            Some(info) => (
                                format!("{:.2} ms", info.decode_ms),
                                if info.is_hw { "D3D11VA" } else { "SW" },
                            ),
                            None => ("-- ms".to_string(), "Pending"),
                        };
                        let buf_frames = stream.decoder_lag_frames.load(Ordering::Relaxed);
                        let pipe_text = if stream.pipe_p50_ms > 0.0 {
                            format!(
                                "{:.1} ms (p95 {:.1}, Buf {})",
                                stream.pipe_p50_ms, stream.pipe_p95_ms, buf_frames
                            )
                        } else {
                            "-- ms".to_string()
                        };
                        let pad_label = match (gamepad_name, settings.kbm_enabled) {
                            (Some(name), true) => format!("{name} + KB+M"),
                            (Some(name), false) => name.to_string(),
                            (None, true) => "KB+M Virtual DualSense".to_string(),
                            (None, false) => "No Gamepad".to_string(),
                        };
                        let lost_total = stream.frames_lost_total.load(Ordering::Relaxed);
                        let udp_buf_text = match stream.udp_rcvbuf_kib {
                            Some(kib) => format!("{kib} KiB"),
                            None => "default".to_string(),
                        };
                        let haptics_label = if stream.usb_haptics_active.load(Ordering::Relaxed) {
                            "DualSense USB (4ch)"
                        } else {
                            "Rumble"
                        };

                        let hdr_badge = if stream.codec == VideoCodec::HevcHdr {
                            if hdr_output_active {
                                "  |  HDR10"
                            } else {
                                "  |  HDR->SDR"
                            }
                        } else {
                            ""
                        };
                        let cas_badge = if settings.sharpness_pct > 0 {
                            format!("  |  CAS: {}%", settings.sharpness_pct)
                        } else {
                            String::new()
                        };

                        ui.label(
                            egui::RichText::new(format!(
                                "{:.1} FPS  |  {:.1} Mbps  |  RTT: {:.1} ms  |  Loss: {:.1}%",
                                stream.fps,
                                stream.measured_mbps,
                                stream.rtt_ms,
                                stream.packet_loss_pct
                            ))
                            .small()
                            .color(egui::Color32::from_rgb(206, 216, 234)),
                        );
                        let telemetry_line = ui.label(
                            egui::RichText::new(format!(
                                "Pipe: {}  |  Decode: {} ({}){}{}  |  UDP Buf: {}  |  Gamepad: {}  |  Haptics: {}",
                                pipe_text,
                                decode_ms_text,
                                decoder_badge,
                                hdr_badge,
                                cas_badge,
                                udp_buf_text,
                                pad_label,
                                haptics_label
                            ))
                            .small()
                            .color(egui::Color32::from_rgb(184, 196, 218)),
                        );
                        if let Ok(guard) = stream.haptics_device_label.lock()
                            && let Some(ref dev_name) = *guard
                        {
                            telemetry_line.on_hover_text(format!("USB haptics device: {dev_name}"));
                        }

                        if lost_total > 0 {
                            let recent_loss = stream
                                .packet_loss_notice_until
                                .is_some_and(|until| Instant::now() < until);
                            let loss_color = if recent_loss {
                                egui::Color32::from_rgb(255, 198, 112)
                            } else {
                                egui::Color32::from_rgb(160, 170, 190)
                            };
                            ui.label(
                                egui::RichText::new(format!("Lost frames: {lost_total}"))
                                    .small()
                                    .color(loss_color),
                            );
                        }

                        ui.label(
                            egui::RichText::new(
                                "F3: Toggle HUD   |   F8: Lock Mouse   |   F11: Fullscreen   |   Home: PS Home   |   Ctrl+M: Toggle Mic   |   Esc: Unlock / Disconnect",
                            )
                            .small()
                            .weak(),
                        );
                    });
            });
    }

    if let Some(prompt) = &mut stream.pin_prompt {
        let mut submit_pin: Option<String> = None;
        let mut cancel_prompt = false;

        egui::Window::new("Console Login PIN")
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(340.0)
            .show(ui.ctx(), |ui| {
                ui.label("Your PS5 requires a user login PIN to unlock this session.");
                if prompt.incorrect {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 148, 148),
                        "Incorrect PIN. Please try again.",
                    );
                }
                ui.add_space(6.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut prompt.pin_input)
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .hint_text("Enter PS5 passcode"),
                );
                let enter_pressed =
                    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if (ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Submit PIN")
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            )
                            .fill(egui::Color32::from_rgb(0, 112, 209)),
                        )
                        .clicked()
                        || enter_pressed)
                        && !prompt.pin_input.trim().is_empty()
                    {
                        submit_pin = Some(prompt.pin_input.trim().to_string());
                    }
                    if ui.button("Disconnect").clicked() {
                        cancel_prompt = true;
                    }
                });
            });

        if let Some(pin) = submit_pin {
            if let Some(session) = stream.session.as_mut()
                && let Err(err) = session.send_login_pin(&pin)
            {
                tracing::warn!("send_login_pin failed: {err:#}");
            }
            stream.pin_prompt = None;
        } else if cancel_prompt {
            stream.pin_prompt = None;
            stream.disconnect_requested = true;
        }
    }

    render_osk_modal(ui, stream);
}

fn render_osk_modal(ui: &egui::Ui, stream: &mut ActiveStream) {
    let Some(osk) = &mut stream.osk_prompt else {
        return;
    };

    let mut submit = false;
    let mut cancel = false;
    let mut sync_text: Option<String> = None;

    egui::Window::new("PS5 On-Screen Keyboard")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .show(ui.ctx(), |ui| {
            ui.label("Type UTF-8 text to forward directly into the active PS5 input field:");
            ui.add_space(6.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut osk.text)
                    .desired_width(f32::INFINITY)
                    .hint_text("Type text to send to PS5..."),
            );
            if osk.needs_focus {
                resp.request_focus();
                osk.needs_focus = false;
            }
            if resp.changed() && osk.text != osk.last_sent_text {
                sync_text = Some(osk.text.clone());
            }
            let enter_pressed = (resp.has_focus() || resp.lost_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Enter));

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Send (Enter)")
                                .color(egui::Color32::WHITE)
                                .strong(),
                        )
                        .fill(egui::Color32::from_rgb(0, 112, 209)),
                    )
                    .clicked()
                    || enter_pressed
                {
                    submit = true;
                }
                if ui.button("Cancel (Esc)").clicked() {
                    cancel = true;
                }
            });
        });

    if submit {
        if let Some(session) = stream.session.as_mut() {
            if osk.text != osk.last_sent_text
                && let Err(err) = session.keyboard_set_text(&osk.text)
            {
                tracing::debug!("keyboard_set_text failed: {err:#}");
            }
            if let Err(err) = session.keyboard_accept() {
                tracing::debug!("keyboard_accept failed: {err:#}");
            }
        }
        stream.osk_prompt = None;
    } else if cancel {
        if let Some(session) = stream.session.as_mut()
            && let Err(err) = session.keyboard_reject()
        {
            tracing::debug!("keyboard_reject failed: {err:#}");
        }
        stream.osk_prompt = None;
    } else if let Some(text) = sync_text {
        if let Some(session) = stream.session.as_mut()
            && let Err(err) = session.keyboard_set_text(&text)
        {
            tracing::debug!("keyboard_set_text failed: {err:#}");
        }
        osk.last_sent_text = text;
    }
}

fn console_state_label(state: ConsoleState) -> &'static str {
    match state {
        ConsoleState::Ready => "Ready",
        ConsoleState::Standby => "Standby",
        ConsoleState::Unknown => "Unknown",
    }
}

fn format_mac_colon(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn format_mac_id(mac: [u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn saved_from_registered(reg: RegisteredConsole, discovered: &[DiscoveredConsole]) -> SavedConsole {
    let mac_id = format_mac_id(reg.host_mac);
    let id = discovered
        .iter()
        .find(|d| d.addr.eq_ignore_ascii_case(reg.addr.trim()) && !d.host_id.is_empty())
        .map(|d| d.host_id.clone())
        .unwrap_or(mac_id);

    SavedConsole {
        id,
        name: reg.host_name,
        host: reg.addr,
        mac: reg.host_mac,
        psn_account_id: reg.psn_account_id,
        regist_key: reg.regist_key,
        rp_key: reg.rp_key,
    }
}

fn upsert_saved_console(saved: &mut Vec<SavedConsole>, console: SavedConsole) {
    if let Some(existing) = saved.iter_mut().find(|s| {
        s.id.eq_ignore_ascii_case(&console.id)
            || (console.mac != [0; 6] && s.mac == console.mac)
            || s.host.eq_ignore_ascii_case(&console.host)
    }) {
        *existing = console;
    } else {
        saved.push(console);
    }
}

fn find_saved_index_for_discovered(
    saved: &[SavedConsole],
    disc: &DiscoveredConsole,
) -> Option<usize> {
    saved.iter().position(|s| {
        s.host.eq_ignore_ascii_case(disc.addr.trim())
            || (!disc.host_id.is_empty() && s.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(s.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
}

fn find_discovered_for_saved<'a>(
    discovered: &'a [DiscoveredConsole],
    saved: &SavedConsole,
) -> Option<&'a DiscoveredConsole> {
    discovered.iter().find(|disc| {
        saved.host.eq_ignore_ascii_case(disc.addr.trim())
            || (!disc.host_id.is_empty() && saved.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(saved.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
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
