use super::{RegisterModal, theme};
use crate::AppState;
use gnsh_input::{KbmAction, KbmBindings};
use gnsh_proto::{
    decode_account_id_b64, exchange_redirect_url_for_account_id, open_psn_login_in_browser,
    register_ps5,
};
use std::thread;
use std::time::Duration;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);

impl AppState {
    pub(crate) fn render_register_modal(&mut self, ctx: &egui::Context) {
        if self.register_modal.is_none() {
            return;
        }
        let mut keep_open = true;
        let resp = egui::Window::new("Register PS5")
            .open(&mut keep_open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                self.register_modal
                    .as_mut()
                    .is_some_and(|modal| render_register_modal_contents(ui, modal))
            });
        let cancel_clicked = resp.and_then(|inner| inner.inner).unwrap_or(false);
        if !keep_open || cancel_clicked {
            self.register_modal = None;
        }
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
                ui.label(egui::RichText::new("Console Name").color(theme::TEXT_SECONDARY));
                ui.add(
                    egui::TextEdit::singleline(&mut modal.name)
                        .desired_width(f32::INFINITY)
                        .hint_text("PS5-LivingRoom"),
                );
                ui.add_space(8.0);

                ui.label(egui::RichText::new("LAN Host / IP").color(theme::TEXT_SECONDARY));
                ui.add(
                    egui::TextEdit::singleline(&mut modal.host)
                        .desired_width(f32::INFINITY)
                        .hint_text("192.168.1.50"),
                );
                ui.add_space(8.0);

