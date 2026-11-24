use super::settings::{
    render_audio_device_combo, render_filter_controls, render_kbm_sliders, render_scale_selector,
};
use super::{StreamSettings, theme};
use crate::stream::ActiveStream;
use gnsh_media::VideoFilterMode;
use gnsh_proto::VideoCodec;
use std::sync::atomic::Ordering;
use std::time::Instant;

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_streaming_ui(
    ui: &mut egui::Ui,
    stream: &mut ActiveStream,
    gamepad_name: Option<&str>,
    settings: &mut StreamSettings,
    audio_devices: &[String],
    mic_devices: &[String],
    hdr_output_active: bool,
    mouse_locked: bool,
    open_keybinds: &mut bool,
    open_settings: &mut bool,
) {
    if stream.show_hud {
        egui::Area::new(egui::Id::unique("stream_hud"))
            .anchor(egui::Align2::LEFT_TOP, [14.0, 14.0])
            .show(ui.ctx(), |ui| {
                theme::hud_frame().show(ui, |ui| {
                    render_hud_top_pill(
                        ui,
                        stream,
                        gamepad_name,
                        settings,
                        hdr_output_active,
                        mouse_locked,
                    );

                    if stream.hud_drawer_open {
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        render_hud_quick_drawer(
                            ui,
                            stream,
                            settings,
                            audio_devices,
                            mic_devices,
                            open_keybinds,
                            open_settings,
                        );
                    }
                });
            });
    }

    render_pin_modal(ui, stream);
    render_osk_modal(ui, stream);
}

