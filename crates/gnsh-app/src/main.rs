#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod egui_d3d11;
mod egui_placebo;
mod gamepad_nav;
mod logging;
mod reconnect;
mod shortcut;
mod storage;
mod stream;
mod ui;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use egui_d3d11::EguiD3d11Painter;
use egui_placebo::EguiPlaceboPainter;
use gamepad_nav::{
    GamepadHomeNav, HUD_QUICK_ROWS, HomeNavEvent, HudNavAction, apply_hud_nav, apply_settings_nav,
    settings_row_count,
};
use gnsh_input::{
    AppWindow, GamepadManager, KbmAction, KbmConfig, KbmEmulator, merge_controller_states,
};
use gnsh_media::{
    D3D11Renderer, D3D12Renderer, DecoderHardware, RendererBackend, VideoRenderOptions,
    VideoRenderer, VulkanPlaceboRenderer, list_playback_devices, list_recording_devices,
};
use gnsh_proto::{
    ControllerState, DiscoveredConsole, RegisteredConsole, SessionEvent, UDP_RECV_BUF_BYTES,
    VideoCodec, discover_ps5_hosts, discover_ps5_lan, init_lib, wake_ps5,
};
use reconnect::ReconnectState;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use storage::{ConsoleStore, SavedConsole, SettingsStore, StoredSettings};
use stream::{ActiveStream, IDR_COOLDOWN, LoginPinPrompt, OskPrompt};
use ui::{
    EditConsoleModal, HomeAction, RegisterModal, SettingsTab, StatusBanner, StreamSettings,
    apply_theme, console_state_label, format_mac_id, host_matches_remote_probe,
    render_streaming_ui, saved_from_registered, upsert_saved_console,
};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_SHARED, LoadImageW, SM_CXICON,
    SM_CXSMICON, SM_CYICON, SM_CYSMICON, SW_SHOWNORMAL, SendMessageW, WM_SETICON,
};
use windows::core::{HSTRING, PCWSTR};

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
pub(crate) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const LOW_LATENCY_INPUT_WAIT: Duration = Duration::from_millis(1);
const IDLE_UI_REFRESH_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LaunchOptions {
    connect_target: Option<String>,
    use_wan: bool,
    start_fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliAction {
    RunGui(LaunchOptions),
    ShowHelp,
    DiscoverLan,
    ListConsoles,
}

fn main() -> Result<()> {
    match handle_cli()? {
        CliAction::RunGui(launch) => run_gui(launch),
        CliAction::ShowHelp | CliAction::DiscoverLan | CliAction::ListConsoles => Ok(()),
    }
}

fn parse_cli_args(args: &[String]) -> std::result::Result<CliAction, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(CliAction::ShowHelp);
    }

    let mut has_discover = false;
    let mut has_list = false;
    let mut launch = LaunchOptions::default();
    let mut i = 0;

    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--discover" => {
                has_discover = true;
                i += 1;
            }
            "--list" => {
                has_list = true;
                i += 1;
            }
            "--wan" => {
                launch.use_wan = true;
                i += 1;
            }
            "--fullscreen" => {
                launch.start_fullscreen = true;
                i += 1;
            }
            "--connect" => {
                if launch.connect_target.is_some() {
                    return Err("--connect cannot be specified more than once".into());
                }
                let Some(val) = args.get(i + 1) else {
                    return Err("--connect requires a console name, ID, or host".into());
                };
                let trimmed = val.trim();
                if trimmed.is_empty() {
                    return Err("--connect requires a non-empty console name, ID, or host".into());
                }
                launch.connect_target = Some(trimmed.to_string());
                i += 2;
            }
            s if s.starts_with("--connect=") => {
                if launch.connect_target.is_some() {
                    return Err("--connect cannot be specified more than once".into());
                }
                let trimmed = s["--connect=".len()..].trim();
                if trimmed.is_empty() {
                    return Err("--connect requires a non-empty console name, ID, or host".into());
                }
                launch.connect_target = Some(trimmed.to_string());
                i += 1;
            }
            other => {
                return Err(format!("Unknown argument: {other}"));
            }
        }
    }

    if has_discover && has_list {
        return Err("--discover and --list cannot be combined".into());
    }
    if (has_discover || has_list)
        && (launch.connect_target.is_some() || launch.use_wan || launch.start_fullscreen)
    {
        return Err(
            "--discover and --list cannot be combined with --connect, --wan, or --fullscreen"
                .into(),
        );
    }
    if has_discover {
        return Ok(CliAction::DiscoverLan);
    }
    if has_list {
        return Ok(CliAction::ListConsoles);
    }
    if launch.use_wan && launch.connect_target.is_none() {
        return Err("--wan requires --connect <NAME|ID|HOST>".into());
    }

    Ok(CliAction::RunGui(launch))
}

fn find_saved_console_for_cli(saved: &[SavedConsole], query: &str) -> Option<usize> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    saved
        .iter()
        .position(|c| {
            c.id.trim().eq_ignore_ascii_case(q) || format_mac_id(c.mac).eq_ignore_ascii_case(q)
        })
        .or_else(|| {
            saved
                .iter()
                .position(|c| c.name.trim().eq_ignore_ascii_case(q))
        })
        .or_else(|| {
            saved.iter().position(|c| {
                c.host.trim().eq_ignore_ascii_case(q)
                    || c.remote_host
                        .as_deref()
                        .is_some_and(|r| r.trim().eq_ignore_ascii_case(q))
            })
        })
}

fn handle_cli() -> Result<CliAction> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match parse_cli_args(&args) {
        Ok(action) => action,
        Err(err) => {
            logging::attach_parent_console();
            eprintln!("{err}");
            print_usage();
            std::process::exit(2);
        }
    };

    match &action {
        CliAction::RunGui(_) => {}
        CliAction::ShowHelp => {
            logging::attach_parent_console();
            print_usage();
        }
        CliAction::DiscoverLan => {
            logging::attach_parent_console();
            init_lib().context("failed to initialize Remote Play library")?;
            let consoles =
                discover_ps5_lan(DISCOVERY_TIMEOUT).context("PS5 LAN discovery failed")?;
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
        }
        CliAction::ListConsoles => {
            logging::attach_parent_console();
            let store =
                ConsoleStore::default_store().context("failed to resolve console store path")?;
            let saved = store.load().context("failed to load saved consoles")?;
            if saved.is_empty() {
                println!("No saved PS5 consoles.");
            } else {
                for console in &saved {
                    match console.remote_host.as_deref() {
                        Some(remote) => println!(
                            "{} | id={} | lan={} | wan={}",
                            console.name, console.id, console.host, remote
                        ),
                        None => println!(
                            "{} | id={} | lan={}",
                            console.name, console.id, console.host
                        ),
                    }
                }
            }
        }
    }

    Ok(action)
}

fn print_usage() {
    println!(concat!(
        "gnsh-play - Native Windows PS5 Remote Play client\n\n",
        "USAGE:\n",
        "    gnsh-play [OPTIONS]\n\n",
        "OPTIONS:\n",
        "    -h, --help              Print this help message and exit\n",
        "        --discover          Discover PS5 consoles on LAN and exit\n",
        "        --list              List saved PS5 consoles and exit\n",
        "        --connect <TARGET>  Directly stream saved console by name, ID, or host\n",
        "        --wan               Use the saved console's Remote / WAN host with --connect\n",
        "        --fullscreen        Launch the GUI window in fullscreen mode",
    ));
}

enum ActiveBackend {
    D3D11 {
        renderer: D3D11Renderer,
        egui: EguiD3d11Painter,
    },
    D3D12 {
        renderer: Box<D3D12Renderer>,
        egui: EguiPlaceboPainter,
    },
    Vulkan {
        renderer: VulkanPlaceboRenderer,
        egui: EguiPlaceboPainter,
    },
}

