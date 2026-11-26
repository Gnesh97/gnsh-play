use super::{SettingsTab, StreamSettings, theme};
use crate::{AppMode, AppState, StatusBanner};
use gnsh_media::{RendererBackend, ScaleMode, VideoFilterMode};
use gnsh_proto::{MAX_BITRATE_KBPS, MIN_BITRATE_KBPS, VideoCodec, VideoPreset};

pub(crate) fn render_preset_controls(ui: &mut egui::Ui, preset: &mut VideoPreset) {
    let mut is_1080p = matches!(preset, VideoPreset::P1080Fps30 | VideoPreset::P1080Fps60);
    let mut is_60fps = matches!(preset, VideoPreset::P720Fps60 | VideoPreset::P1080Fps60);

    ui.horizontal(|ui| {
        ui.selectable_value(&mut is_1080p, true, "1080p");
        ui.selectable_value(&mut is_1080p, false, "720p");
        ui.separator();
        ui.selectable_value(&mut is_60fps, true, "60 FPS");
        ui.selectable_value(&mut is_60fps, false, "30 FPS");
    });

    *preset = match (is_1080p, is_60fps) {
        (true, true) => VideoPreset::P1080Fps60,
        (true, false) => VideoPreset::P1080Fps30,
        (false, true) => VideoPreset::P720Fps60,
        (false, false) => VideoPreset::P720Fps30,
    };
}

pub(crate) fn render_codec_controls(ui: &mut egui::Ui, codec: &mut VideoCodec) {
    ui.horizontal(|ui| {
        ui.selectable_value(codec, VideoCodec::Hevc, "HEVC");
        ui.selectable_value(codec, VideoCodec::HevcHdr, "HEVC HDR10")
            .on_hover_text(
                "10-bit HEVC HDR10: native passthrough on Windows HDR displays, ACES tone-mapping on SDR monitors.",
            );
        ui.selectable_value(codec, VideoCodec::H264, "H.264");
    });
}

pub(crate) fn render_scale_selector(ui: &mut egui::Ui, scale_mode: &mut ScaleMode) {
    ui.horizontal(|ui| {
        ui.selectable_value(scale_mode, ScaleMode::Fit, "Fit")
            .on_hover_text("Preserve 16:9 aspect ratio (letterbox/pillarbox black bars)");
        ui.selectable_value(scale_mode, ScaleMode::Zoom, "Zoom")
            .on_hover_text(
                "Fill screen preserving aspect ratio by cropping outer edges (16:10 / 21:9)",
            );
        ui.selectable_value(scale_mode, ScaleMode::Stretch, "Stretch")
            .on_hover_text("Stretch frame to fill entire window without cropping");
    });
}

pub(crate) fn render_filter_mode_selector(ui: &mut egui::Ui, filter_mode: &mut VideoFilterMode) {
    ui.horizontal(|ui| {
        ui.selectable_value(filter_mode, VideoFilterMode::Bilinear, "Bilinear")
            .on_hover_text("Fast bilinear scaling with no sharpening filter");
        ui.selectable_value(filter_mode, VideoFilterMode::Cas, "CAS")
            .on_hover_text("Single-pass HLSL Contrast Adaptive Sharpening");
        ui.selectable_value(filter_mode, VideoFilterMode::Fsr1, "FSR 1.0")
            .on_hover_text(
                "AMD FidelityFX Super Resolution 1.0: 12-tap EASU edge-adaptive upscaling + RCAS sharpening",
            );
    });
}

pub(crate) fn render_renderer_backend_selector(ui: &mut egui::Ui, backend: &mut RendererBackend) {
    ui.horizontal(|ui| {
        ui.selectable_value(backend, RendererBackend::Auto, "Auto")
            .on_hover_text("Default Direct3D 11 renderer with zero-copy D3D11VA decode");
        ui.selectable_value(backend, RendererBackend::D3D11, "Direct3D 11")
            .on_hover_text("Direct3D 11 swapchain with HLSL CAS / FSR 1.0");
        ui.selectable_value(backend, RendererBackend::Vulkan, "Vulkan")
            .on_hover_text(
                "libplacebo Vulkan renderer with zero-copy Vulkan Video decode (Bilinear / Spline36 + live CAS sharpening). Restart required.",
            );
        ui.selectable_value(backend, RendererBackend::D3D12, "D3D12")
            .on_hover_text(
                "Native Direct3D 12 renderer with zero-copy D3D12VA decode + HLSL CAS / FSR 1.0. Restart required.",
            );
    });
}