fn render_hud_top_pill(
    ui: &mut egui::Ui,
    stream: &mut ActiveStream,
    gamepad_name: Option<&str>,
    settings: &StreamSettings,
    hdr_output_active: bool,
    mouse_locked: bool,
) {
    let (decode_ms_text, decoder_badge) = match &stream.current_frame {
        Some(info) => (
            format!("{:.2} ms", info.decode_ms),
            if info.is_hw { "D3D11VA" } else { "SW" },
        ),
        None => ("-- ms".to_string(), "Pending"),
    };
    let buf_frames = stream.decoder_lag_frames.load(Ordering::Relaxed);
    let pipe_short = if stream.pipe_p50_ms > 0.0 {
        format!("{:.1} ms", stream.pipe_p50_ms)
    } else {
        "-- ms".to_string()
    };
    let pipe_full = if stream.pipe_p50_ms > 0.0 {
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
            " · HDR10"
        } else {
            " · HDR->SDR"
        }
    } else {
        ""
    };
    let filter_badge = match settings.filter_mode {
        VideoFilterMode::Bilinear => String::new(),
        VideoFilterMode::Cas if settings.sharpness_pct > 0 => {
            format!(" · CAS {}%", settings.sharpness_pct)
        }
        VideoFilterMode::Cas => String::new(),
        VideoFilterMode::Fsr1 if settings.sharpness_pct > 0 => {
            format!(" · FSR 1.0 RCAS {}%", settings.sharpness_pct)
        }
        VideoFilterMode::Fsr1 => " · FSR 1.0".to_string(),
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(&stream.console_name)
                .strong()
                .color(theme::TEXT_PRIMARY),
        );

        if stream.connected {
            theme::status_pill(ui, theme::SUCCESS_BG, theme::SUCCESS_FG, "Live");
        } else {
            ui.spinner();
            theme::status_pill(
                ui,
                theme::WARN_BG,
                theme::WARN_FG,
                &format!("Connecting ({})...", stream.console_host),
            );
        }

        let summary = format!(
            "{:.1} FPS · {:.1} Mbps · RTT {:.1} ms · Loss {:.1}% · Pipe {} [{}{}{}]",
            stream.fps,
            stream.measured_mbps,
            stream.rtt_ms,
            stream.packet_loss_pct,
            pipe_short,
            decoder_badge,
            hdr_badge,
            filter_badge
        );
        let mut hover = format!(
            "Pipe: {pipe_full}\nDecode: {decode_ms_text} ({decoder_badge})\nUDP Buf: {udp_buf_text}\nGamepad: {pad_label}\nHaptics: {haptics_label}"
        );
        if let Ok(guard) = stream.haptics_device_label.lock()
            && let Some(ref dev_name) = *guard
        {
            hover.push_str(&format!("\nUSB Haptics: {dev_name}"));
        }
        ui.label(
            egui::RichText::new(summary)
                .small()
                .family(egui::FontFamily::Monospace)
                .color(theme::TEXT_SECONDARY),
        )
        .on_hover_text(hover);

        if lost_total > 0 {
            let recent_loss = stream
                .packet_loss_notice_until
                .is_some_and(|until| Instant::now() < until);
            let (bg, fg) = if recent_loss {
                (theme::WARN_BG, theme::WARN_FG)
            } else {
                (theme::SURFACE_RAISED, theme::TEXT_MUTED)
            };
            theme::status_pill(ui, bg, fg, &format!("Lost {lost_total}"));
        }

        ui.separator();

        let (mic_label, mic_bg, mic_fg, mic_border) = if stream.mic_muted {
            (
                "Mic Muted",
                theme::SURFACE_RAISED,
                theme::TEXT_SECONDARY,
                Some(theme::BORDER_SUBTLE),
            )
        } else {
            (
                "Mic LIVE",
                theme::SUCCESS_BG,
                theme::SUCCESS_FG,
                Some(theme::SUCCESS_BORDER),
            )
        };
        if ui
            .add_enabled(
                stream.connected,
                theme::small_pill_button(mic_label, mic_bg, mic_fg, mic_border),
            )
            .on_hover_text(
                stream
                    .mic_error
                    .as_deref()
                    .unwrap_or("Toggle microphone uplink (Ctrl+M or DualSense Mute)"),
            )
            .clicked()
        {
            stream.mic_toggle_requested = true;
        }

        if ui
            .add_enabled(
                stream.connected,
                theme::small_pill_button(
                    "PS Home",
                    theme::SURFACE_RAISED,
                    theme::TEXT_PRIMARY,
                    Some(theme::BORDER_SUBTLE),
                ),
            )
            .on_hover_text("Open PS5 Home screen (Home key)")
            .clicked()
        {
            stream.go_home_requested = true;
        }

        if settings.kbm_enabled {
            let (lock_label, lock_bg, lock_fg) = if mouse_locked {
                ("Mouse Locked", theme::SUCCESS_BG, theme::SUCCESS_FG)
            } else {
                ("Lock Mouse (F8)", theme::ACCENT, egui::Color32::WHITE)
            };
            if ui
                .add(theme::small_pill_button(lock_label, lock_bg, lock_fg, None))
                .on_hover_text("Toggle relative mouse camera capture (F8 / Esc)")
                .clicked()
            {
                stream.mouse_lock_toggle_requested = true;
            }
        }

        let (tune_bg, tune_fg, tune_border) = if stream.hud_drawer_open {
            (theme::ACCENT, egui::Color32::WHITE, None)
        } else {
            (
                theme::SURFACE_RAISED,
                theme::TEXT_PRIMARY,
                Some(theme::BORDER_SUBTLE),
            )
        };
        if ui
            .add(theme::small_pill_button(
                "Quick Settings",
                tune_bg,
                tune_fg,
                tune_border,
            ))
            .on_hover_text("Expand or collapse live stream & audio controls")
            .clicked()
        {
            stream.hud_drawer_open = !stream.hud_drawer_open;
        }

        if ui
            .add(theme::small_pill_button(
                "Disconnect",
                theme::DANGER_SOLID_BG,
                egui::Color32::WHITE,
                None,
            ))
            .clicked()
        {
            stream.disconnect_requested = true;
        }
    });
}