fn paint_egui_overlay<R: VideoRenderer + gnsh_media::UiOverlayRenderer>(
    label: &str,
    renderer: &mut R,
    egui: &mut EguiPlaceboPainter,
    textures_delta: &egui::TexturesDelta,
    pixels_per_point: f32,
    clipped: &[egui::ClippedPrimitive],
    hdr_output_active: bool,
) {
    if let Err(err) = egui.update_textures(renderer, textures_delta) {
        tracing::warn!("{label} egui texture upload failed: {err:#}");
    }
    if let Err(err) = egui.paint(
        renderer,
        renderer.viewport_size(),
        pixels_per_point,
        hdr_output_active,
        clipped,
    ) {
        tracing::warn!("{label} egui paint failed: {err:#}");
    }
    egui.free_textures(renderer, textures_delta);
}

impl ActiveBackend {
    fn init(
        requested: RendererBackend,
        initial_banner: &mut Option<StatusBanner>,
    ) -> Result<(AppWindow, Self)> {
        if requested == RendererBackend::Vulkan {
            match AppWindow::new_with_vulkan("gnsh-play", DEFAULT_WIDTH, DEFAULT_HEIGHT, true) {
                Ok(window) => {
                    apply_window_icon(window.hwnd());
                    let (w, h) = window.size();
                    let ext_names = match window.vulkan_instance_extensions() {
                        Ok(names) => names,
                        Err(err) => {
                            tracing::warn!("SDL Vulkan extensions unavailable: {err:#}");
                            Vec::new()
                        }
                    };
                    let exts: Vec<*const std::ffi::c_char> =
                        ext_names.iter().map(|s| s.as_ptr()).collect();
                    // SAFETY: `exts` holds valid SDL C strings and `destroy_vulkan_surface`
                    // matches the instance/surface pair made by `create_vulkan_surface`.
                    let vk_res = unsafe {
                        VulkanPlaceboRenderer::new(
                            &exts,
                            |inst| window.create_vulkan_surface(inst),
                            AppWindow::destroy_vulkan_surface,
                            w,
                            h,
                            true,
                        )
                    };
                    match vk_res {
                        Ok(renderer) => {
                            return Ok((
                                window,
                                Self::Vulkan {
                                    renderer,
                                    egui: EguiPlaceboPainter::new(),
                                },
                            ));
                        }
                        Err(err) => {
                            tracing::warn!("Vulkan init failed, using Direct3D 11: {err:#}");
                            if initial_banner.is_none() {
                                *initial_banner = Some(StatusBanner::info(format!(
                                    "Vulkan unavailable ({err}); fell back to Direct3D 11."
                                )));
                            }
                        }
                    }
                    drop(window);
                }
                Err(err) => {
                    tracing::warn!("Vulkan window failed, using Direct3D 11: {err:#}");
                    if initial_banner.is_none() {
                        *initial_banner = Some(StatusBanner::info(format!(
                            "Vulkan window unavailable ({err}); fell back to Direct3D 11."
                        )));
                    }
                }
            }
        } else if requested == RendererBackend::D3D12 {
            let window = AppWindow::new("gnsh-play", DEFAULT_WIDTH, DEFAULT_HEIGHT)
                .context("failed creating application window")?;
            apply_window_icon(window.hwnd());
            let (init_w, init_h) = window.size();
            match D3D12Renderer::new(window.hwnd(), init_w, init_h) {
                Ok(renderer) => {
                    return Ok((
                        window,
                        Self::D3D12 {
                            renderer: Box::new(renderer),
                            egui: EguiPlaceboPainter::new(),
                        },
                    ));
                }
                Err(err) => {
                    tracing::warn!("Direct3D 12 init failed, using Direct3D 11: {err:#}");
                    if initial_banner.is_none() {
                        *initial_banner = Some(StatusBanner::info(format!(
                            "Direct3D 12 unavailable ({err}); fell back to Direct3D 11."
                        )));
                    }
                    let renderer = D3D11Renderer::new(window.hwnd(), init_w, init_h)
                        .context("failed initializing D3D11 fallback renderer")?;
                    let egui = EguiD3d11Painter::new(renderer.device())
                        .context("failed initializing egui D3D11 painter")?;
                    return Ok((window, Self::D3D11 { renderer, egui }));
                }
            }
        }

        let window = AppWindow::new("gnsh-play", DEFAULT_WIDTH, DEFAULT_HEIGHT)
            .context("failed creating application window")?;
        apply_window_icon(window.hwnd());
        let (init_w, init_h) = window.size();
        let renderer = D3D11Renderer::new(window.hwnd(), init_w, init_h)
            .context("failed initializing D3D11 renderer")?;
        let egui = EguiD3d11Painter::new(renderer.device())
            .context("failed initializing egui D3D11 painter")?;
        Ok((window, Self::D3D11 { renderer, egui }))
    }

    fn label(&self) -> &'static str {
        match self {
            Self::D3D11 { .. } => "Direct3D 11",
            Self::D3D12 { .. } => "Direct3D 12",
            Self::Vulkan { .. } => "Vulkan (libplacebo)",
        }
    }

    fn short_label(&self) -> &'static str {
        match self {
            Self::D3D11 { .. } => "D3D11",
            Self::D3D12 { .. } => "D3D12",
            Self::Vulkan { .. } => "Vulkan",
        }
    }

    fn decoder_hardware(&self) -> DecoderHardware {
        match self {
            Self::D3D11 { renderer, .. } => {
                DecoderHardware::D3D11ZeroCopy(renderer.device().clone())
            }
            Self::D3D12 { renderer, .. } => {
                DecoderHardware::D3D12ZeroCopy(renderer.device().clone())
            }
            Self::Vulkan { renderer, .. } => match renderer.video_device() {
                Some(vk_dev) => DecoderHardware::VulkanZeroCopy(vk_dev),
                None => DecoderHardware::D3D11Transfer,
            },
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        match self {
            Self::D3D11 { renderer, .. } => VideoRenderer::resize(renderer, width, height),
            Self::D3D12 { renderer, .. } => VideoRenderer::resize(renderer.as_mut(), width, height),
            Self::Vulkan { renderer, .. } => VideoRenderer::resize(renderer, width, height),
        }
    }

    fn wait_for_frame_latency(&self, timeout_ms: u32) {
        match self {
            Self::D3D11 { renderer, .. } => renderer.wait_for_frame_latency(timeout_ms),
            Self::D3D12 { renderer, .. } => {
                VideoRenderer::wait_for_frame_latency(renderer.as_ref(), timeout_ms)
            }
            Self::Vulkan { renderer, .. } => renderer.wait_for_frame_latency(timeout_ms),
        }
    }

    fn clear_video_cache(&mut self) {
        match self {
            Self::D3D11 { renderer, .. } => renderer.clear_video_cache(),
            Self::D3D12 { renderer, .. } => VideoRenderer::clear_video_cache(renderer.as_mut()),
            Self::Vulkan { renderer, .. } => renderer.clear_video_cache(),
        }
    }

    fn is_hdr_output_active(&self) -> bool {
        match self {
            Self::D3D11 { renderer, .. } => VideoRenderer::is_hdr_output_active(renderer),
            Self::D3D12 { renderer, .. } => VideoRenderer::is_hdr_output_active(renderer.as_ref()),
            Self::Vulkan { renderer, .. } => VideoRenderer::is_hdr_output_active(renderer),
        }
    }

    fn set_hdr_output(&mut self, enable: bool) -> Result<bool> {
        match self {
            Self::D3D11 { renderer, .. } => VideoRenderer::set_hdr_output(renderer, enable),
            Self::D3D12 { renderer, .. } => {
                VideoRenderer::set_hdr_output(renderer.as_mut(), enable)
            }
            Self::Vulkan { renderer, .. } => VideoRenderer::set_hdr_output(renderer, enable),
        }
    }

    fn set_vsync(&mut self, vsync: bool) -> Result<()> {
        match self {
            Self::D3D11 { .. } | Self::D3D12 { .. } => Ok(()),
            Self::Vulkan { renderer, .. } => renderer.set_vsync(vsync),
        }
    }

    fn begin_frame(&self, clear_rgba: [f32; 4]) {
        match self {
            Self::D3D11 { renderer, .. } => VideoRenderer::begin_frame(renderer, clear_rgba),
            Self::D3D12 { renderer, .. } => {
                VideoRenderer::begin_frame(renderer.as_ref(), clear_rgba)
            }
            Self::Vulkan { renderer, .. } => VideoRenderer::begin_frame(renderer, clear_rgba),
        }
    }

    fn draw_video_frame(
        &mut self,
        frame: &gnsh_media::DecodedFrame,
        options: VideoRenderOptions,
    ) -> Result<()> {
        match self {
            Self::D3D11 { renderer, .. } => {
                VideoRenderer::draw_video_frame(renderer, frame, options)
            }
            Self::D3D12 { renderer, .. } => {
                VideoRenderer::draw_video_frame(renderer.as_mut(), frame, options)
            }
            Self::Vulkan { renderer, .. } => {
                VideoRenderer::draw_video_frame(renderer, frame, options)
            }
        }
    }

    fn paint_egui(
        &mut self,
        textures_delta: &egui::TexturesDelta,
        pixels_per_point: f32,
        clipped: &[egui::ClippedPrimitive],
        hdr_output_active: bool,
    ) {
        match self {
            Self::D3D11 { renderer, egui } => {
                renderer.bind_backbuffer();
                if let Err(err) =
                    egui.update_textures(renderer.device(), renderer.context(), textures_delta)
                {
                    tracing::warn!("egui texture upload failed: {err:#}");
                }
                if let Err(err) = egui.paint(
                    renderer.device(),
                    renderer.context(),
                    VideoRenderer::viewport_size(renderer),
                    pixels_per_point,
                    hdr_output_active,
                    clipped,
                ) {
                    tracing::warn!("egui paint failed: {err:#}");
                }
                egui.free_textures(textures_delta);
            }
            Self::D3D12 { renderer, egui } => paint_egui_overlay(
                "D3D12",
                renderer.as_mut(),
                egui,
                textures_delta,
                pixels_per_point,
                clipped,
                hdr_output_active,
            ),
            Self::Vulkan { renderer, egui } => paint_egui_overlay(
                "Vulkan",
                renderer,
                egui,
                textures_delta,
                pixels_per_point,
                clipped,
                hdr_output_active,
            ),
        }
    }

    fn present(&self, vsync: bool) -> Result<()> {
        match self {
            Self::D3D11 { renderer, .. } => VideoRenderer::present(renderer, vsync),
            Self::D3D12 { renderer, .. } => VideoRenderer::present(renderer.as_ref(), vsync),
            Self::Vulkan { renderer, .. } => VideoRenderer::present(renderer, vsync),
        }
    }
}

