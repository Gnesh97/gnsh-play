mod egui_d3d11;
mod storage;

use anyhow::{Context as _, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use egui_d3d11::EguiD3d11Painter;
use gnsh_input::{AppWindow, GamepadManager};
use gnsh_media::{AudioOutput, D3D11Renderer, DecodedFrame, MediaCodec, VideoDecoder};
use gnsh_proto::{
    ConsoleState, DiscoveredConsole, Ps5Session, Ps5SessionConfig, RegisteredConsole, SessionEvent,
    VideoCodec, VideoPreset, decode_account_id_b64, discover_ps5_lan,
    exchange_redirect_url_for_account_id, init_lib, open_psn_login_in_browser, register_ps5,
    wake_ps5,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use storage::{ConsoleStore, SavedConsole};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::HSTRING;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
const IDR_COOLDOWN: Duration = Duration::from_millis(500);

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

fn run_gui() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .try_init();

    init_lib().context("failed to initialize Remote Play library")?;

    let store = ConsoleStore::default_store().context("failed to resolve console store path")?;
    let (saved_consoles, initial_banner) = match store.load() {
        Ok(list) => (list, None),
        Err(err) => (
            Vec::new(),
            Some(StatusBanner::error(format!(
                "Failed loading saved consoles ({}): {err:#}",
                store.path().display()
            ))),
        ),
    };

    let mut window = AppWindow::new("gnsh-play", DEFAULT_WIDTH, DEFAULT_HEIGHT)
        .context("failed creating application window")?;
    let (init_w, init_h) = window.size();
    let mut renderer = D3D11Renderer::new(window.hwnd(), init_w, init_h)
        .context("failed initializing D3D11 renderer")?;
    let mut painter = EguiD3d11Painter::new(renderer.device())
        .context("failed initializing egui D3D11 painter")?;
    let mut gamepads = GamepadManager::new().context("failed initializing SDL3 gamepad manager")?;

    let egui_ctx = egui::Context::default();
    apply_theme(&egui_ctx);

    let mut app = AppState::new(store, saved_consoles, initial_banner);
    app.start_discovery();

    let mut raw_input = egui::RawInput::default();
    let start_time = Instant::now();

    loop {
        let summary = window.poll_events(&mut gamepads, &mut raw_input);
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

        app.handle_shortcuts(summary.toggle_hud, summary.escape_pressed);
        app.poll_background(&mut gamepads);

        raw_input.time = Some(start_time.elapsed().as_secs_f64());

        let is_streaming = matches!(app.mode, AppMode::Streaming(_));
        let clear_rgba = if is_streaming {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            [0.06, 0.07, 0.10, 1.0]
        };
        renderer.begin_frame(clear_rgba);

        if let AppMode::Streaming(ref stream) = app.mode
            && let Some(ref latest) = stream.current_frame
            && let Err(err) = renderer.draw_video_frame(&latest.frame)
        {
            tracing::warn!("draw_video_frame failed: {err:#}");
        }

        renderer.bind_backbuffer();

        let gamepad_name = gamepads.connected_name().map(str::to_owned);
        let device_for_connect = renderer.device().clone();
        let full_output = egui_ctx.run_ui(raw_input.take(), |ui| {
            app.ui(ui, gamepad_name.as_deref(), &device_for_connect);
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
            &clipped,
        ) {
            tracing::warn!("egui paint failed: {err:#}");
        }
        painter.free_textures(&full_output.textures_delta);

        let vsync = if is_streaming {
            app.settings.vsync
        } else {
            true
        };
        if let Err(err) = renderer.present(vsync) {
            tracing::warn!("D3D11 present failed: {err:#}");
        }

        app.process_post_ui(&mut gamepads);
    }

    app.stop_stream(&mut gamepads, None);
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

#[derive(Debug, Clone)]
struct StreamSettings {
    preset: VideoPreset,
    codec: VideoCodec,
    bitrate_kbps: u32,
    vsync: bool,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            preset: VideoPreset::P1080Fps60,
            codec: VideoCodec::Hevc,
            bitrate_kbps: 30_000,
            vsync: true,
        }
    }
}

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

struct LatestFrame {
    frame: DecodedFrame,
    decode_ms: f32,
    is_hw: bool,
}

struct LoginPinPrompt {
    pin_input: String,
    incorrect: bool,
}

struct ActiveStream {
    console_name: String,
    console_host: String,
    session: Option<Ps5Session>,
    events_rx: Receiver<SessionEvent>,
    shared_latest: Arc<Mutex<Option<LatestFrame>>>,
    current_frame: Option<LatestFrame>,
    frames_decoded: Arc<AtomicU64>,
    frames_lost_total: Arc<AtomicU64>,
    idr_needed: Arc<AtomicBool>,
    video_init_error: Arc<Mutex<Option<String>>>,
    stop_flag: Arc<AtomicBool>,
    video_thread: Option<JoinHandle<()>>,
    audio_thread: Option<JoinHandle<()>>,
    connected: bool,
    show_hud: bool,
    disconnect_requested: bool,
    pin_prompt: Option<LoginPinPrompt>,
    fps: f32,
    fps_window_start: Instant,
    fps_last_count: u64,
    last_idr_request: Instant,
    last_seen_lost: u64,
    packet_loss_notice_until: Option<Instant>,
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

        let media_codec = match settings.codec {
            VideoCodec::H264 => MediaCodec::H264,
            VideoCodec::Hevc => MediaCodec::Hevc,
        };

        let shared_latest = Arc::new(Mutex::new(None));
        let frames_decoded = Arc::new(AtomicU64::new(0));
        let frames_lost_total = Arc::new(AtomicU64::new(0));
        let idr_needed = Arc::new(AtomicBool::new(false));
        let video_init_error = Arc::new(Mutex::new(None));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let video_args = VideoWorkerArgs {
            codec: media_codec,
            device,
            video_rx: channels.video_rx,
            shared_latest: Arc::clone(&shared_latest),
            frames_decoded: Arc::clone(&frames_decoded),
            frames_lost_total: Arc::clone(&frames_lost_total),
            idr_needed: Arc::clone(&idr_needed),
            init_error: Arc::clone(&video_init_error),
            stop_flag: Arc::clone(&stop_flag),
        };
        let video_thread = thread::Builder::new()
            .name("gnsh-video-decode".into())
            .spawn(move || run_video_decode_loop(video_args))
            .context("failed spawning video decode thread")?;

        let audio_rx = channels.audio_rx;
        let audio_stop = Arc::clone(&stop_flag);
        let audio_thread = thread::Builder::new()
            .name("gnsh-audio-out".into())
            .spawn(move || run_audio_output_loop(audio_rx, audio_stop))
            .context("failed spawning audio output thread")?;

        Ok(Self {
            console_name: console.name.clone(),
            console_host: console.host.clone(),
            session: Some(session),
            events_rx: channels.events_rx,
            shared_latest,
            current_frame: None,
            frames_decoded,
            frames_lost_total,
            idr_needed,
            video_init_error,
            stop_flag,
            video_thread: Some(video_thread),
            audio_thread: Some(audio_thread),
            connected: false,
            show_hud: true,
            disconnect_requested: false,
            pin_prompt: None,
            fps: 0.0,
            fps_window_start: Instant::now(),
            fps_last_count: 0,
            last_idr_request: Instant::now() - IDR_COOLDOWN,
            last_seen_lost: 0,
            packet_loss_notice_until: None,
        })
    }

    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
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
        if let Ok(mut guard) = self.shared_latest.lock() {
            *guard = None;
        }
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
    shared_latest: Arc<Mutex<Option<LatestFrame>>>,
    frames_decoded: Arc<AtomicU64>,
    frames_lost_total: Arc<AtomicU64>,
    idr_needed: Arc<AtomicBool>,
    init_error: Arc<Mutex<Option<String>>>,
    stop_flag: Arc<AtomicBool>,
}

