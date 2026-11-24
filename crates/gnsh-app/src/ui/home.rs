use super::settings::format_profile_summary;
use super::{
    HomeAction, RegisterModal, StatusBanner, find_discovered_for_saved,
    find_saved_index_for_discovered, format_mac_colon, theme,
};
use crate::AppState;
use crate::storage::SavedConsole;
use gnsh_proto::DiscoveredConsole;

const HOME_MAX_WIDTH: f32 = 960.0;

impl AppState {
    pub(crate) fn render_home_ui(&mut self, ui: &mut egui::Ui, gamepad_name: Option<&str>) {
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::CHROME)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                    .inner_margin(egui::Margin::symmetric(18, 10)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("gnsh-play")
                            .size(19.0)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );
                    ui.add_space(6.0);

                    match gamepad_name {
                        Some(name) => theme::status_pill(
                            ui,
                            theme::SUCCESS_BG,
                            theme::SUCCESS_FG,
                            &format!("Controller: {name}"),
                        ),
                        None => theme::status_pill(
                            ui,
                            theme::SURFACE_RAISED,
                            theme::TEXT_SECONDARY,
                            "No Controller",
                        ),
                    }
                    ui.add_space(6.0);
                    theme::status_pill(
                        ui,
                        theme::SURFACE_RAISED,
                        theme::TEXT_SECONDARY,
                        &format!("Renderer: {}", self.active_renderer_label),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary_button("+ Register PS5")).clicked() {
                            self.register_modal = Some(RegisterModal::new(String::new()));
                        }

                        let settings_btn = if self.settings_open {
                            theme::primary_button("Settings")
                        } else {
                            theme::secondary_button("Settings")
                        };
                        if ui.add(settings_btn).clicked() {
                            self.settings_open = !self.settings_open;
                        }