fn run_gui(launch: LaunchOptions) -> Result<()> {
    let _log_path = logging::init_logging();

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

    let (mut window, mut backend) =
        ActiveBackend::init(initial_settings.renderer_backend, &mut initial_banner)?;
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
    app.active_renderer_label = backend.label();
    app.active_renderer_short = backend.short_label();
    tracing::info!(
        renderer = app.active_renderer_label,
        "Active graphics backend ready"
    );
    if launch.start_fullscreen
        && let Err(err) = window.toggle_fullscreen()
    {
        tracing::warn!("Failed entering fullscreen on startup: {err:#}");
        app.status_banner = Some(StatusBanner::error(format!(
            "Failed toggling fullscreen: {err:#}"
        )));
    }
    if let Some(target) = launch.connect_target.as_deref() {
        if let Some(idx) = find_saved_console_for_cli(&app.saved_consoles, target) {
            app.selected_console_idx = idx;
            app.begin_stream(idx, launch.use_wan, backend.decoder_hardware());
        } else {
            app.status_banner = Some(StatusBanner::error(format!(
                "Saved console '{target}' not found. Run --list to view saved consoles."
            )));
        }
    }
    if matches!(app.mode, AppMode::Home) {
        app.start_discovery();
    }

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
            backend.wait_for_frame_latency(100);
            true
        };

        let summary = window.poll_events(&mut gamepads, &mut kbm, &mut raw_input);
        if summary.quit {
            break;
        }
        if let Some((nw, nh)) = summary.resized
            && let Err(err) = backend.resize(nw, nh)
        {
            tracing::warn!(
                "{} resize({nw}, {nh}) failed: {err:#}",
                backend.short_label()
            );
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
        let mut toggle_mic_for_shortcuts = summary.toggle_mic;
        let mut go_home_for_shortcuts = summary.go_home;
        let mut take_screenshot_for_shortcuts = summary.take_screenshot;
        let mut toggle_recording_for_shortcuts = summary.toggle_recording;
        let mut save_replay_clip_for_shortcuts = summary.save_replay_clip;
        if app.kbm_modal_open {
            toggle_mic_for_shortcuts = false;
            go_home_for_shortcuts = false;
            take_screenshot_for_shortcuts = false;
            toggle_recording_for_shortcuts = false;
            save_replay_clip_for_shortcuts = false;
            if let Some(action) = app.kbm_listening_action {
                if summary.escape_pressed {
                    app.kbm_listening_action = None;
                    escape_for_shortcuts = false;
                    raw_input.events.clear();
                } else if let Some(input) = summary.last_kbm_input {
                    app.settings.kbm_bindings.set(action, input);
                    app.persist_settings();
                    app.kbm_listening_action = None;
                    raw_input.events.clear();
                }
            } else if summary.escape_pressed {
                app.kbm_modal_open = false;
                escape_for_shortcuts = false;
                raw_input.events.clear();
            }
        }
        if escape_for_shortcuts && prev_mouse_locked {
            window.set_relative_mouse_mode(false);
            kbm.clear_state();
            escape_for_shortcuts = false;
        }
        if summary.toggle_mouse_lock
            && !app.kbm_modal_open
            && !app.settings_open
            && matches!(app.mode, AppMode::Streaming(_))
        {
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

        app.handle_shortcuts(StreamShortcuts {
            toggle_hud: summary.toggle_hud,
            escape_pressed: escape_for_shortcuts,
            toggle_mic: toggle_mic_for_shortcuts,
            go_home: go_home_for_shortcuts,
            take_screenshot: take_screenshot_for_shortcuts,
            toggle_recording: toggle_recording_for_shortcuts,
            save_replay_clip: save_replay_clip_for_shortcuts,
        });
        if summary.audio_devices_changed {
            app.audio_devices = list_playback_devices();
            app.mic_devices = list_recording_devices();
            if let AppMode::Streaming(ref stream) = app.mode {
                stream.haptics_recheck.store(true, Ordering::Release);
                stream.audio_device_recheck.store(true, Ordering::Release);
            }
        }
        let got_new_frame =
            app.poll_background(&mut gamepads, &mut kbm, window.is_relative_mouse_mode());

        let is_streaming = matches!(app.mode, AppMode::Streaming(_));
        if was_streaming && !is_streaming {
            backend.clear_video_cache();
        }
        was_streaming = is_streaming;

        let lock_state_changed = window.is_relative_mouse_mode() != prev_mouse_locked;

        let want_hdr = matches!(&app.mode, AppMode::Streaming(s) if s.codec == VideoCodec::HevcHdr);
        let hdr_mode_changed = want_hdr != prev_want_hdr;
        if hdr_mode_changed
            || (want_hdr && (summary.resized.is_some() || summary.toggle_fullscreen))
        {
            match backend.set_hdr_output(want_hdr) {
                Ok(active) => {
                    if hdr_mode_changed {
                        tracing::info!(
                            renderer = backend.short_label(),
                            want_hdr,
                            hdr_output_active = active,
                            "Updated HDR output mode"
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!("HDR output transition failed: {err:#}");
                }
            }
            prev_want_hdr = want_hdr;
        }

        let should_render = !is_streaming
            || app.settings.vsync
            || got_new_frame
            || hdr_mode_changed
            || lock_state_changed
            || app.settings_open
            || app.kbm_modal_open
            || summary.last_kbm_input.is_some()
            || !raw_input.events.is_empty()
            || summary.resized.is_some()
            || summary.toggle_fullscreen
            || summary.toggle_hud
            || summary.toggle_mouse_lock
            || summary.toggle_mic
            || summary.go_home
            || summary.take_screenshot
            || summary.toggle_recording
            || summary.save_replay_clip
            || summary.escape_pressed
            || last_present.elapsed() >= IDLE_UI_REFRESH_INTERVAL;
        if !should_render {
            continue;
        }

        if !waited_for_latency {
            backend.wait_for_frame_latency(100);
        }

        raw_input.time = Some(start_time.elapsed().as_secs_f64());

        let vsync = if is_streaming {
            app.settings.vsync
        } else {
            true
        };
        if let Err(err) = backend.set_vsync(vsync) {
            tracing::warn!("Failed updating VSync mode ({vsync}): {err:#}");
        }

        let render_t0 = Instant::now();
        let clear_rgba = if is_streaming {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            [32.0 / 255.0, 39.0 / 255.0, 52.0 / 255.0, 1.0]
        };
        backend.begin_frame(clear_rgba);

        let sharpness = f32::from(app.settings.sharpness_pct.min(100)) / 100.0;
        if let AppMode::Streaming(ref stream) = app.mode
            && let Some(ref latest) = stream.current_frame
            && let Err(err) = backend.draw_video_frame(
                &latest.frame,
                VideoRenderOptions {
                    filter_mode: app.settings.filter_mode,
                    sharpness,
                    scale_mode: app.settings.scale_mode,
                },
            )
        {
            tracing::warn!("draw_video_frame failed: {err:#}");
        }

        let draw_egui = !is_streaming
            || app.settings_open
            || app.kbm_modal_open
            || matches!(
                &app.mode,
                AppMode::Streaming(s)
                    if s.show_hud
                        || s.pin_prompt.is_some()
                        || s.osk_prompt.is_some()
                        || s.is_recording()
                        || s.has_active_toast()
            );
        let hdr_output_active = backend.is_hdr_output_active();
        let mouse_locked = window.is_relative_mouse_mode();
        if draw_egui {
            let gamepad_name = gamepads.connected_name().map(str::to_owned);
            let decoder_hw = backend.decoder_hardware();
            let full_output = egui_ctx.run_ui(raw_input.take(), |ui| {
                app.ui(
                    ui,
                    gamepad_name.as_deref(),
                    decoder_hw.clone(),
                    hdr_output_active,
                    mouse_locked,
                );
            });

            handle_platform_commands(&window, &full_output.platform_output.commands);

            let clipped = egui_ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
            backend.paint_egui(
                &full_output.textures_delta,
                full_output.pixels_per_point,
                &clipped,
                hdr_output_active,
            );
        } else {
            let _ = raw_input.take();
        }

        match backend.present(vsync) {
            Ok(()) => {
                last_present = Instant::now();
                let render_ms = render_t0.elapsed().as_secs_f32() * 1000.0;
                if let AppMode::Streaming(ref mut stream) = app.mode {
                    stream.render_ms = render_ms;
                    stream.record_present_latency();
                    if !stream.logged_pipeline_diag
                        && stream.frames_decoded.load(Ordering::Relaxed) >= 60
                    {
                        stream.logged_pipeline_diag = true;
                        let decode_ms = stream.current_frame.as_ref().map_or(0.0, |f| f.decode_ms);
                        let buf_frames = stream.decoder_lag_frames.load(Ordering::Relaxed);
                        tracing::info!(
                            renderer = backend.short_label(),
                            decode_ms,
                            render_ms,
                            pipe_p50_ms = stream.pipe_p50_ms,
                            pipe_p95_ms = stream.pipe_p95_ms,
                            buf_frames,
                            vsync,
                            "Pipeline latency sample"
                        );
                    }
                }
            }
            Err(err) => {
                tracing::warn!("{} present failed: {err:#}", backend.short_label());
            }
        }

        app.process_post_ui(&mut gamepads);

        if let AppMode::Streaming(ref mut stream) = app.mode
            && std::mem::take(&mut stream.mouse_lock_toggle_requested)
            && app.settings.kbm_enabled
            && !app.settings_open
            && !stream.hud_drawer_open
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
            && !app.settings_open
            && !app.kbm_modal_open
            && matches!(
                &app.mode,
                AppMode::Streaming(s)
                    if !s.hud_drawer_open && s.pin_prompt.is_none() && s.osk_prompt.is_none()
            );
        if !allow_mouse_lock && window.is_relative_mouse_mode() {
            window.set_relative_mouse_mode(false);
            kbm.clear_state();
        }
    }

    window.set_relative_mouse_mode(false);
    kbm.clear_state();
    app.stop_stream(&mut gamepads, None);
    let _ = backend.set_hdr_output(false);
    backend.clear_video_cache();
    drop(app);
    drop(backend);
    drop(window);
    Ok(())
}

fn apply_window_icon(hwnd: HWND) {
    let Ok(hinst) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    let icon_res = PCWSTR(std::ptr::without_provenance(1));
    let small_w = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let small_h = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);
    let big_w = unsafe { GetSystemMetrics(SM_CXICON) }.max(32);
    let big_h = unsafe { GetSystemMetrics(SM_CYICON) }.max(32);

    if let Ok(small_icon) = unsafe {
        LoadImageW(
            Some(hinst.into()),
            icon_res,
            IMAGE_ICON,
            small_w,
            small_h,
            LR_SHARED,
        )
    } {
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(small_icon.0 as isize)),
            );
        }
    }
    if let Ok(big_icon) = unsafe {
        LoadImageW(
            Some(hinst.into()),
            icon_res,
            IMAGE_ICON,
            big_w,
            big_h,
            LR_SHARED,
        )
    } {
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(big_icon.0 as isize)),
            );
        }
    }
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