fn run_video_decode_loop(args: VideoWorkerArgs) {
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

                let t0 = Instant::now();
                match decoder.decode(&pkt.data) {
                    Ok(frames) => {
                        let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;
                        let is_hw = decoder.is_hw_active();
                        let count = frames.len() as u64;
                        if count > 0 {
                            args.frames_decoded.fetch_add(count, Ordering::Relaxed);
                        }
                        if let Some(frame) = frames.into_iter().last() {
                            let prev = {
                                let mut guard =
                                    args.shared_latest.lock().unwrap_or_else(|e| e.into_inner());
                                guard.replace(LatestFrame {
                                    frame,
                                    decode_ms,
                                    is_hw,
                                })
                            };
                            drop(prev);
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            "Video decode error (frames_lost={}): {err:#}",
                            pkt.frames_lost
                        );
                        args.idr_needed.store(true, Ordering::Relaxed);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn run_audio_output_loop(
    audio_rx: Receiver<gnsh_proto::AudioPcmFrame>,
    stop_flag: Arc<AtomicBool>,
) {
    let mut audio = match AudioOutput::new(2, 48_000) {
        Ok(out) => Some(out),
        Err(err) => {
            tracing::warn!("AudioOutput init failed (continuing muted): {err:#}");
            None
        }
    };

    while !stop_flag.load(Ordering::Relaxed) {
        match audio_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(frame) => {
                if let Some(out) = audio.as_mut()
                    && let Err(err) =
                        out.push_pcm(&frame.samples, frame.channels, frame.sample_rate)
                {
                    tracing::warn!("Audio push_pcm failed: {err:#}");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
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
    saved_consoles: Vec<SavedConsole>,
    discovered_consoles: Vec<DiscoveredConsole>,
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
        saved_consoles: Vec<SavedConsole>,
        status_banner: Option<StatusBanner>,
    ) -> Self {
        Self {
            store,
            saved_consoles,
            discovered_consoles: Vec::new(),
            discovery_rx: None,
            wake_rx: None,
            settings: StreamSettings::default(),
            status_banner,
            register_modal: None,
            mode: AppMode::Home,
            pending_home_action: None,
            pending_stream_connect: None,
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

    fn handle_shortcuts(&mut self, toggle_hud: bool, escape_pressed: bool) {
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
                if escape_pressed {
                    stream.disconnect_requested = true;
                }
            }
        }
    }

    fn poll_background(&mut self, gamepads: &mut GamepadManager) {
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
                    }
                    SessionEvent::Nickname(nick) => {
                        if !nick.trim().is_empty() {
                            stream.console_name = nick;
                        }
                    }
                    SessionEvent::Rumble { left, right } => {
                        gamepads.set_rumble(left, right);
                    }
                    SessionEvent::Led { r, g, b } => {
                        gamepads.set_led(r, g, b);
                    }
                    SessionEvent::TriggerEffects { .. } => {}
                    SessionEvent::LoginPinRequested { incorrect } => {
                        stream.pin_prompt = Some(LoginPinPrompt {
                            pin_input: String::new(),
                            incorrect,
                        });
                    }
                    SessionEvent::Quit { reason, is_error } => {
                        stream_quit_banner = Some(if is_error {
                            StatusBanner::error(format!("Session ended: {reason}"))
                        } else {
                            StatusBanner::info(format!("Disconnected: {reason}"))
                        });
                    }
                }
            }

            if stream_quit_banner.is_none() {
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

                if let Ok(mut guard) = stream.shared_latest.lock()
                    && let Some(newer) = guard.take()
                {
                    stream.current_frame = Some(newer);
                }

                let elapsed = stream.fps_window_start.elapsed().as_secs_f32();
                if elapsed >= 0.5 {
                    let count = stream.frames_decoded.load(Ordering::Relaxed);
                    stream.fps = (count.saturating_sub(stream.fps_last_count)) as f32 / elapsed;
                    stream.fps_last_count = count;
                    stream.fps_window_start = Instant::now();
                }

                if let Some(ctrl_state) = gamepads.poll_state()
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
            if let Some(b) = banner {
                self.status_banner = Some(b);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, gamepad_name: Option<&str>, device: &ID3D11Device) {
        if let Some(idx) = self.pending_stream_connect.take() {
            self.begin_stream(idx, device.clone());
        }

        match &mut self.mode {
            AppMode::Home => {
                self.render_home_ui(ui, gamepad_name);
            }
            AppMode::Streaming(stream) => {
                render_streaming_ui(ui, stream, gamepad_name);
            }
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

                    ui.label(egui::RichText::new("Res:").weak());
                    ui.selectable_value(
                        &mut self.settings.preset,
                        VideoPreset::P1080Fps60,
                        "1080p60",
                    );
                    ui.selectable_value(
                        &mut self.settings.preset,
                        VideoPreset::P720Fps60,
                        "720p60",
                    );

                    ui.separator();

                    ui.label(egui::RichText::new("Codec:").weak());
                    ui.selectable_value(&mut self.settings.codec, VideoCodec::Hevc, "HEVC");
                    ui.selectable_value(&mut self.settings.codec, VideoCodec::H264, "H264");

                    ui.separator();

                    ui.add(
                        egui::Slider::new(&mut self.settings.bitrate_kbps, 2_000..=100_000)
                            .step_by(1_000.0)
                            .clamping(egui::SliderClamping::Never)
                            .suffix(" kbps")
                            .text("Bitrate"),
                    );

                    ui.checkbox(&mut self.settings.vsync, "VSync");
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

fn render_streaming_ui(ui: &mut egui::Ui, stream: &mut ActiveStream, gamepad_name: Option<&str>) {
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
                        });

                        let (decode_ms_text, decoder_badge) = match &stream.current_frame {
                            Some(info) => (
                                format!("{:.2} ms", info.decode_ms),
                                if info.is_hw { "D3D11VA" } else { "SW" },
                            ),
                            None => ("-- ms".to_string(), "Pending"),
                        };
                        let pad_label = gamepad_name.unwrap_or("No Gamepad");
                        let lost_total = stream.frames_lost_total.load(Ordering::Relaxed);

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:.1} FPS  |  Decode: {} ({})  |  Gamepad: {}",
                                    stream.fps, decode_ms_text, decoder_badge, pad_label
                                ))
                                .small()
                                .color(egui::Color32::from_rgb(206, 216, 234)),
                            );
                        });

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
                                "F3: Toggle HUD   |   F11: Fullscreen   |   Esc: Disconnect",
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
