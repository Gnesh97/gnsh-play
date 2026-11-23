use crate::AppState;
use crate::storage::{SavedConsole, StoredSettings};
use crate::stream::ActiveStream;
use gnsh_input::{KbmAction, KbmBindings};
use gnsh_media::ScaleMode;
use gnsh_proto::{
    ConsoleState, DiscoveredConsole, MAX_BITRATE_KBPS, MIN_BITRATE_KBPS, RegisteredConsole,
    VideoCodec, VideoPreset, decode_account_id_b64, exchange_redirect_url_for_account_id,
    open_psn_login_in_browser, register_ps5, resolve_host_ipv4,
};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

pub(crate) type StreamSettings = StoredSettings;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn apply_theme(ctx: &egui::Context) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BannerKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusBanner {
    pub(crate) kind: BannerKind,
    pub(crate) text: String,
}

impl StatusBanner {
    pub(crate) fn info(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Info,
            text: text.into(),
        }
    }

    pub(crate) fn success(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Success,
            text: text.into(),
        }
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self {
            kind: BannerKind::Error,
            text: text.into(),
        }
    }
}

pub(crate) enum HomeAction {
    ConnectSaved(usize),
    ConnectSavedWan(usize),
    WakeSaved(usize),
    EditSaved(usize),
    DeleteSaved(usize),
    OpenRegister(String),
}

pub(crate) struct RegisterModal {
    pub(crate) host: String,
    pub(crate) redirect_url: String,
    pub(crate) psn_account_id: String,
    pub(crate) pin_input: String,
    pub(crate) notice: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) exchange_rx:
        Option<crossbeam_channel::Receiver<std::result::Result<String, String>>>,
    pub(crate) register_rx:
        Option<crossbeam_channel::Receiver<std::result::Result<RegisteredConsole, String>>>,
}