fn is_safe_https_url(url: &str) -> bool {
    let trimmed = url.trim();
    trimmed.len() <= 2048
        && trimmed
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
        && trimmed.len() > 8
        && !trimmed.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn open_url(url: &str) {
    let trimmed = url.trim();
    if !is_safe_https_url(trimmed) {
        tracing::warn!(url = trimmed, "Blocked unsafe URL before ShellExecuteW");
        return;
    }
    let op = HSTRING::from("open");
    let target = HSTRING::from(trimmed);
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
            url = trimmed,
            code = ret.0 as isize,
            "ShellExecuteW failed opening URL"
        );
    }
}

pub(crate) fn open_capture_folder(override_dir: Option<&str>) -> Result<()> {
    let dir = stream::resolve_capture_dir(override_dir, true);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed creating capture folder {}", dir.display()))?;
    if !dir.is_dir() {
        anyhow::bail!("capture path is not a directory: {}", dir.display());
    }
    let dir_str = dir
        .to_str()
        .context("capture folder path contains non-UTF-8 characters")?;
    let op = HSTRING::from("explore");
    let target = HSTRING::from(dir_str);
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
        anyhow::bail!(
            "ShellExecuteW failed opening {} (code {})",
            dir.display(),
            ret.0 as isize
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct StreamShortcuts {
    toggle_hud: bool,
    escape_pressed: bool,
    toggle_mic: bool,
    go_home: bool,
    take_screenshot: bool,
    toggle_recording: bool,
    save_replay_clip: bool,
}

pub(crate) enum AppMode {
    Home,
    Streaming(Box<ActiveStream>),
}

pub(crate) struct AppState {
    pub(crate) store: ConsoleStore,
    pub(crate) settings_store: SettingsStore,
    pub(crate) saved_consoles: Vec<SavedConsole>,
    pub(crate) discovered_consoles: Vec<DiscoveredConsole>,
    pub(crate) audio_devices: Vec<String>,
    pub(crate) mic_devices: Vec<String>,
    pub(crate) discovery_rx: Option<Receiver<std::result::Result<Vec<DiscoveredConsole>, String>>>,
    pub(crate) wake_rx: Option<Receiver<std::result::Result<String, String>>>,
    pub(crate) settings: StreamSettings,
    pub(crate) active_renderer_label: &'static str,
    pub(crate) active_renderer_short: &'static str,
    pub(crate) status_banner: Option<StatusBanner>,
    pub(crate) register_modal: Option<RegisterModal>,
    pub(crate) edit_modal: Option<EditConsoleModal>,
    pub(crate) settings_open: bool,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) settings_focus_row: usize,
    pub(crate) kbm_modal_open: bool,
    pub(crate) kbm_listening_action: Option<KbmAction>,
    pub(crate) mode: AppMode,
    pub(crate) selected_console_idx: usize,
    gamepad_nav: GamepadHomeNav,
    pub(crate) active_stream_target: Option<(usize, bool)>,
    pub(crate) reconnect: Option<ReconnectState>,
    pub(crate) pending_home_action: Option<HomeAction>,
    pub(crate) pending_stream_connect: Option<(usize, bool)>,
}

fn step_wrap(idx: &mut usize, count: usize, forward: bool) {
    if count == 0 {
        *idx = 0;
        return;
    }
    if *idx >= count {
        *idx = count - 1;
    }
    *idx = if forward {
        (*idx + 1) % count
    } else if *idx == 0 {
        count - 1
    } else {
        *idx - 1
    };
}

struct SettingsPadCtx<'a> {
    settings: &'a mut StreamSettings,
    settings_open: &'a mut bool,
    settings_tab: &'a mut SettingsTab,
    settings_focus_row: &'a mut usize,
    gamepad_nav: &'a mut GamepadHomeNav,
    audio_devices: &'a [String],
    mic_devices: &'a [String],
}

