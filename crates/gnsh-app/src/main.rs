#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod egui_d3d11;
mod logging;
mod storage;
mod stream;
mod ui;

use anyhow::{Context as _, Result};
use crossbeam_channel::Receiver;
use egui_d3d11::EguiD3d11Painter;
use gnsh_input::{
    AppWindow, GamepadManager, KbmAction, KbmConfig, KbmEmulator, merge_controller_states,
};
use gnsh_media::{D3D11Renderer, list_playback_devices};
use gnsh_proto::{
    ControllerState, DiscoveredConsole, RegisteredConsole, SessionEvent, UDP_RECV_BUF_BYTES,
    VideoCodec, discover_ps5_hosts, discover_ps5_lan, init_lib, wake_ps5,
};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use storage::{ConsoleStore, SavedConsole, SettingsStore, StoredSettings};
use stream::{ActiveStream, IDR_COOLDOWN, LoginPinPrompt, OskPrompt};
use ui::{
    EditConsoleModal, HomeAction, RegisterModal, StatusBanner, StreamSettings, apply_theme,
    console_state_label, format_mac_id, host_matches_remote_probe, render_streaming_ui,
    saved_from_registered, upsert_saved_console,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::HSTRING;

const DEFAULT_WIDTH: u32 = 1280;
const DEFAULT_HEIGHT: u32 = 720;
pub(crate) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const LOW_LATENCY_INPUT_WAIT: Duration = Duration::from_millis(1);
const IDLE_UI_REFRESH_INTERVAL: Duration = Duration::from_millis(50);

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
        logging::attach_parent_console();
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

fn run_gui() -> Result<()> {
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
        let mut toggle_mic_for_shortcuts = summary.toggle_mic;
        let mut go_home_for_shortcuts = summary.go_home;
        if app.kbm_modal_open {
            toggle_mic_for_shortcuts = false;
            go_home_for_shortcuts = false;
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

        app.handle_shortcuts(
            summary.toggle_hud,
            escape_for_shortcuts,
            toggle_mic_for_shortcuts,
            go_home_for_shortcuts,
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
            || app.kbm_modal_open
            || summary.last_kbm_input.is_some()
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
            || app.kbm_modal_open
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
            && !app.kbm_modal_open
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
    pub(crate) discovery_rx: Option<Receiver<std::result::Result<Vec<DiscoveredConsole>, String>>>,
    pub(crate) wake_rx: Option<Receiver<std::result::Result<String, String>>>,
    pub(crate) settings: StreamSettings,
    pub(crate) status_banner: Option<StatusBanner>,
    pub(crate) register_modal: Option<RegisterModal>,
    pub(crate) edit_modal: Option<EditConsoleModal>,
    pub(crate) kbm_modal_open: bool,
    pub(crate) kbm_listening_action: Option<KbmAction>,
    pub(crate) mode: AppMode,
    pub(crate) pending_home_action: Option<HomeAction>,
    pub(crate) pending_stream_connect: Option<(usize, bool)>,
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
            edit_modal: None,
            kbm_modal_open: false,
            kbm_listening_action: None,
            mode: AppMode::Home,
            pending_home_action: None,
            pending_stream_connect: None,
        }
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

    fn handle_shortcuts(
        &mut self,
        toggle_hud: bool,
        escape_pressed: bool,
        toggle_mic: bool,
        go_home: bool,
    ) {
        match &mut self.mode {
            AppMode::Home => {
                if escape_pressed {
                    if self.edit_modal.is_some() {
                        self.edit_modal = None;
                    } else if self.register_modal.is_some() {
                        self.register_modal = None;
                    }
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
                            && !host_matches_remote_probe(saved, &disc.addr)
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
                    && !self.kbm_modal_open
                    && stream.pin_prompt.is_none()
                    && stream.osk_prompt.is_none();
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
                    self.pending_stream_connect = Some((idx, false));
                }
                HomeAction::ConnectSavedWan(idx) => {
                    self.pending_stream_connect = Some((idx, true));
                }
                HomeAction::WakeSaved(idx) => {
                    if let Some(console) = self.saved_consoles.get(idx).cloned() {
                        self.start_wake(&console);
                    }
                }
                HomeAction::EditSaved(idx) => {
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
        if let Some((idx, use_wan)) = self.pending_stream_connect.take() {
            self.begin_stream(idx, use_wan, device.clone());
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
                    &mut self.kbm_modal_open,
                );
            }
        }
        self.render_kbm_bindings_modal(ui.ctx());
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

    fn begin_stream(&mut self, saved_idx: usize, use_wan: bool, device: ID3D11Device) {
        let Some(console) = self.saved_consoles.get(saved_idx) else {
            self.status_banner = Some(StatusBanner::error("Selected console no longer exists."));
            return;
        };
        let target_host = if use_wan {
            match console.remote_host.as_deref() {
                Some(remote) => remote,
                None => {
                    self.status_banner = Some(StatusBanner::error(
                        "No Remote / WAN host configured for this console.",
                    ));
                    return;
                }
            }
        } else {
            &console.host
        };
        match ActiveStream::start_with_host(console, target_host, &self.settings, device) {
            Ok(stream) => {
                self.status_banner = None;
                self.register_modal = None;
                self.edit_modal = None;
                self.mode = AppMode::Streaming(Box::new(stream));
            }
            Err(err) => {
                tracing::warn!(
                    "Unable to connect to {} ({}): {err:#}",
                    console.name,
                    target_host
                );
                self.status_banner = Some(StatusBanner::error(format!(
                    "Unable to connect to {} ({}): {err:#}",
                    console.name, target_host
                )));
            }
        }
    }
}