                        let scanning = self.discovery_rx.is_some();
                        if ui
                            .add_enabled(!scanning, theme::secondary_button("Discover LAN"))
                            .clicked()
                        {
                            self.status_banner = Some(StatusBanner::info(
                                "Scanning LAN & saved remote hosts for PS5 consoles...",
                            ));
                            self.start_discovery();
                        }
                        if scanning {
                            ui.spinner();
                        }
                    });
                });
            });

        if gamepad_name.is_some() {
            egui::Panel::bottom("gamepad_hints")
                .frame(
                    egui::Frame::new()
                        .fill(theme::CHROME)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                        .inner_margin(egui::Margin::symmetric(18, 8)),
                )
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            egui::RichText::new(
                                "D-Pad / Stick: Select  ·  Cross: Connect  ·  Triangle: Wake  ·  Square: Discover  ·  Options: Settings (D-Pad + L1/R1)  ·  Circle: Back",
                            )
                            .small()
                            .color(theme::TEXT_SECONDARY),
                        );
                    });
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let avail_w = ui.available_width();
                        let col_w = avail_w.min(HOME_MAX_WIDTH);
                        let side_pad = ((avail_w - col_w) * 0.5).max(0.0);

                        ui.horizontal(|ui| {
                            if side_pad > 0.0 {
                                ui.add_space(side_pad);
                            }
                            ui.vertical(|ui| {
                                ui.set_width(col_w);

                                if let Some(banner) = self.status_banner.clone() {
                                    if theme::render_status_banner(ui, &banner) {
                                        self.status_banner = None;
                                    }
                                    ui.add_space(10.0);
                                }

                                if let Some(ref rc) = self.reconnect {
                                    let now = std::time::Instant::now();
                                    theme::card_frame()
                                        .stroke(egui::Stroke::new(1.5, theme::FOCUS_RING))
                                        .show(ui, |ui| {
                                            ui.set_min_width(ui.available_width());
                                            ui.horizontal_wrapped(|ui| {
                                                ui.spinner();
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(rc.status_line(now))
                                                            .strong()
                                                            .color(theme::TEXT_PRIMARY),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "Last reason: {}",
                                                            rc.last_reason
                                                        ))
                                                        .small()
                                                        .color(theme::TEXT_MUTED),
                                                    );
                                                });
                                            });
                                            ui.add_space(8.0);
                                            ui.horizontal(|ui| {
                                                if ui
                                                    .add(theme::primary_button("Retry Now"))
                                                    .clicked()
                                                {
                                                    self.pending_home_action =
                                                        Some(HomeAction::RetryReconnectNow);
                                                }
                                                if ui
                                                    .add(theme::secondary_button("Cancel"))
                                                    .clicked()
                                                {
                                                    self.pending_home_action =
                                                        Some(HomeAction::CancelReconnect);
                                                }
                                            });
                                        });
                                    ui.add_space(10.0);
                                }

                                theme::nested_surface_frame().show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new("Active Profile:")
                                                .small()
                                                .color(theme::TEXT_MUTED),
                                        );
                                        ui.label(
                                            egui::RichText::new(format_profile_summary(
                                                &self.settings,
                                            ))
                                            .small()
                                            .family(egui::FontFamily::Monospace)
                                            .color(theme::TEXT_SECONDARY),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .add(theme::secondary_button("Configure..."))
                                                    .clicked()
                                                {
                                                    self.settings_open = true;
                                                }
                                            },
                                        );
                                    });
                                });

                                ui.add_space(16.0);
                                ui.horizontal(|ui| {
                                    ui.heading("Saved Consoles");
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} saved",
                                            self.saved_consoles.len()
                                        ))
                                        .small()
                                        .color(theme::TEXT_MUTED),
                                    )
                                    .on_hover_text(self.store.path().display().to_string());
                                });
                                ui.add_space(8.0);

                                if self.saved_consoles.is_empty() {
                                    if render_saved_empty_state(
                                        ui,
                                        self.discovery_rx.is_some(),
                                        &mut self.pending_home_action,
                                    ) {
                                        self.status_banner = Some(StatusBanner::info(
                                            "Scanning LAN & saved remote hosts for PS5 consoles...",
                                        ));
                                        self.start_discovery();
                                    }
                                } else {
                                    let waking = self.wake_rx.is_some();
                                    let gamepad_connected = gamepad_name.is_some();
                                    for (idx, saved) in self.saved_consoles.iter().enumerate() {
                                        let live_match = find_discovered_for_saved(
                                            &self.discovered_consoles,
                                            saved,
                                        );
                                        render_saved_console_card(
                                            ui,
                                            idx,
                                            saved,
                                            live_match,
                                            waking,
                                            gamepad_connected && idx == self.selected_console_idx,
                                            &mut self.pending_home_action,
                                        );
                                        ui.add_space(10.0);
                                    }
                                }

                                ui.add_space(16.0);
                                ui.horizontal(|ui| {
                                    ui.heading("Discovered on LAN");
                                    if self.discovery_rx.is_some() {
                                        ui.spinner();
                                        ui.label(
                                            egui::RichText::new("Scanning network...")
                                                .small()
                                                .color(theme::TEXT_MUTED),
                                        );
                                    }
                                });
                                ui.add_space(8.0);

                                if self.discovered_consoles.is_empty()
                                    && self.discovery_rx.is_none()
                                {
                                    theme::card_frame().show(ui, |ui| {
                                        ui.set_min_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new(
                                                "No PS5 consoles found on LAN or saved remote hosts. Verify Remote Play is enabled on your PS5 and click Discover LAN.",
                                            )
                                            .color(theme::TEXT_SECONDARY),
                                        );
                                    });
                                } else {
                                    for disc in &self.discovered_consoles {
                                        let saved_idx = find_saved_index_for_discovered(
                                            &self.saved_consoles,
                                            disc,
                                        );
                                        render_discovered_console_card(
                                            ui,
                                            disc,
                                            saved_idx,
                                            &mut self.pending_home_action,
                                        );
                                        ui.add_space(10.0);
                                    }
                                }
                            });
                        });
                    });
            });

        self.render_register_modal(ui.ctx());
        self.render_edit_console_modal(ui.ctx());
    }
}

fn render_saved_empty_state(
    ui: &mut egui::Ui,
    scanning: bool,
    pending_action: &mut Option<HomeAction>,
) -> bool {
    let mut discover_clicked = false;
    theme::card_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            egui::RichText::new("Link your PlayStation 5 to get started")
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Discover consoles on your local network or register directly using your PSN Account ID and the 8-digit PIN from PS5 Settings.",
            )
            .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.add(theme::primary_button("+ Register PS5")).clicked() {
                *pending_action = Some(HomeAction::OpenRegister(String::new()));
            }
            if ui
                .add_enabled(!scanning, theme::secondary_button("Discover LAN"))
                .clicked()
            {
                discover_clicked = true;
            }
        });
    });
    discover_clicked
}