fn handle_settings_pad_event(
    ctx: &mut SettingsPadCtx<'_>,
    is_streaming: bool,
    event: HomeNavEvent,
) {
    match event {
        HomeNavEvent::Back | HomeNavEvent::ToggleSettings => {
            *ctx.settings_open = false;
            if is_streaming {
                ctx.gamepad_nav.arm_release_guard();
            }
        }
        HomeNavEvent::PrevSettingsTab => {
            *ctx.settings_tab = ctx.settings_tab.prev();
            *ctx.settings_focus_row = 0;
        }
        HomeNavEvent::NextSettingsTab => {
            *ctx.settings_tab = ctx.settings_tab.next();
            *ctx.settings_focus_row = 0;
        }
        HomeNavEvent::PrevConsole | HomeNavEvent::NextConsole => {
            let count = settings_row_count(
                *ctx.settings_tab,
                ctx.settings.kbm_enabled,
                ctx.settings.wan_profile_enabled,
            );
            step_wrap(
                ctx.settings_focus_row,
                count,
                event == HomeNavEvent::NextConsole,
            );
        }
        HomeNavEvent::Left | HomeNavEvent::Right | HomeNavEvent::Connect => {
            let _ = apply_settings_nav(
                ctx.settings,
                *ctx.settings_tab,
                *ctx.settings_focus_row,
                event,
                is_streaming,
                ctx.audio_devices,
                ctx.mic_devices,
            );
            let count = settings_row_count(
                *ctx.settings_tab,
                ctx.settings.kbm_enabled,
                ctx.settings.wan_profile_enabled,
            );
            if count > 0 && *ctx.settings_focus_row >= count {
                *ctx.settings_focus_row = count - 1;
            }
        }
        _ => {}
    }
}