pub(crate) fn render_sharpness_slider(
    ui: &mut egui::Ui,
    filter_mode: VideoFilterMode,
    sharpness_pct: &mut u8,
) {
    let (slider_label, enabled, tooltip) = match filter_mode {
        VideoFilterMode::Bilinear => (
            "Sharp",
            false,
            "Switch Filter to CAS or FSR 1.0 to adjust sharpening",
        ),
        VideoFilterMode::Cas => (
            "CAS",
            true,
            "Contrast Adaptive Sharpening intensity (0-100%)",
        ),
        VideoFilterMode::Fsr1 => (
            "RCAS",
            true,
            "FSR 1.0 RCAS sharpening intensity after EASU upscaling (0% = EASU only)",
        ),
    };
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 90.0;
        ui.add_enabled(
            enabled,
            egui::Slider::new(sharpness_pct, 0..=100)
                .clamping(egui::SliderClamping::Always)
                .suffix("%")
                .text(slider_label),
        )
        .on_hover_text(tooltip)
        .on_disabled_hover_text(tooltip);
    });
}

pub(crate) fn render_audio_device_combo(
    ui: &mut egui::Ui,
    id_salt: &str,
    devices: &[String],
    selected: &mut Option<String>,
    width: f32,
) {
    let selected_label = match selected.as_deref() {
        None => "System Default".to_string(),
        Some(name) if devices.iter().any(|d| d == name) => name.to_string(),
        Some(name) => format!("{name} (unavailable)"),
    };
    egui::ComboBox::from_id_salt(id_salt)
        .width(width)
        .selected_text(selected_label)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, "System Default");
            for dev in devices {
                ui.selectable_value(selected, Some(dev.clone()), dev);
            }
        });
}

pub(crate) fn render_kbm_sliders(ui: &mut egui::Ui, settings: &mut StreamSettings) {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = 80.0;
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
            "Aim response curve: <100% boosts low-mid speed; >100% increases micro-aim precision",
        );
    });
    ui.checkbox(&mut settings.kbm_invert_y, "Inv Y")
        .on_hover_text("Invert vertical mouse camera axis");
}

fn preset_label(preset: VideoPreset) -> (&'static str, u8) {
    match preset {
        VideoPreset::P720Fps30 => ("720p", 30),
        VideoPreset::P720Fps60 => ("720p", 60),
        VideoPreset::P1080Fps30 => ("1080p", 30),
        VideoPreset::P1080Fps60 => ("1080p", 60),
    }
}

pub(crate) fn format_profile_summary(settings: &StreamSettings) -> String {
    let (res, fps) = preset_label(settings.preset);
    let codec = match settings.codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::HevcHdr => "HEVC HDR10",
    };
    let mbps = settings.bitrate_kbps / 1_000;
    let filter = match settings.filter_mode {
        VideoFilterMode::Bilinear => "Bilinear".to_string(),
        VideoFilterMode::Cas => {
            if settings.sharpness_pct > 0 {
                format!("CAS {}%", settings.sharpness_pct)
            } else {
                "CAS Off".to_string()
            }
        }
        VideoFilterMode::Fsr1 => {
            if settings.sharpness_pct > 0 {
                format!("FSR 1.0 (RCAS {}%)", settings.sharpness_pct)
            } else {
                "FSR 1.0".to_string()
            }
        }
    };
    let vsync = if settings.vsync {
        "VSync On"
    } else {
        "Low-Latency No-VSync"
    };
    let kbm = if settings.kbm_enabled { " · KB+M" } else { "" };
    let wan = if settings.wan_profile_enabled {
        let (wan_res, wan_fps) = preset_label(settings.wan_preset);
        format!(
            " · WAN {wan_res}{wan_fps} {} Mbps",
            settings.wan_bitrate_kbps / 1_000
        )
    } else {
        String::new()
    };
    format!("{res}{fps} · {codec} · {mbps} Mbps · {filter} · {vsync}{kbm}{wan}")
}

fn settings_row_label(ui: &mut egui::Ui, label: &str, focus_row: Option<usize>, row_idx: usize) {
    if focus_row == Some(row_idx) {
        ui.label(
            egui::RichText::new(format!("> {label}"))
                .strong()
                .color(theme::FOCUS_RING),
        );
    } else {
        ui.label(egui::RichText::new(label).color(theme::TEXT_SECONDARY));
    }
}