fn render_saved_console_card(
    ui: &mut egui::Ui,
    idx: usize,
    saved: &SavedConsole,
    live: Option<&DiscoveredConsole>,
    waking: bool,
    is_selected: bool,
    pending_action: &mut Option<HomeAction>,
) {
    let mut frame = theme::card_frame();
    if is_selected {
        frame = frame.stroke(egui::Stroke::new(1.5, theme::FOCUS_RING));
    }
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(&saved.name)
                        .size(16.0)
                        .strong()
                        .color(theme::TEXT_PRIMARY),
                );
                if let Some(disc) = live {
                    theme::state_badge(ui, disc.state);
                    if !disc.running_app_name.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("Playing: {}", disc.running_app_name))
                                .small()
                                .color(theme::INFO_FG),
                        );
                    }
                }
            });
            ui.add_space(3.0);
            let subtitle = match saved.remote_host.as_deref() {
                Some(remote) => format!(
                    "LAN {}  ·  WAN {}  ·  ID {}  ·  MAC {}",
                    saved.host,
                    remote,
                    saved.id,
                    format_mac_colon(saved.mac)
                ),
                None => format!(
                    "Host {}  ·  ID {}  ·  MAC {}",
                    saved.host,
                    saved.id,
                    format_mac_colon(saved.mac)
                ),
            };
            ui.label(
                egui::RichText::new(subtitle)
                    .small()
                    .family(egui::FontFamily::Monospace)
                    .color(theme::TEXT_MUTED),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(theme::primary_button("Connect")).clicked() {
                    *pending_action = Some(HomeAction::ConnectSaved(idx));
                }
                if let Some(ref remote) = saved.remote_host
                    && ui
                        .add(theme::emerald_button("Connect WAN"))
                        .on_hover_text(format!("Connect over WAN/VPN host: {remote}"))
                        .clicked()
                {
                    *pending_action = Some(HomeAction::ConnectSavedWan(idx));
                }
                if ui
                    .add_enabled(!waking, theme::secondary_button("Wake"))
                    .clicked()
                {
                    *pending_action = Some(HomeAction::WakeSaved(idx));
                }
                if ui
                    .add(theme::secondary_button("Shortcut"))
                    .on_hover_text("Create Desktop shortcut for direct fullscreen launch")
                    .clicked()
                {
                    *pending_action = Some(HomeAction::CreateShortcut(idx));
                }
                if saved.remote_host.is_some()
                    && ui
                        .add(theme::secondary_button("Shortcut (WAN)"))
                        .on_hover_text(
                            "Create Desktop shortcut for direct fullscreen WAN launch (--wan)",
                        )
                        .clicked()
                {
                    *pending_action = Some(HomeAction::CreateShortcutWan(idx));
                }
                if ui.add(theme::secondary_button("Edit")).clicked() {
                    *pending_action = Some(HomeAction::EditSaved(idx));
                }
                if ui.add(theme::subtle_danger_button("Delete")).clicked() {
                    *pending_action = Some(HomeAction::DeleteSaved(idx));
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
    theme::card_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&disc.host_name)
                            .size(16.0)
                            .strong()
                            .color(theme::TEXT_PRIMARY),
                    );
                    theme::state_badge(ui, disc.state);
                    if saved_idx.is_some() {
                        theme::status_pill(ui, theme::INFO_BG, theme::INFO_FG, "Saved");
                    }
                });
                ui.add_space(3.0);

                let app_part = if disc.running_app_name.is_empty() {
                    String::new()
                } else {
                    format!("  ·  App {}", disc.running_app_name)
                };
                ui.label(
                    egui::RichText::new(format!(
                        "IP {}  ·  ID {}  ·  FW {}{}",
                        disc.addr, disc.host_id, disc.system_version, app_part
                    ))
                    .small()
                    .family(egui::FontFamily::Monospace)
                    .color(theme::TEXT_MUTED),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(idx) = saved_idx {
                    if ui.add(theme::primary_button("Connect")).clicked() {
                        *pending_action = Some(HomeAction::ConnectSaved(idx));
                    }
                } else if ui.add(theme::primary_button("Register")).clicked() {
                    *pending_action = Some(HomeAction::OpenRegister(disc.addr.clone()));
                }
            });
        });
    });
}