fn handle_streaming_pad_overlay(
    ctx: &mut SettingsPadCtx<'_>,
    kbm_modal_open: bool,
    stream: &mut ActiveStream,
    pad: &ControllerState,
) -> bool {
    let events = ctx.gamepad_nav.update(pad);
    if events.is_empty() {
        return false;
    }
    for event in events {
        if *ctx.settings_open && !kbm_modal_open {
            handle_settings_pad_event(ctx, true, event);
        } else if stream.hud_drawer_open {
            match event {
                HomeNavEvent::Back | HomeNavEvent::ToggleSettings => {
                    stream.hud_drawer_open = false;
                    ctx.gamepad_nav.arm_release_guard();
                }
                HomeNavEvent::PrevConsole | HomeNavEvent::NextConsole => {
                    step_wrap(
                        &mut stream.hud_focus_row,
                        HUD_QUICK_ROWS,
                        event == HomeNavEvent::NextConsole,
                    );
                }
                HomeNavEvent::Left | HomeNavEvent::Right | HomeNavEvent::Connect => {
                    let (_, action) = apply_hud_nav(ctx.settings, stream.hud_focus_row, event);
                    match action {
                        HudNavAction::None => {}
                        HudNavAction::ToggleMic => {
                            stream.mic_toggle_requested = true;
                        }
                        HudNavAction::GoHome => {
                            stream.go_home_requested = true;
                        }
                        HudNavAction::OpenSettings => {
                            stream.hud_drawer_open = false;
                            *ctx.settings_open = true;
                            *ctx.settings_focus_row = 0;
                            ctx.gamepad_nav.arm_release_guard();
                        }
                        HudNavAction::Disconnect => {
                            stream.disconnect_requested = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    true
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
            mic_devices: list_recording_devices(),
            discovery_rx: None,
            wake_rx: None,
            settings,
            active_renderer_label: "Direct3D 11",
            active_renderer_short: "D3D11",
            status_banner,
            register_modal: None,
            edit_modal: None,
            settings_open: false,
            settings_tab: SettingsTab::Video,
            settings_focus_row: 0,
            kbm_modal_open: false,
            kbm_listening_action: None,
            mode: AppMode::Home,
            selected_console_idx: 0,
            gamepad_nav: GamepadHomeNav::default(),
            active_stream_target: None,
            reconnect: None,
            pending_home_action: None,
            pending_stream_connect: None,
        }
    }

    fn clamp_selected_console(&mut self) {
        if self.saved_consoles.is_empty() {
            self.selected_console_idx = 0;
        } else if self.selected_console_idx >= self.saved_consoles.len() {
            self.selected_console_idx = self.saved_consoles.len() - 1;
        }
    }

    fn handle_home_gamepad_events(&mut self, gamepads: &mut GamepadManager) -> bool {
        let _ = gamepads.take_mic_toggle();
        let Some(state) = gamepads.poll_state(false) else {
            self.gamepad_nav.reset();
            return false;
        };
        let events = self.gamepad_nav.update(&state);
        if events.is_empty() {
            return false;
        }
        self.clamp_selected_console();
        let prev_settings = self.settings.clone();

        for event in events {
            let no_modal_open = !self.settings_open
                && !self.kbm_modal_open
                && self.register_modal.is_none()
                && self.edit_modal.is_none();

            if self.settings_open
                && !self.kbm_modal_open
                && self.register_modal.is_none()
                && self.edit_modal.is_none()
            {
                let mut ctx = SettingsPadCtx {
                    settings: &mut self.settings,
                    settings_open: &mut self.settings_open,
                    settings_tab: &mut self.settings_tab,
                    settings_focus_row: &mut self.settings_focus_row,
                    gamepad_nav: &mut self.gamepad_nav,
                    audio_devices: &self.audio_devices,
                    mic_devices: &self.mic_devices,
                };
                handle_settings_pad_event(&mut ctx, false, event);
                continue;
            }

            match event {
                HomeNavEvent::Back => {
                    if self.kbm_modal_open {
                        self.kbm_modal_open = false;
                        self.kbm_listening_action = None;
                    } else if self.edit_modal.is_some() {
                        self.edit_modal = None;
                    } else if self.register_modal.is_some() {
                        self.register_modal = None;
                    } else if self.reconnect.is_some() {
                        self.reconnect = None;
                    }
                }
                HomeNavEvent::ToggleSettings if no_modal_open => {
                    self.settings_open = true;
                    self.settings_focus_row = 0;
                }
                HomeNavEvent::PrevConsole | HomeNavEvent::NextConsole if no_modal_open => {
                    step_wrap(
                        &mut self.selected_console_idx,
                        self.saved_consoles.len(),
                        event == HomeNavEvent::NextConsole,
                    );
                }
                HomeNavEvent::Connect if no_modal_open => {
                    if let Some(ref rc) = self.reconnect {
                        self.pending_stream_connect = Some((rc.saved_idx, rc.use_wan));
                    } else if !self.saved_consoles.is_empty() {
                        self.pending_stream_connect = Some((self.selected_console_idx, false));
                    }
                }
                HomeNavEvent::Wake if no_modal_open => {
                    if let Some(console) =
                        self.saved_consoles.get(self.selected_console_idx).cloned()
                    {
                        self.start_wake(&console);
                    }
                }
                HomeNavEvent::Discover if no_modal_open && self.discovery_rx.is_none() => {
                    self.status_banner = Some(StatusBanner::info(
                        "Scanning LAN & saved remote hosts for PS5 consoles...",
                    ));
                    self.start_discovery();
                }
                _ => {}
            }
        }
        self.apply_settings_side_effects(&prev_settings);
        true
    }

    pub(crate) fn persist_settings(&self) {
        if let Err(err) = self.settings_store.save(&self.settings) {
            tracing::warn!("Failed saving settings.json: {err:#}");
        }
    }

    pub(crate) fn start_discovery(&mut self) {
        if self.discovery_rx.is_some() {
            return;
        }
        let mut extra_hosts = Vec::new();
        for console in &self.saved_consoles {
            if !console.host.trim().is_empty() {
                extra_hosts.push(console.host.clone());
            }
            if let Some(ref remote) = console.remote_host {
                extra_hosts.push(remote.clone());
            }
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::Builder::new()
            .name("gnsh-lan-discover".into())
            .spawn(move || {
                let res = discover_ps5_hosts(DISCOVERY_TIMEOUT, &extra_hosts)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(res);
            })
            .ok();
        self.discovery_rx = Some(rx);
    }

    fn start_wake(&mut self, console: &SavedConsole) {
        if self.wake_rx.is_some() {
            return;
        }
        let name = console.name.clone();
        let regist_key_hex = console.regist_key_hex();
        let mut targets = Vec::new();
        if !console.host.trim().is_empty() {
            targets.push(console.host.clone());
        }
        if let Some(ref remote) = console.remote_host
            && !remote.trim().is_empty()
            && !targets.iter().any(|t| t.eq_ignore_ascii_case(remote))
        {
            targets.push(remote.clone());
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::Builder::new()
            .name("gnsh-wake".into())
            .spawn(move || {
                let mut sent = Vec::new();
                let mut last_err = None;
                for target in &targets {
                    match wake_ps5(target, &regist_key_hex) {
                        Ok(()) => sent.push(target.clone()),
                        Err(err) => last_err = Some(err),
                    }
                }
                let res = if !sent.is_empty() {
                    Ok(format!(
                        "Wake-up packet sent to {name} ({}).",
                        sent.join(", ")
                    ))
                } else {
                    Err(format!(
                        "Wake failed for {name}: {}",
                        last_err
                            .map(|e| format!("{e:#}"))
                            .unwrap_or_else(|| "no valid host".to_string())
                    ))
                };
                let _ = tx.send(res);
            })
            .ok();
        self.wake_rx = Some(rx);
    }

    fn handle_shortcuts(&mut self, shortcuts: StreamShortcuts) {
        match &mut self.mode {
            AppMode::Home => {
                if shortcuts.escape_pressed {
                    if self.edit_modal.is_some() {
                        self.edit_modal = None;
                    } else if self.register_modal.is_some() {
                        self.register_modal = None;
                    } else if self.settings_open {
                        self.settings_open = false;
                    } else if self.reconnect.is_some() {
                        self.reconnect = None;
                    }
                }
            }
            AppMode::Streaming(stream) => {
                if shortcuts.toggle_hud {
                    stream.show_hud = !stream.show_hud;
                    if !stream.show_hud {
                        stream.hud_drawer_open = false;
                    }
                }
                if shortcuts.toggle_mic {
                    stream.mic_toggle_requested = true;
                }
                if shortcuts.go_home && stream.osk_prompt.is_none() && stream.pin_prompt.is_none() {
                    stream.go_home_requested = true;
                }
                if shortcuts.take_screenshot {
                    stream.screenshot_requested = true;
                }
                if shortcuts.toggle_recording {
                    stream.record_toggle_requested = true;
                }
                if shortcuts.save_replay_clip {
                    stream.save_clip_requested = true;
                }
                if shortcuts.escape_pressed {
                    if let Some(osk) = stream.osk_prompt.as_mut() {
                        osk.cancel_requested = true;
                    } else if self.settings_open {
                        self.settings_open = false;
                    } else if stream.hud_drawer_open {
                        stream.hud_drawer_open = false;
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
                            && should_auto_update_lan_host(saved, &disc.addr)
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
                            kind: ui::BannerKind::Info,
                            ..
                        })
                    ) {
                        self.status_banner = if self.discovered_consoles.is_empty() {
                            Some(StatusBanner::info(
                                "Discovery finished: no PS5 consoles found.",
                            ))
                        } else {
                            None
                        };
                    }
                }
                Err(err) => {
                    self.status_banner =
                        Some(StatusBanner::error(format!("PS5 discovery failed: {err}")));
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
            self.clamp_selected_console();
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
        let mut stream_quit_error_reason: Option<String> = None;
        let mut cleared_reconnect = false;
        let prev_pad_settings = self.settings.clone();

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
                        cleared_reconnect = true;
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
                            stream_quit_error_reason = Some(reason);
                        } else {
                            tracing::info!(host = %stream.console_host, "Stream session disconnected: {reason}");
                            stream_quit_banner =
                                Some(StatusBanner::info(format!("Disconnected: {reason}")));
                        }
                    }
                }
            }

            if stream_quit_banner.is_none() && stream_quit_error_reason.is_none() {
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

                let capture_dir = prev_pad_settings.capture_dir.as_deref();
                if std::mem::take(&mut stream.screenshot_requested) {
                    stream.trigger_screenshot(capture_dir);
                    ui_dirty = true;
                }
                if std::mem::take(&mut stream.record_toggle_requested) {
                    stream.toggle_recording(capture_dir);
                    ui_dirty = true;
                }
                if std::mem::take(&mut stream.save_clip_requested) {
                    stream.save_replay_clip(capture_dir);
                    ui_dirty = true;
                }
                ui_dirty |= stream.poll_capture_notices();

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
                let modal_prompt_active =
                    stream.pin_prompt.is_some() || stream.osk_prompt.is_some();
                if !modal_prompt_active
                    && !self.settings_open
                    && !stream.hud_drawer_open
                    && let Some(ref pad) = pad_state
                    && self.gamepad_nav.poll_hud_chord(pad.buttons)
                {
                    stream.show_hud = true;
                    stream.hud_drawer_open = true;
                    stream.hud_focus_row = 0;
                    self.gamepad_nav.arm_release_guard();
                    ui_dirty = true;
                }

                let overlay_open =
                    !modal_prompt_active && (self.settings_open || stream.hud_drawer_open);
                let pad_for_ps5 = if overlay_open {
                    if let Some(ref pad) = pad_state {
                        let mut ctx = SettingsPadCtx {
                            settings: &mut self.settings,
                            settings_open: &mut self.settings_open,
                            settings_tab: &mut self.settings_tab,
                            settings_focus_row: &mut self.settings_focus_row,
                            gamepad_nav: &mut self.gamepad_nav,
                            audio_devices: &self.audio_devices,
                            mic_devices: &self.mic_devices,
                        };
                        ui_dirty |= handle_streaming_pad_overlay(
                            &mut ctx,
                            self.kbm_modal_open,
                            stream,
                            pad,
                        );
                    } else {
                        self.gamepad_nav.reset();
                    }
                    None
                } else {
                    pad_state
                };
                let overlay_just_opened = overlay_open && !stream.pad_overlay_was_active;
                stream.pad_overlay_was_active = overlay_open;

                let kbm_active = self.settings.kbm_enabled
                    && mouse_locked
                    && !self.kbm_modal_open
                    && !modal_prompt_active;
                let kbm_state = if kbm_active {
                    Some(kbm.sample_state(
                        KbmConfig {
                            sensitivity_pct: self.settings.kbm_sensitivity_pct,
                            deadzone_pct: self.settings.kbm_deadzone_pct,
                            curve_pct: self.settings.kbm_curve_pct,
                            invert_y: self.settings.kbm_invert_y,
                        },
                        &self.settings.kbm_bindings,
                    ))
                } else {
                    None
                };
                let combined = match (pad_for_ps5, kbm_state.as_ref()) {
                    (Some(pad), Some(kb)) => Some(merge_controller_states(&pad, kb)),
                    (Some(pad), None) => Some(pad),
                    (None, Some(_)) => kbm_state,
                    (None, None) => {
                        if stream.kbm_was_active || overlay_just_opened {
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
        } else {
            ui_dirty |= self.handle_home_gamepad_events(gamepads);
            if let Some(ref rc) = self.reconnect
                && rc.is_due(Instant::now())
                && self.pending_stream_connect.is_none()
            {
                self.pending_stream_connect = Some((rc.saved_idx, rc.use_wan));
                ui_dirty = true;
            }
        }

        if cleared_reconnect {
            self.reconnect = None;
        }
        self.apply_settings_side_effects(&prev_pad_settings);

        if let Some(banner) = stream_quit_banner {
            self.stop_stream(gamepads, Some(banner));
        } else if let Some(reason) = stream_quit_error_reason {
            self.handle_stream_error_quit(gamepads, reason);
        }
        got_new_frame || ui_dirty
    }

    fn handle_stream_error_quit(&mut self, gamepads: &mut GamepadManager, reason: String) {
        let now = Instant::now();
        let can_retry = self.settings.auto_reconnect && reconnect::is_retryable_reason(&reason);
        if can_retry
            && let Some((saved_idx, use_wan)) = self.active_stream_target
            && let Some(console) = self.saved_consoles.get(saved_idx)
        {
            if let Some(ref mut rc) = self.reconnect {
                if rc.advance_after_failure(reason.clone(), now) {
                    self.stop_stream_inner(gamepads, None, true);
                    return;
                }
                let name = rc.console_name.clone();
                self.stop_stream_inner(
                    gamepads,
                    Some(StatusBanner::error(format!(
                        "Auto-reconnect failed for {name}: {reason}"
                    ))),
                    false,
                );
                return;
            }
            self.reconnect = Some(ReconnectState::start(
                saved_idx,
                console.name.clone(),
                use_wan,
                console.remote_host.is_some(),
                reason,
                now,
            ));
            self.stop_stream_inner(gamepads, None, true);
            return;
        }

        self.stop_stream_inner(
            gamepads,
            Some(StatusBanner::error(format!("Session ended: {reason}"))),
            false,
        );
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
                    self.reconnect = None;
                    self.selected_console_idx = idx;
                    self.pending_stream_connect = Some((idx, false));
                }
                HomeAction::ConnectSavedWan(idx) => {
                    self.reconnect = None;
                    self.selected_console_idx = idx;
                    self.pending_stream_connect = Some((idx, true));
                }
                HomeAction::WakeSaved(idx) => {
                    self.selected_console_idx = idx;
                    if let Some(console) = self.saved_consoles.get(idx).cloned() {
                        self.start_wake(&console);
                    }
                }
                HomeAction::CreateShortcut(idx) => {
                    self.create_shortcut_for_saved(idx, false);
                }
                HomeAction::CreateShortcutWan(idx) => {
                    self.create_shortcut_for_saved(idx, true);
                }
                HomeAction::EditSaved(idx) => {
                    self.selected_console_idx = idx;
                    if let Some(console) = self.saved_consoles.get(idx) {
                        self.edit_modal = Some(EditConsoleModal {
                            index: idx,
                            name: console.name.clone(),
                            host: console.host.clone(),
                            remote_host: console.remote_host.clone().unwrap_or_default(),
                            error: None,
                        });
                    }
                }
                HomeAction::DeleteSaved(idx) => {
                    if idx < self.saved_consoles.len() {
                        self.reconnect = None;
                        let removed = self.saved_consoles.remove(idx);
                        match self.store.save(&self.saved_consoles) {
                            Ok(()) => {
                                self.clamp_selected_console();
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
                HomeAction::RetryReconnectNow => {
                    if let Some(ref rc) = self.reconnect {
                        self.pending_stream_connect = Some((rc.saved_idx, rc.use_wan));
                    }
                }
                HomeAction::CancelReconnect => {
                    self.reconnect = None;
                }
            }
        }
    }

    fn create_shortcut_for_saved(&mut self, idx: usize, use_wan: bool) {
        self.selected_console_idx = idx;
        if let Some(console) = self.saved_consoles.get(idx) {
            match shortcut::create_desktop_shortcut(console, use_wan) {
                Ok(path) => {
                    self.status_banner = Some(StatusBanner::success(format!(
                        "Created Desktop shortcut: {}",
                        path.display()
                    )));
                }
                Err(err) => {
                    self.status_banner = Some(StatusBanner::error(format!(
                        "Failed creating Desktop shortcut: {err:#}"
                    )));
                }
            }
        }
    }

    fn stop_stream(&mut self, gamepads: &mut GamepadManager, banner: Option<StatusBanner>) {
        self.stop_stream_inner(gamepads, banner, false);
    }

    fn stop_stream_inner(
        &mut self,
        gamepads: &mut GamepadManager,
        banner: Option<StatusBanner>,
        keep_reconnect: bool,
    ) {
        if !keep_reconnect {
            self.reconnect = None;
            self.active_stream_target = None;
        }
        if let AppMode::Streaming(mut stream) = std::mem::replace(&mut self.mode, AppMode::Home) {
            stream.shutdown();
            self.gamepad_nav.arm_release_guard();
            gamepads.set_rumble(0, 0);
            gamepads.set_led(0, 0, 0);
            gamepads.set_mic_led(false);
            gamepads.clear_trigger_effects();
            if let Some(b) = banner {
                self.status_banner = Some(b);
            }
        }
    }

    fn apply_settings_side_effects(&mut self, prev: &StreamSettings) {
        if self.settings == *prev {
            return;
        }
        if let AppMode::Streaming(ref mut stream) = self.mode {
            if self.settings.volume_pct != prev.volume_pct {
                stream
                    .volume_pct
                    .store(self.settings.volume_pct.min(100), Ordering::Relaxed);
            }
            if self.settings.audio_device != prev.audio_device {
                if let Ok(mut guard) = stream.audio_device.lock() {
                    *guard = self.settings.audio_device.clone();
                }
                stream.audio_device_recheck.store(true, Ordering::Release);
            }
            if self.settings.mic_device != prev.mic_device {
                stream.set_mic_device(self.settings.mic_device.clone());
            }
            if self.settings.replay_buffer_enabled != prev.replay_buffer_enabled
                || self.settings.replay_buffer_secs != prev.replay_buffer_secs
            {
                stream.update_replay_settings(
                    self.settings.replay_buffer_enabled,
                    self.settings.replay_buffer_secs,
                );
            }
        }
        if self.settings.renderer_backend != prev.renderer_backend {
            self.status_banner = Some(StatusBanner::info(format!(
                "Renderer set to {}. Restart gnsh-play to apply.",
                self.settings.renderer_backend.label()
            )));
        }
        self.persist_settings();
    }

    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        gamepad_name: Option<&str>,
        hw: DecoderHardware,
        hdr_output_active: bool,
        mouse_locked: bool,
    ) {
        if let Some((idx, use_wan)) = self.pending_stream_connect.take() {
            self.begin_stream(idx, use_wan, hw);
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
                    &self.mic_devices,
                    self.active_renderer_short,
                    hdr_output_active,
                    mouse_locked,
                    &mut self.kbm_modal_open,
                    &mut self.settings_open,
                );
            }
        }
        self.render_settings_modal(ui.ctx(), gamepad_name.is_some());
        self.render_kbm_bindings_modal(ui.ctx());
        self.apply_settings_side_effects(&prev_settings);
    }

    fn begin_stream(&mut self, saved_idx: usize, use_wan: bool, hw: DecoderHardware) {
        let Some(console) = self.saved_consoles.get(saved_idx) else {
            self.reconnect = None;
            self.active_stream_target = None;
            self.status_banner = Some(StatusBanner::error("Selected console no longer exists."));
            return;
        };
        let target_host = if use_wan {
            match console.remote_host.as_deref() {
                Some(remote) => remote,
                None => {
                    self.reconnect = None;
                    self.active_stream_target = None;
                    self.status_banner = Some(StatusBanner::error(
                        "No Remote / WAN host configured for this console.",
                    ));
                    return;
                }
            }
        } else {
            &console.host
        };
        match ActiveStream::start_with_host(console, target_host, &self.settings, use_wan, hw) {
            Ok(stream) => {
                self.active_stream_target = Some((saved_idx, use_wan));
                self.status_banner = None;
                self.register_modal = None;
                self.edit_modal = None;
                self.settings_open = false;
                self.mode = AppMode::Streaming(Box::new(stream));
            }
            Err(err) => {
                let msg = format!("{err:#}");
                tracing::warn!(
                    "Unable to connect to {} ({}): {msg}",
                    console.name,
                    target_host
                );
                if let Some(ref mut rc) = self.reconnect {
                    if !rc.advance_after_failure(msg.clone(), Instant::now()) {
                        let name = rc.console_name.clone();
                        self.reconnect = None;
                        self.active_stream_target = None;
                        self.status_banner = Some(StatusBanner::error(format!(
                            "Auto-reconnect failed for {name}: {msg}"
                        )));
                    }
                } else {
                    self.active_stream_target = None;
                    self.status_banner = Some(StatusBanner::error(format!(
                        "Unable to connect to {} ({}): {msg}",
                        console.name, target_host
                    )));
                }
            }
        }
    }
}

fn should_auto_update_lan_host(saved: &SavedConsole, disc_addr: &str) -> bool {
    let Ok(existing_ip) = saved.host.trim().parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let Ok(discovered_ip) = disc_addr.trim().parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    existing_ip.is_private()
        && discovered_ip.is_private()
        && !host_matches_remote_probe(saved, disc_addr)
        && saved.host != disc_addr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_console() -> SavedConsole {
        SavedConsole {
            id: "001122334455".into(),
            name: "PS5".into(),
            host: "192.168.1.50".into(),
            remote_host: Some("85.100.12.34".into()),
            mac: [0, 0x11, 0x22, 0x33, 0x44, 0x55],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        }
    }

    #[test]
    fn lan_auto_update_rejects_public_or_remote_ips_and_allows_private_dhcp() {
        let saved = sample_console();
        assert!(!should_auto_update_lan_host(&saved, "85.100.12.34"));
        assert!(!should_auto_update_lan_host(
            &SavedConsole {
                remote_host: None,
                ..saved.clone()
            },
            "85.100.12.34"
        ));
        assert!(!should_auto_update_lan_host(&saved, "192.168.1.50"));
        assert!(should_auto_update_lan_host(&saved, "192.168.1.55"));

        let hostname_saved = SavedConsole {
            host: "my-ps5.local".into(),
            ..saved
        };
        assert!(!should_auto_update_lan_host(
            &hostname_saved,
            "192.168.1.55"
        ));
    }

    #[test]
    fn url_launcher_allows_https_and_rejects_dangerous_schemes() {
        assert!(is_safe_https_url(
            "https://auth.api.sonyentertainmentnetwork.com/login"
        ));
        assert!(!is_safe_https_url("http://example.com"));
        assert!(!is_safe_https_url("file:///C:/Windows/System32/cmd.exe"));
        assert!(!is_safe_https_url(r"\\attacker\share\payload.exe"));
        assert!(!is_safe_https_url("javascript:alert(1)"));
        assert!(!is_safe_https_url("https://example.com/\nfoo"));
        assert!(!is_safe_https_url("https://"));
    }

    fn cli_args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn cli_parser_supports_help_list_discover_and_connect_flags() {
        assert_eq!(
            parse_cli_args(&cli_args(&[])),
            Ok(CliAction::RunGui(LaunchOptions::default()))
        );
        assert_eq!(parse_cli_args(&cli_args(&["-h"])), Ok(CliAction::ShowHelp));
        assert_eq!(
            parse_cli_args(&cli_args(&["--help", "--connect", "PS5"])),
            Ok(CliAction::ShowHelp)
        );
        assert_eq!(
            parse_cli_args(&cli_args(&["--discover"])),
            Ok(CliAction::DiscoverLan)
        );
        assert_eq!(
            parse_cli_args(&cli_args(&["--list"])),
            Ok(CliAction::ListConsoles)
        );
        assert_eq!(
            parse_cli_args(&cli_args(&["--connect", "LivingRoom"])),
            Ok(CliAction::RunGui(LaunchOptions {
                connect_target: Some("LivingRoom".into()),
                use_wan: false,
                start_fullscreen: false,
            }))
        );
        assert_eq!(
            parse_cli_args(&cli_args(&[
                "--connect=001122334455",
                "--wan",
                "--fullscreen"
            ])),
            Ok(CliAction::RunGui(LaunchOptions {
                connect_target: Some("001122334455".into()),
                use_wan: true,
                start_fullscreen: true,
            }))
        );
        assert_eq!(
            parse_cli_args(&cli_args(&["--fullscreen"])),
            Ok(CliAction::RunGui(LaunchOptions {
                connect_target: None,
                use_wan: false,
                start_fullscreen: true,
            }))
        );
    }

    #[test]
    fn cli_parser_rejects_invalid_combinations_and_missing_values() {
        assert!(parse_cli_args(&cli_args(&["--unknown"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--wan"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--connect"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--connect", "   "])).is_err());
        assert!(parse_cli_args(&cli_args(&["--connect="])).is_err());
        assert!(parse_cli_args(&cli_args(&["--connect", "a", "--connect=b"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--discover", "--list"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--list", "--fullscreen"])).is_err());
        assert!(parse_cli_args(&cli_args(&["--discover", "--connect", "PS5"])).is_err());
    }

    #[test]
    fn cli_console_lookup_prioritizes_id_then_name_then_host() {
        let first = SavedConsole {
            id: "001122334455".into(),
            name: "LivingRoom".into(),
            host: "192.168.1.50".into(),
            remote_host: Some("85.100.12.34".into()),
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        };
        let second = SavedConsole {
            id: "998877665544".into(),
            name: "001122334455".into(),
            host: "192.168.1.60".into(),
            remote_host: None,
            mac: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xBB; 16],
        };
        let list = [first, second];

        assert_eq!(find_saved_console_for_cli(&list, "001122334455"), Some(0));
        assert_eq!(find_saved_console_for_cli(&list, "aabbccddeeff"), Some(0));
        assert_eq!(find_saved_console_for_cli(&list, "112233445566"), Some(1));
        assert_eq!(find_saved_console_for_cli(&list, "livingroom"), Some(0));
        assert_eq!(find_saved_console_for_cli(&list, "192.168.1.60"), Some(1));
        assert_eq!(find_saved_console_for_cli(&list, "85.100.12.34"), Some(0));
        assert_eq!(find_saved_console_for_cli(&list, "missing"), None);
        assert_eq!(find_saved_console_for_cli(&list, "   "), None);
    }
}