fn render_hud_quick_drawer(
    ui: &mut egui::Ui,
    stream: &mut ActiveStream,
    settings: &mut StreamSettings,
    audio_devices: &[String],
    mic_devices: &[String],
    open_keybinds: &mut bool,
    open_settings: &mut bool,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("Scale:")
                .small()
                .color(theme::TEXT_MUTED),
        );
        render_scale_selector(ui, &mut settings.scale_mode);

        ui.separator();
        ui.label(
            egui::RichText::new("Filter:")
                .small()
                .color(theme::TEXT_MUTED),
        );
        render_filter_controls(ui, &mut settings.filter_mode, &mut settings.sharpness_pct);
    });

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = 76.0;
            ui.add(
                egui::Slider::new(&mut settings.volume_pct, 0..=100)
                    .clamping(egui::SliderClamping::Always)
                    .suffix("%")
                    .text("Vol"),
            )
            .on_hover_text("Stream playback volume (0-100%)");
        });

        ui.separator();
        ui.label(
            egui::RichText::new("Audio:")
                .small()
                .color(theme::TEXT_MUTED),
        );
        render_audio_device_combo(
            ui,
            "hud_audio_device",
            audio_devices,
            &mut settings.audio_device,
            160.0,
        );

        ui.separator();
        ui.label(egui::RichText::new("Mic:").small().color(theme::TEXT_MUTED));
        render_audio_device_combo(
            ui,
            "hud_mic_device",
            mic_devices,
            &mut settings.mic_device,
            160.0,
        );

        ui.separator();
        ui.checkbox(&mut settings.vsync, "VSync").on_hover_text(
            "Off: lowest latency (decode-paced + tearing, 1000 Hz input). On: tear-free VSync.",
        );
        ui.checkbox(&mut settings.touchpad_touch, "Touchpad Touch")
            .on_hover_text("Off: click-only. On: capacitive touch/swipe.");
    });

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut settings.kbm_enabled, "KB+M")
            .on_hover_text(settings.kbm_bindings.summary_tooltip());
        if settings.kbm_enabled {
            render_kbm_sliders(ui, settings);
        }
        if ui.add(theme::secondary_button("Keybinds...")).clicked() {
            *open_keybinds = true;
        }
        if ui.add(theme::secondary_button("All Settings...")).clicked() {
            stream.hud_drawer_open = false;
            *open_settings = true;
        }
    });

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "F3: Toggle HUD  ·  F8: Lock Mouse  ·  F11: Fullscreen  ·  Home: PS Home  ·  Ctrl+M: Mic  ·  Esc: Close / Disconnect",
        )
        .small()
        .color(theme::TEXT_MUTED),
    );
}

fn render_pin_modal(ui: &egui::Ui, stream: &mut ActiveStream) {
    let Some(prompt) = &mut stream.pin_prompt else {
        return;
    };
    let mut submit_pin: Option<String> = None;
    let mut cancel_prompt = false;

    egui::Window::new("Console Login PIN")
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
        .default_width(340.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                egui::RichText::new("Your PS5 requires a user login PIN to unlock this session.")
                    .color(theme::TEXT_SECONDARY),
            );
            if prompt.incorrect {
                ui.add_space(4.0);
                ui.colored_label(theme::DANGER_FG, "Incorrect PIN. Please try again.");
            }
            ui.add_space(8.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut prompt.pin_input)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .hint_text("Enter PS5 passcode"),
            );
            let enter_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if (ui.add(theme::primary_button("Submit PIN")).clicked() || enter_pressed)
                    && !prompt.pin_input.trim().is_empty()
                {
                    submit_pin = Some(prompt.pin_input.trim().to_string());
                }
                if ui.add(theme::secondary_button("Disconnect")).clicked() {
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
            ui.label(
                egui::RichText::new(
                    "Type UTF-8 text to forward directly into the active PS5 input field:",
                )
                .color(theme::TEXT_SECONDARY),
            );
            ui.add_space(8.0);
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

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(theme::primary_button("Send (Enter)")).clicked() || enter_pressed {
                    submit = true;
                }
                if ui.add(theme::secondary_button("Cancel (Esc)")).clicked() {
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