impl RegisterModal {
    pub(crate) fn new(host: String) -> Self {
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

pub(crate) struct EditConsoleModal {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) host: String,
    pub(crate) remote_host: String,
    pub(crate) error: Option<String>,
}

impl AppState {
    pub(crate) fn render_home_ui(&mut self, ui: &mut egui::Ui, gamepad_name: Option<&str>) {
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
                        self.status_banner = Some(StatusBanner::info(
                            "Scanning LAN & saved remote hosts for PS5 consoles...",
                        ));
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
                    render_kbm_controls(
                        ui,
                        &mut self.settings,
                        None,
                        &mut self.kbm_modal_open,
                    );
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
                                ui.label(egui::RichText::new("Scanning...").weak());
                            }
                        });
                        ui.add_space(6.0);

                        if self.discovered_consoles.is_empty() && self.discovery_rx.is_none() {
                            card_frame(ui.style()).show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.label(
                                    egui::RichText::new(
                                        "No PS5 consoles found on the local network or saved remote hosts. Ensure Remote Play is enabled on your PS5 and click \"Discover LAN\".",
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

        self.render_edit_console_modal(ui.ctx());
    }

    pub(crate) fn render_edit_console_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.edit_modal.take() else {
            return;
        };

        let mut keep_open = true;
        let mut save_requested = false;
        let mut cancel_requested = false;

        egui::Window::new("Edit Saved Console")
            .open(&mut keep_open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label("Console Name:");
                ui.add(
                    egui::TextEdit::singleline(&mut modal.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("PS5-LivingRoom"),
                );
                ui.add_space(8.0);

                ui.label("LAN Host / IP:");
                ui.add(
                    egui::TextEdit::singleline(&mut modal.host)
                        .desired_width(f32::INFINITY)
                        .hint_text("192.168.1.50"),
                );
                ui.add_space(8.0);

                ui.label("Remote / WAN / VPN Host (optional):");
                ui.add(
                    egui::TextEdit::singleline(&mut modal.remote_host)
                        .desired_width(f32::INFINITY)
                        .hint_text("my-ps5.duckdns.org or 100.x.y.z (leave blank to disable)"),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "WAN setup: forward UDP 9302, TCP 9295, and UDP 9296-9297 to your PS5 on your home router (or use Tailscale/WireGuard IP directly). :port suffixes and http:// are stripped automatically.",
                    )
                    .small()
                    .weak(),
                );

                if let Some(ref err) = modal.error {
                    ui.add_space(8.0);
                    ui.colored_label(egui::Color32::from_rgb(255, 148, 148), err);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Save")
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            )
                            .fill(egui::Color32::from_rgb(0, 112, 209)),
                        )
                        .clicked()
                    {
                        save_requested = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_requested = true;
                    }
                });
            });

        if !keep_open || cancel_requested {
            return;
        }

        if save_requested {
            let trimmed_name = modal.name.trim().to_string();
            let normalized_host = gnsh_proto::normalize_console_host(&modal.host);
            let normalized_remote = gnsh_proto::normalize_console_host(&modal.remote_host);
            if trimmed_name.is_empty() {
                modal.error = Some("Console name cannot be empty.".into());
                self.edit_modal = Some(modal);
                return;
            }
            if normalized_host.is_empty() {
                modal.error = Some("LAN Host / IP cannot be empty.".into());
                self.edit_modal = Some(modal);
                return;
            }
            let Some(console) = self.saved_consoles.get(modal.index).cloned() else {
                self.status_banner =
                    Some(StatusBanner::error("Selected console no longer exists."));
                return;
            };
            let remote_opt = if normalized_remote.is_empty() {
                None
            } else {
                Some(normalized_remote)
            };
            let prev = console.clone();
            if let Some(slot) = self.saved_consoles.get_mut(modal.index) {
                slot.name = trimmed_name;
                slot.host = normalized_host;
                slot.remote_host = remote_opt;
            }
            match self.store.save(&self.saved_consoles) {
                Ok(()) => {
                    self.status_banner = Some(StatusBanner::success(format!(
                        "Updated saved console {}.",
                        self.saved_consoles[modal.index].name
                    )));
                }
                Err(err) => {
                    if let Some(slot) = self.saved_consoles.get_mut(modal.index) {
                        *slot = prev;
                    }
                    modal.error = Some(format!("Failed saving console store: {err:#}"));
                    self.edit_modal = Some(modal);
                }
            }
        } else {
            self.edit_modal = Some(modal);
        }
    }

    pub(crate) fn render_kbm_bindings_modal(&mut self, ctx: &egui::Context) {
        if !self.kbm_modal_open {
            self.kbm_listening_action = None;
            return;
        }
        let mut open = self.kbm_modal_open;
        let mut close_clicked = false;
        egui::Window::new("KB+M DualSense Keybindings")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .show(ctx, |ui| {
                if let Some(listening) = self.kbm_listening_action {
                    ui.label(
                        egui::RichText::new(format!(
                            "Listening for {}: Press any key or mouse button (Esc to cancel)",
                            listening.label()
                        ))
                        .strong()
                        .color(egui::Color32::from_rgb(255, 208, 122)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Click any action below to bind a keyboard key or mouse button (1-5). Duplicate bindings swap automatically. Reserved: Esc, F3, F8, F11.",
                        )
                        .small()
                        .weak(),
                    );
                }

                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .show(ui, |ui| {
                        egui::Grid::new("kbm_bindings_grid")
                            .num_columns(2)
                            .striped(true)
                            .spacing([24.0, 6.0])
                            .show(ui, |ui| {
                                for action in KbmAction::ALL {
                                    ui.label(action.label());
                                    let is_listening = self.kbm_listening_action == Some(action);
                                    let btn_label = if is_listening {
                                        "> Press key/mouse <".to_string()
                                    } else {
                                        self.settings.kbm_bindings.get(action).label()
                                    };
                                    let mut button = egui::Button::new(
                                        egui::RichText::new(btn_label).color(egui::Color32::WHITE),
                                    );
                                    if is_listening {
                                        button = button.fill(egui::Color32::from_rgb(0, 112, 209));
                                    }
                                    if ui.add_sized([165.0, 22.0], button).clicked() {
                                        self.kbm_listening_action = if is_listening {
                                            None
                                        } else {
                                            Some(action)
                                        };
                                    }
                                    ui.end_row();
                                }
                            });
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Reset to Defaults").clicked() {
                        self.settings.kbm_bindings = KbmBindings::default();
                        self.kbm_listening_action = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close_clicked = true;
                        }
                    });
                });
            });

        if !open || close_clicked {
            self.kbm_modal_open = false;
            self.kbm_listening_action = None;
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
                let subtitle = match saved.remote_host.as_deref() {
                    Some(remote) => format!(
                        "LAN: {}   |   WAN: {}   |   ID: {}   |   MAC: {}",
                        saved.host,
                        remote,
                        saved.id,
                        format_mac_colon(saved.mac)
                    ),
                    None => format!(
                        "Host: {}   |   ID: {}   |   MAC: {}",
                        saved.host,
                        saved.id,
                        format_mac_colon(saved.mac)
                    ),
                };
                ui.label(egui::RichText::new(subtitle).small().weak());
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

                if ui.button("Edit").clicked() {
                    *pending_action = Some(HomeAction::EditSaved(idx));
                }

                if ui.add_enabled(!waking, egui::Button::new("Wake")).clicked() {
                    *pending_action = Some(HomeAction::WakeSaved(idx));
                }

                if let Some(ref remote) = saved.remote_host
                    && ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Connect WAN")
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            )
                            .fill(egui::Color32::from_rgb(38, 126, 86)),
                        )
                        .on_hover_text(format!("Connect over WAN/VPN host: {remote}"))
                        .clicked()
                {
                    *pending_action = Some(HomeAction::ConnectSavedWan(idx));
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

    ui.label("1. Enter your PS5 LAN/Remote host or IP and authenticate with PSN (or paste your Base64 Account ID directly).");
    ui.add_space(8.0);

    ui.label("Target PS5 Host / IP:");
    ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.host)
            .desired_width(f32::INFINITY)
            .hint_text("192.168.1.50 or my-ps5.duckdns.org"),
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
    let host = gnsh_proto::normalize_console_host(&modal.host);
    if host.is_empty() {
        modal.notice = None;
        modal.error = Some("Enter the target PS5 host or IP address.".into());
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
    open_keybinds: &mut bool,
) {
    ui.checkbox(&mut settings.kbm_enabled, "KB+M")
        .on_hover_text(settings.kbm_bindings.summary_tooltip());
    if ui
        .button("Keybinds...")
        .on_hover_text("Customize keyboard & mouse bindings for all DualSense buttons")
        .clicked()
    {
        *open_keybinds = true;
    }
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
        ui.add(
            egui::Slider::new(&mut settings.kbm_curve_pct, 50..=150)
                .clamping(egui::SliderClamping::Always)
                .suffix("%")
                .text("Curve"),
        )
        .on_hover_text(
            "Aim response curve: <100% boosts low-mid speed to counter console exponential curves; >100% increases slow micro-aim precision (default 100%)",
        );
    });
    ui.checkbox(&mut settings.kbm_invert_y, "Inv Y")
        .on_hover_text("Invert vertical mouse camera axis");
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_streaming_ui(
    ui: &mut egui::Ui,
    stream: &mut ActiveStream,
    gamepad_name: Option<&str>,
    settings: &mut StreamSettings,
    audio_devices: &[String],
    hdr_output_active: bool,
    mouse_locked: bool,
    open_keybinds: &mut bool,
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
                                open_keybinds,
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

pub(crate) fn console_state_label(state: ConsoleState) -> &'static str {
    match state {
        ConsoleState::Ready => "Ready",
        ConsoleState::Standby => "Standby",
        ConsoleState::Unknown => "Unknown",
    }
}