                ui.label(
                    egui::RichText::new("Remote / WAN / VPN Host (optional)")
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut modal.remote_host)
                        .desired_width(f32::INFINITY)
                        .hint_text("my-ps5.duckdns.org or 100.x.y.z (leave blank to disable)"),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "WAN setup: forward UDP 9302, TCP 9295, and UDP 9296-9297 to your PS5 on your router (or use Tailscale/WireGuard IP directly).",
                    )
                    .small()
                    .color(theme::TEXT_MUTED),
                );

                if let Some(ref err) = modal.error {
                    ui.add_space(8.0);
                    ui.colored_label(theme::DANGER_FG, err);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::primary_button("Save")).clicked() {
                        save_requested = true;
                    }
                    if ui.add(theme::secondary_button("Cancel")).clicked() {
                        cancel_requested = true;
                    }
                });
            });

        if !keep_open || cancel_requested {
            return;
        }

        if save_requested {
            let trimmed_name = modal.name.trim().to_string();
            if trimmed_name.is_empty() {
                modal.error = Some("Console name cannot be empty.".into());
                self.edit_modal = Some(modal);
                return;
            }
            let valid_host = match gnsh_proto::validate_console_host(&modal.host) {
                Ok(h) => h,
                Err(err) => {
                    modal.error = Some(format!("Invalid LAN Host / IP: {err}"));
                    self.edit_modal = Some(modal);
                    return;
                }
            };
            let remote_opt = if modal.remote_host.trim().is_empty() {
                None
            } else {
                match gnsh_proto::validate_console_host(&modal.remote_host) {
                    Ok(r) => Some(r),
                    Err(err) => {
                        modal.error = Some(format!("Invalid Remote / WAN / VPN Host: {err}"));
                        self.edit_modal = Some(modal);
                        return;
                    }
                }
            };
            let Some(console) = self.saved_consoles.get(modal.index).cloned() else {
                self.status_banner = Some(super::StatusBanner::error(
                    "Selected console no longer exists.",
                ));
                return;
            };
            let prev = console;
            if let Some(slot) = self.saved_consoles.get_mut(modal.index) {
                slot.name = trimmed_name;
                slot.host = valid_host;
                slot.remote_host = remote_opt;
            }
            match self.store.save(&self.saved_consoles) {
                Ok(()) => {
                    self.status_banner = Some(super::StatusBanner::success(format!(
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
                            "Listening for {}: press any key or mouse button (Esc to cancel)",
                            listening.label()
                        ))
                        .strong()
                        .color(theme::WARN_FG),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Click any action below to bind a key or mouse button (1-5). Duplicates swap automatically. Reserved: Esc, F3, F8, F11.",
                        )
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                }

                ui.add_space(10.0);
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
                                    let button = if is_listening {
                                        theme::primary_button(&btn_label)
                                    } else {
                                        theme::secondary_button(&btn_label)
                                    };
                                    if ui.add_sized([165.0, 26.0], button).clicked() {
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

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(theme::secondary_button("Reset to Defaults")).clicked() {
                        self.settings.kbm_bindings = KbmBindings::default();
                        self.kbm_listening_action = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary_button("Close")).clicked() {
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

fn render_register_modal_contents(ui: &mut egui::Ui, modal: &mut RegisterModal) -> bool {
    let mut request_close = false;
    let is_linking = modal.register_rx.is_some();
    let is_exchanging = modal.exchange_rx.is_some();

    ui.label(
        egui::RichText::new(
            "Enter your PS5 LAN/Remote host and authenticate with PSN (or paste your Base64 Account ID directly).",
        )
        .color(theme::TEXT_SECONDARY),
    );
    ui.add_space(10.0);

    ui.label(egui::RichText::new("Target PS5 Host / IP").color(theme::TEXT_SECONDARY));
    ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.host)
            .desired_width(f32::INFINITY)
            .hint_text("192.168.1.50 or my-ps5.duckdns.org"),
    );
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !is_linking && !is_exchanging,
                theme::secondary_button("Open PSN Login in Browser"),
            )
            .clicked()
        {
            match open_psn_login_in_browser() {
                Ok(()) => {
                    modal.error = None;
                    modal.notice = Some(
                        "Browser opened. Sign in to PSN, then paste the final redirect URL below."
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
            ui.label(
                egui::RichText::new("Exchanging URL...")
                    .small()
                    .color(theme::TEXT_MUTED),
            );
        }
    });
    ui.add_space(6.0);

    ui.label(egui::RichText::new("PSN Redirect URL").color(theme::TEXT_SECONDARY));
    ui.horizontal(|ui| {
        ui.add_enabled(
            !is_linking && !is_exchanging,
            egui::TextEdit::singleline(&mut modal.redirect_url)
                .desired_width(290.0)
                .hint_text("https://remoteplay.dl.playstation.net/remoteplay/redirect?code=..."),
        );
        if ui
            .add_enabled(
                !is_linking && !is_exchanging,
                theme::secondary_button("Exchange Account ID"),
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

    ui.label(egui::RichText::new("PSN Account ID (Base64, 8 bytes)").color(theme::TEXT_SECONDARY));
    ui.add_enabled(
        !is_linking,
        egui::TextEdit::singleline(&mut modal.psn_account_id)
            .desired_width(f32::INFINITY)
            .hint_text("Filled automatically by Exchange or pasted directly"),
    );
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new(
            "PS5 8-Digit Remote Play PIN (Settings -> System -> Remote Play -> Link Device)",
        )
        .color(theme::TEXT_SECONDARY),
    );
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
        ui.colored_label(theme::SUCCESS_FG, notice);
    }
    if let Some(err) = &modal.error {
        ui.add_space(8.0);
        ui.colored_label(theme::DANGER_FG, err);
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        if is_linking {
            ui.spinner();
            ui.label(
                egui::RichText::new("Linking with PS5 (30s timeout)...")
                    .small()
                    .color(theme::TEXT_SECONDARY),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let link_clicked = ui
                .add_enabled(!is_linking, theme::primary_button("Link PS5"))
                .clicked();
            if link_clicked || submit_on_enter {
                trigger_register_worker(modal);
            }
            if ui.add(theme::secondary_button("Cancel")).clicked() {
                request_close = true;
            }
        });
    });

    request_close
}

fn trigger_register_worker(modal: &mut RegisterModal) {
    let host = match gnsh_proto::validate_console_host(&modal.host) {
        Ok(h) => h,
        Err(err) => {
            modal.notice = None;
            modal.error = Some(format!("Invalid PS5 host / IP: {err}"));
            return;
        }
    };

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
