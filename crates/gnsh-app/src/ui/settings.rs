use super::{SettingsTab, StreamSettings, theme};
use crate::{AppMode, AppState};
use gnsh_media::{ScaleMode, VideoFilterMode};
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

pub(crate) fn render_filter_controls(
    ui: &mut egui::Ui,
    filter_mode: &mut VideoFilterMode,
    sharpness_pct: &mut u8,
) {
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(filter_mode, VideoFilterMode::Bilinear, "Bilinear")
            .on_hover_text("Fast bilinear scaling with no sharpening filter");
        ui.selectable_value(filter_mode, VideoFilterMode::Cas, "CAS")
            .on_hover_text("Single-pass HLSL Contrast Adaptive Sharpening");
        ui.selectable_value(filter_mode, VideoFilterMode::Fsr1, "FSR 1.0")
            .on_hover_text(
                "AMD FidelityFX Super Resolution 1.0: 12-tap EASU edge-adaptive upscaling + RCAS sharpening",
            );

        let (slider_label, enabled, tooltip) = match *filter_mode {
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

pub(crate) fn format_profile_summary(settings: &StreamSettings) -> String {
    let (res, fps) = match settings.preset {
        VideoPreset::P720Fps30 => ("720p", 30),
        VideoPreset::P720Fps60 => ("720p", 60),
        VideoPreset::P1080Fps30 => ("1080p", 30),
        VideoPreset::P1080Fps60 => ("1080p", 60),
    };
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
    format!("{res}{fps} · {codec} · {mbps} Mbps · {filter} · {vsync}{kbm}")
}

impl AppState {
    pub(crate) fn render_settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
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

                match self.settings_tab {
                    SettingsTab::Video => self.render_settings_video_tab(ui, is_streaming),
                    SettingsTab::Audio => self.render_settings_audio_tab(ui),
                    SettingsTab::Input => self.render_settings_input_tab(ui),
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(theme::primary_button("Close")).clicked() {
                        close_clicked = true;
                    }
                });
            });

        if !open || close_clicked {
            self.settings_open = false;
        }
    }

    fn render_settings_video_tab(&mut self, ui: &mut egui::Ui, is_streaming: bool) {
        egui::Grid::new("settings_video_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Stream Preset").color(theme::TEXT_SECONDARY));
                ui.add_enabled_ui(!is_streaming, |ui| {
                    render_preset_controls(ui, &mut self.settings.preset);
                });
                ui.end_row();

                ui.label(egui::RichText::new("Video Codec").color(theme::TEXT_SECONDARY));
                ui.add_enabled_ui(!is_streaming, |ui| {
                    render_codec_controls(ui, &mut self.settings.codec);
                });
                ui.end_row();

                ui.label(egui::RichText::new("Target Bitrate").color(theme::TEXT_SECONDARY));
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

                ui.label(egui::RichText::new("Aspect Scale").color(theme::TEXT_SECONDARY));
                render_scale_selector(ui, &mut self.settings.scale_mode);
                ui.end_row();

                ui.label(egui::RichText::new("Upscale Filter").color(theme::TEXT_SECONDARY));
                render_filter_controls(
                    ui,
                    &mut self.settings.filter_mode,
                    &mut self.settings.sharpness_pct,
                );
                ui.end_row();

                ui.label(egui::RichText::new("Presentation").color(theme::TEXT_SECONDARY));
                ui.checkbox(
                    &mut self.settings.vsync,
                    "VSync (tear-free, 1-frame DXGI wait)",
                )
                .on_hover_text(
                    "Off enables decode-paced tearing + 1000 Hz input polling for lowest latency.",
                );
                ui.end_row();
            });
    }

    fn render_settings_audio_tab(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings_audio_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Master Volume").color(theme::TEXT_SECONDARY));
                ui.add(
                    egui::Slider::new(&mut self.settings.volume_pct, 0..=100)
                        .clamping(egui::SliderClamping::Always)
                        .suffix("%"),
                )
                .on_hover_text("Stream playback gain (0-100%)");
                ui.end_row();

                ui.label(egui::RichText::new("Playback Device").color(theme::TEXT_SECONDARY));
                render_audio_device_combo(
                    ui,
                    "settings_audio_device",
                    &self.audio_devices,
                    &mut self.settings.audio_device,
                    280.0,
                );
                ui.end_row();

                ui.label(egui::RichText::new("Microphone Input").color(theme::TEXT_SECONDARY));
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

    fn render_settings_input_tab(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("settings_input_grid")
            .num_columns(2)
            .spacing([20.0, 12.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("DualSense Touchpad").color(theme::TEXT_SECONDARY));
                ui.checkbox(
                    &mut self.settings.touchpad_touch,
                    "Forward capacitive finger touch",
                )
                .on_hover_text(
                    "Off: click-only (prevents unintended clicks in games). On: capacitive swipe/touch.",
                );
                ui.end_row();

                ui.label(egui::RichText::new("Virtual KB+M").color(theme::TEXT_SECONDARY));
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.settings.kbm_enabled, "Enable Mouse & Keyboard (F8 lock)")
                        .on_hover_text(self.settings.kbm_bindings.summary_tooltip());
                    if ui.add(theme::secondary_button("Keybinds...")).clicked() {
                        self.kbm_modal_open = true;
                    }
                });
                ui.end_row();

                if self.settings.kbm_enabled {
                    ui.label(egui::RichText::new("Sensitivity").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_sensitivity_pct, 1..=100)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label(egui::RichText::new("Inner Deadzone").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_deadzone_pct, 0..=40)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label(egui::RichText::new("Aim Curve").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::Slider::new(&mut self.settings.kbm_curve_pct, 50..=150)
                            .clamping(egui::SliderClamping::Always)
                            .suffix("%"),
                    );
                    ui.end_row();

                    ui.label(egui::RichText::new("Vertical Axis").color(theme::TEXT_SECONDARY));
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
    }
}