pub(crate) fn format_mac_colon(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub(crate) fn format_mac_id(mac: [u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub(crate) fn saved_from_registered(
    reg: RegisteredConsole,
    discovered: &[DiscoveredConsole],
) -> SavedConsole {
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
        remote_host: None,
        mac: reg.host_mac,
        psn_account_id: reg.psn_account_id,
        regist_key: reg.regist_key,
        rp_key: reg.rp_key,
    }
}

pub(crate) fn upsert_saved_console(saved: &mut Vec<SavedConsole>, mut console: SavedConsole) {
    if let Some(existing) = saved.iter_mut().find(|s| {
        s.id.eq_ignore_ascii_case(&console.id)
            || (console.mac != [0; 6] && s.mac == console.mac)
            || s.host.eq_ignore_ascii_case(&console.host)
    }) {
        if console.remote_host.is_none() {
            console.remote_host = existing.remote_host.clone();
        }
        *existing = console;
    } else {
        saved.push(console);
    }
}

fn addr_matches_host(target: &str, disc_addr: &str) -> bool {
    let t = target.trim();
    let d = disc_addr.trim();
    if t.is_empty() || d.is_empty() {
        return false;
    }
    t.eq_ignore_ascii_case(d) || resolve_host_ipv4(t).is_some_and(|ip| ip.to_string() == d)
}

pub(crate) fn host_matches_remote_probe(saved: &SavedConsole, disc_addr: &str) -> bool {
    saved
        .remote_host
        .as_deref()
        .is_some_and(|remote| addr_matches_host(remote, disc_addr))
}

pub(crate) fn find_saved_index_for_discovered(
    saved: &[SavedConsole],
    disc: &DiscoveredConsole,
) -> Option<usize> {
    saved.iter().position(|s| {
        addr_matches_host(&s.host, &disc.addr)
            || host_matches_remote_probe(s, &disc.addr)
            || (!disc.host_id.is_empty() && s.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(s.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
}

pub(crate) fn find_discovered_for_saved<'a>(
    discovered: &'a [DiscoveredConsole],
    saved: &SavedConsole,
) -> Option<&'a DiscoveredConsole> {
    discovered.iter().find(|disc| {
        addr_matches_host(&saved.host, &disc.addr)
            || host_matches_remote_probe(saved, &disc.addr)
            || (!disc.host_id.is_empty() && saved.id.eq_ignore_ascii_case(disc.host_id.trim()))
            || (!disc.host_id.is_empty()
                && format_mac_id(saved.mac).eq_ignore_ascii_case(disc.host_id.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_saved() -> SavedConsole {
        SavedConsole {
            id: "001122334455".into(),
            name: "PS5-LivingRoom".into(),
            host: "192.168.1.50".into(),
            remote_host: Some("100.64.0.10".into()),
            mac: [0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            psn_account_id: "AQIDBAUGBwg=".into(),
            regist_key: *b"1a2b3c4d\0\0\0\0\0\0\0\0",
            rp_key: [0xAA; 16],
        }
    }

    #[test]
    fn upsert_preserves_remote_host_and_lookup_matches_lan_and_wan() {
        let mut saved = vec![sample_saved()];
        let re_registered = SavedConsole {
            remote_host: None,
            host: "192.168.1.51".into(),
            ..sample_saved()
        };
        upsert_saved_console(&mut saved, re_registered);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].host, "192.168.1.51");
        assert_eq!(saved[0].remote_host.as_deref(), Some("100.64.0.10"));

        let lan_disc = DiscoveredConsole {
            host_id: "other-id".into(),
            host_name: "PS5".into(),
            addr: "192.168.1.51".into(),
            state: ConsoleState::Ready,
            is_ps5: true,
            system_version: "10.00".into(),
            running_app_name: String::new(),
        };
        let wan_disc = DiscoveredConsole {
            addr: "100.64.0.10".into(),
            ..lan_disc.clone()
        };
        let list = [lan_disc, wan_disc];
        assert_eq!(
            find_discovered_for_saved(&list[..1], &saved[0]).map(|d| d.addr.as_str()),
            Some("192.168.1.51")
        );
        assert_eq!(
            find_discovered_for_saved(&list[1..], &saved[0]).map(|d| d.addr.as_str()),
            Some("100.64.0.10")
        );
        assert!(host_matches_remote_probe(&saved[0], "100.64.0.10"));
        assert!(!host_matches_remote_probe(&saved[0], "192.168.1.51"));
    }
}