impl AppState {
    pub(crate) fn render_settings_modal(&mut self, ctx: &egui::Context, gamepad_connected: bool) {
        if !self.settings_open {
            return;
        }
        let row_count = crate::gamepad_nav::settings_row_count(
            self.settings_tab,
            self.settings.kbm_enabled,
            self.settings.wan_profile_enabled,
        );
        if row_count > 0 && self.settings_focus_row >= row_count {
            self.settings_focus_row = row_count - 1;
        }
        let focus_row = gamepad_connected.then_some(self.settings_focus_row);
        let mut open = self.settings_open;
        let mut close_clicked = false;
        let is_streaming = matches!(self.mode, AppMode::Streaming(_));

        egui::Window::new("Settings")
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .collapsible(false)
            .resizable(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SettingsTab::Video, "Video"),
                        (SettingsTab::Audio, "Audio"),
                        (SettingsTab::Input, "Input"),
                    ] {
                        let active = self.settings_tab == tab;
                        let btn = if active {
                            theme::primary_button(label)
                        } else {
                            theme::secondary_button(label)
                        };
                        if ui.add_sized([92.0, 30.0], btn).clicked() {
                            self.settings_tab = tab;
                            self.settings_focus_row = 0;
                        }
                    }
                });

                if is_streaming {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Resolution, FPS, Codec, and Bitrate apply on next connection; all other controls update live.",
                        )
                        .small()
                        .color(theme::INFO_FG),
                    );
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| match self.settings_tab {
                        SettingsTab::Video => {
                            self.render_settings_video_tab(ui, is_streaming, focus_row)
                        }
                        SettingsTab::Audio => self.render_settings_audio_tab(ui, focus_row),
                        SettingsTab::Input => self.render_settings_input_tab(ui, focus_row),
                    });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if gamepad_connected {
                        ui.label(
                            egui::RichText::new(
                                "D-Pad: Row  ·  Left/Right: Adjust  ·  L1/R1: Tab  ·  Cross: Toggle  ·  Circle: Close",
                            )
                            .small()
                            .color(theme::TEXT_MUTED),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(theme::primary_button("Close")).clicked() {
                            close_clicked = true;
                        }
                    });
                });
            });

        if !open || close_clicked {
            self.settings_open = false;
        }
    }

    fn render_settings_video_tab(
        &mut self,
        ui: &mut egui::Ui,
        is_streaming: bool,
        focus_row: Option<usize>,
    ) {
        egui::Grid::new("settings_video_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                settings_row_label(ui, "Stream Preset", focus_row, 0);
                ui.add_enabled_ui(!is_streaming, |ui| {
                    render_preset_controls(ui, &mut self.settings.preset);
                });
                ui.end_row();

                settings_row_label(ui, "Video Codec", focus_row, 1);
                ui.add_enabled_ui(!is_streaming, |ui| {
                    render_codec_controls(ui, &mut self.settings.codec);
                });
                ui.end_row();

                settings_row_label(ui, "Target Bitrate", focus_row, 2);
                ui.add_enabled_ui(!is_streaming, |ui| {
                    ui.add(
                        egui::Slider::new(
                            &mut self.settings.bitrate_kbps,
                            MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS,
                        )
                        .step_by(1_000.0)
                        .clamping(egui::SliderClamping::Always)
                        .suffix(" kbps"),
                    )
                    .on_hover_text("PS5 Remote Play FEC limit: 2,000-100,000 kbps");
                });
                ui.end_row();

                settings_row_label(ui, "Aspect Scale", focus_row, 3);
                render_scale_selector(ui, &mut self.settings.scale_mode);
                ui.end_row();

                settings_row_label(ui, "Upscale Filter", focus_row, 4);
                render_filter_mode_selector(ui, &mut self.settings.filter_mode);
                ui.end_row();

                settings_row_label(ui, "Sharpness", focus_row, 5);
                render_sharpness_slider(ui, self.settings.filter_mode, &mut self.settings.sharpness_pct);
                ui.end_row();

                settings_row_label(ui, "Deband", focus_row, 6);
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut self.settings.deband,
                        "Reduce 8-bit compression banding",
                    )
                    .on_hover_text("Applies a light GPU deband pass before sharpening to clean dark/sky macroblock banding.");
                    if ui
                        .add_enabled(
                            self.settings.has_custom_image_adjustments(),
                            theme::secondary_button("Reset Image"),
                        )
                        .clicked()
                    {
                        self.settings.reset_image_adjustments();
                    }
                });
                ui.end_row();

                settings_row_label(ui, "Vibrance", focus_row, 7);
                ui.add(
                    egui::Slider::new(&mut self.settings.vibrance_pct, -50..=100)
                        .clamping(egui::SliderClamping::Always)
                        .suffix("%"),
                )
                .on_hover_text("Smart color saturation boost without oversaturating already-vivid hues (-50% to +100%)");
                ui.end_row();

                settings_row_label(ui, "Contrast", focus_row, 8);
                ui.add(
                    egui::Slider::new(&mut self.settings.contrast_pct, 50..=150)
                        .clamping(egui::SliderClamping::Always)
                        .suffix("%"),
                )
                .on_hover_text("Midtone contrast multiplier (50-150%, default 100%)");
                ui.end_row();

                settings_row_label(ui, "Gamma", focus_row, 9);
                ui.add(
                    egui::Slider::new(&mut self.settings.gamma_pct, 70..=140)
                        .clamping(egui::SliderClamping::Always)
                        .custom_formatter(|v, _| format!("{:.2}", v / 100.0)),
                )
                .on_hover_text("Display gamma curve adjustment (0.70-1.40, default 1.00)");
                ui.end_row();

                settings_row_label(ui, "HDR->SDR Exposure", focus_row, 10);
                ui.add(
                    egui::Slider::new(&mut self.settings.hdr_sdr_exposure_pct, 50..=200)
                        .clamping(egui::SliderClamping::Always)
                        .suffix("%"),
                )
                .on_hover_text("Brightness multiplier when tone-mapping 10-bit HEVC HDR10 streams onto SDR monitors (50-200%)");
                ui.end_row();

                settings_row_label(ui, "Presentation", focus_row, 11);
                ui.checkbox(
                    &mut self.settings.vsync,
                    "VSync (tear-free, 1-frame DXGI wait)",
                )
                .on_hover_text(
                    "Off enables decode-paced tearing + 1000 Hz input polling for lowest latency.",
                );
                ui.end_row();

                settings_row_label(ui, "Auto-Reconnect", focus_row, 12);
                ui.checkbox(
                    &mut self.settings.auto_reconnect,
                    "Retry drops (2s/4s/8s) with LAN -> WAN fallback",
                )
                .on_hover_text(
                    "Automatically reconnect transient stream drops and switch to saved WAN host if LAN retries fail.",
                );
                ui.end_row();

                settings_row_label(ui, "Renderer", focus_row, 13);
                ui.horizontal(|ui| {
                    render_renderer_backend_selector(ui, &mut self.settings.renderer_backend);
                    ui.label(
                        egui::RichText::new("Restart required")
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                });
                ui.end_row();

                settings_row_label(ui, "Replay Buffer", focus_row, 14);
                ui.horizontal(|ui| {
                    ui.checkbox(
                        &mut self.settings.replay_buffer_enabled,
                        "Keep rolling clip buffer",
                    )
                    .on_hover_text("Records a bounded rolling buffer in RAM so F10 saves the last 15-60s with zero re-encoding.");
                    for secs in [15u16, 30, 60] {
                        ui.selectable_value(
                            &mut self.settings.replay_buffer_secs,
                            secs,
                            format!("{secs}s"),
                        );
                    }
                    ui.label(
                        egui::RichText::new("(F10 saves clip)")
                            .small()
                            .color(theme::TEXT_MUTED),
                    );
                });
                ui.end_row();

                settings_row_label(ui, "Capture Folder", focus_row, 15);
                ui.horizontal(|ui| {
                    let mut custom_dir = self.settings.capture_dir.clone().unwrap_or_default();
                    let edit = egui::TextEdit::singleline(&mut custom_dir)
                        .hint_text("Default (Videos\\gnsh-play & Pictures\\gnsh-play)")
                        .desired_width(260.0);
                    if ui.add(edit).changed() {
                        let trimmed = custom_dir.trim();
                        self.settings.capture_dir = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        };
                    }
                    if ui.add(theme::secondary_button("Open")).clicked()
                        && let Err(err) =
                            crate::open_capture_folder(self.settings.capture_dir.as_deref())
                    {
                        self.status_banner = Some(StatusBanner::error(format!(
                            "Failed opening capture folder: {err:#}"
                        )));
                    }
                });
                ui.end_row();

                settings_row_label(ui, "WAN Profile", focus_row, 16);
                ui.checkbox(
                    &mut self.settings.wan_profile_enabled,
                    "Use separate stream profile for WAN / VPN connections",
                )
                .on_hover_text(
                    "Overrides Resolution, FPS, Codec, and Bitrate when streaming via Connect WAN or WAN fallback.",
                );
                ui.end_row();

                if self.settings.wan_profile_enabled {
                    settings_row_label(ui, "WAN Preset", focus_row, 17);
                    ui.add_enabled_ui(!is_streaming, |ui| {
                        render_preset_controls(ui, &mut self.settings.wan_preset);
                    });
                    ui.end_row();

                    settings_row_label(ui, "WAN Codec", focus_row, 18);
                    ui.add_enabled_ui(!is_streaming, |ui| {
                        render_codec_controls(ui, &mut self.settings.wan_codec);
                    });
                    ui.end_row();

                    settings_row_label(ui, "WAN Bitrate", focus_row, 19);
                    ui.add_enabled_ui(!is_streaming, |ui| {
                        ui.add(
                            egui::Slider::new(
                                &mut self.settings.wan_bitrate_kbps,
                                MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS,
                            )
                            .step_by(1_000.0)
                            .clamping(egui::SliderClamping::Always)
                            .suffix(" kbps"),
                        )
                        .on_hover_text("Target bitrate used for WAN / VPN streams (2,000-100,000 kbps)");
                    });
                    ui.end_row();
                }
            });
    }

    fn render_settings_audio_tab(&mut self, ui: &mut egui::Ui, focus_row: Option<usize>) {
        egui::Grid::new("settings_audio_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                settings_row_label(ui, "Master Volume", focus_row, 0);
                ui.add(
                    egui::Slider::new(&mut self.settings.volume_pct, 0..=100)
                        .clamping(egui::SliderClamping::Always)
                        .suffix("%"),
                )
                .on_hover_text("Stream playback gain (0-100%)");
                ui.end_row();

                settings_row_label(ui, "Playback Device", focus_row, 1);
                render_audio_device_combo(
                    ui,
                    "settings_audio_device",
                    &self.audio_devices,
                    &mut self.settings.audio_device,
                    280.0,
                );
                ui.end_row();

                settings_row_label(ui, "Microphone Input", focus_row, 2);
                render_audio_device_combo(
                    ui,
                    "settings_mic_device",
                    &self.mic_devices,
                    &mut self.settings.mic_device,
                    280.0,
                );
                ui.end_row();
            });
    }

    fn render_settings_input_tab(&mut self, ui: &mut egui::Ui, focus_row: Option<usize>) {
        egui::Grid::new("settings_input_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                settings_row_label(ui, "DualSense Touchpad", focus_row, 0);
                ui.checkbox(
                    &mut self.settings.touchpad_touch,
                    "Forward capacitive finger touch",
                )
                .on_hover_text(
                    "Off: click-only (prevents unintended clicks in games). On: capacitive swipe/touch.",
                );
                ui.end_row();

                settings_row_label(ui, "Virtual KB+M", focus_row, 1);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.settings.kbm_enabled, "Enable Mouse & Keyboard (F8 lock)")
                        .on_hover_text(self.settings.kbm_bindings.summary_tooltip());
                    if ui.add(theme::secondary_button("Keybinds...")).clicked() {
                        self.kbm_modal_open = true;
                    }
                });
                ui.end_row();

                if self.settings.kbm_enabled {
                    settings_row_label(ui, "Sensitivity", focus_row, 2);
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_sensitivity_pct, 1..=100)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    settings_row_label(ui, "Inner Deadzone", focus_row, 3);
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_deadzone_pct, 0..=40)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    settings_row_label(ui, "Aim Curve", focus_row, 4);
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_curve_pct, 50..=150)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    settings_row_label(ui, "Vertical Axis", focus_row, 5);
                    ui.checkbox(&mut self.settings.kbm_invert_y, "Invert Mouse Y");
                    ui.end_row();
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_active_profile_summary_cleanly() {
        let mut s = StreamSettings::default();
        assert_eq!(
            format_profile_summary(&s),
            "1080p60 · HEVC · 30 Mbps · Bilinear · VSync On"
        );
        s.filter_mode = VideoFilterMode::Fsr1;
        s.sharpness_pct = 45;
        s.vsync = false;
        s.kbm_enabled = true;
        assert_eq!(
            format_profile_summary(&s),
            "1080p60 · HEVC · 30 Mbps · FSR 1.0 (RCAS 45%) · Low-Latency No-VSync · KB+M"
        );
        s.wan_profile_enabled = true;
        assert_eq!(
            format_profile_summary(&s),
            "1080p60 · HEVC · 30 Mbps · FSR 1.0 (RCAS 45%) · Low-Latency No-VSync · KB+M · WAN 720p60 12 Mbps"
        );
    }
}
