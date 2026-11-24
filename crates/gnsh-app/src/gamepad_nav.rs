use crate::ui::{SettingsTab, StreamSettings};
use gnsh_media::{RendererBackend, ScaleMode, VideoFilterMode};
use gnsh_proto::controller::{ControllerState, buttons};
use gnsh_proto::{MAX_BITRATE_KBPS, MIN_BITRATE_KBPS, VideoCodec, VideoPreset};
use std::time::{Duration, Instant};

const STICK_THRESHOLD: i16 = 16_000;
const REPEAT_DELAY: Duration = Duration::from_millis(220);
const HUD_CHORD_MASK: u32 = buttons::L1 | buttons::R1 | buttons::OPTIONS;

pub(crate) const HUD_QUICK_ROWS: usize = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeNavEvent {
    PrevConsole,
    NextConsole,
    Left,
    Right,
    Connect,
    Wake,
    Discover,
    ToggleSettings,
    PrevSettingsTab,
    NextSettingsTab,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HudNavAction {
    None,
    ToggleMic,
    GoHome,
    OpenSettings,
    Disconnect,
}

#[derive(Debug)]
pub(crate) struct GamepadHomeNav {
    prev_buttons: u32,
    prev_stick_y: i8,
    last_repeat_y: Instant,
    prev_stick_x: i8,
    last_repeat_x: Instant,
    wait_for_release: bool,
    prev_hud_chord: bool,
}

impl Default for GamepadHomeNav {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            prev_buttons: 0,
            prev_stick_y: 0,
            last_repeat_y: now,
            prev_stick_x: 0,
            last_repeat_x: now,
            wait_for_release: false,
            prev_hud_chord: false,
        }
    }
}

fn stick_axis_dir(axis: i16) -> i8 {
    if axis < -STICK_THRESHOLD {
        -1
    } else if axis > STICK_THRESHOLD {
        1
    } else {
        0
    }
}

#[allow(clippy::too_many_arguments)]
fn push_stick_repeat(
    axis: i16,
    prev_dir: &mut i8,
    last_repeat: &mut Instant,
    now: Instant,
    neg_event: HomeNavEvent,
    pos_event: HomeNavEvent,
    out: &mut Vec<HomeNavEvent>,
) {
    let dir = stick_axis_dir(axis);
    if dir != 0 {
        let should_emit =
            dir != *prev_dir || now.saturating_duration_since(*last_repeat) >= REPEAT_DELAY;
        if should_emit {
            out.push(if dir < 0 { neg_event } else { pos_event });
            *last_repeat = now;
        }
    }
    *prev_dir = dir;
}

impl GamepadHomeNav {
    /// Suppresses UI actions until held buttons and stick axes return to neutral.
    pub(crate) fn arm_release_guard(&mut self) {
        self.wait_for_release = true;
        self.prev_buttons = 0;
        self.prev_stick_y = 0;
        self.prev_stick_x = 0;
    }

    /// Resets edge-detection state when no gamepad is connected.
    pub(crate) fn reset(&mut self) {
        self.prev_buttons = 0;
        self.prev_stick_y = 0;
        self.prev_stick_x = 0;
        self.wait_for_release = false;
        self.prev_hud_chord = false;
    }

    /// Returns `true` on the rising edge when `L1 + R1 + Options` are held together.
    pub(crate) fn poll_hud_chord(&mut self, btn_mask: u32) -> bool {
        let active = (btn_mask & HUD_CHORD_MASK) == HUD_CHORD_MASK;
        let fired = active && !self.prev_hud_chord;
        self.prev_hud_chord = active;
        fired
    }

    pub(crate) fn update(&mut self, state: &ControllerState) -> Vec<HomeNavEvent> {
        self.update_at(state, Instant::now())
    }

    pub(crate) fn update_at(&mut self, state: &ControllerState, now: Instant) -> Vec<HomeNavEvent> {
        let dir_y = stick_axis_dir(state.left_y);
        let dir_x = stick_axis_dir(state.left_x);

        if self.wait_for_release {
            if state.buttons == 0 && dir_y == 0 && dir_x == 0 {
                self.wait_for_release = false;
                self.prev_buttons = 0;
                self.prev_stick_y = 0;
                self.prev_stick_x = 0;
            }
            return Vec::new();
        }

        let mut events = Vec::new();
        let newly_pressed = state.buttons & !self.prev_buttons;
        self.prev_buttons = state.buttons;

        let button_bindings = [
            (buttons::DPAD_UP, HomeNavEvent::PrevConsole),
            (buttons::DPAD_DOWN, HomeNavEvent::NextConsole),
            (buttons::DPAD_LEFT, HomeNavEvent::Left),
            (buttons::DPAD_RIGHT, HomeNavEvent::Right),
            (buttons::L1, HomeNavEvent::PrevSettingsTab),
            (buttons::R1, HomeNavEvent::NextSettingsTab),
            (buttons::CROSS, HomeNavEvent::Connect),
            (buttons::TRIANGLE, HomeNavEvent::Wake),
            (buttons::SQUARE, HomeNavEvent::Discover),
            (buttons::OPTIONS, HomeNavEvent::ToggleSettings),
            (buttons::CIRCLE, HomeNavEvent::Back),
        ];

        for &(mask, event) in &button_bindings {
            if (newly_pressed & mask) != 0 {
                events.push(event);
            }
        }

        push_stick_repeat(
            state.left_y,
            &mut self.prev_stick_y,
            &mut self.last_repeat_y,
            now,
            HomeNavEvent::PrevConsole,
            HomeNavEvent::NextConsole,
            &mut events,
        );
        push_stick_repeat(
            state.left_x,
            &mut self.prev_stick_x,
            &mut self.last_repeat_x,
            now,
            HomeNavEvent::Left,
            HomeNavEvent::Right,
            &mut events,
        );

        events
    }
}

pub(crate) fn settings_row_count(
    tab: SettingsTab,
    kbm_enabled: bool,
    wan_profile_enabled: bool,
) -> usize {
    match tab {
        SettingsTab::Video => {
            if wan_profile_enabled {
                13
            } else {
                10
            }
        }
        SettingsTab::Audio => 3,
        SettingsTab::Input => {
            if kbm_enabled {
                6
            } else {
                2
            }
        }
    }
}

fn adjust_preset(preset: &mut VideoPreset, event: HomeNavEvent) -> bool {
    nav_dir(event).is_some_and(|dir| {
        cycle_slice(
            preset,
            &[
                VideoPreset::P720Fps30,
                VideoPreset::P720Fps60,
                VideoPreset::P1080Fps30,
                VideoPreset::P1080Fps60,
            ],
            dir,
        )
    })
}

fn adjust_codec(codec: &mut VideoCodec, event: HomeNavEvent) -> bool {
    nav_dir(event).is_some_and(|dir| {
        cycle_slice(
            codec,
            &[VideoCodec::H264, VideoCodec::Hevc, VideoCodec::HevcHdr],
            dir,
        )
    })
}

fn adjust_bitrate(bitrate_kbps: &mut u32, event: HomeNavEvent, step: i32) -> bool {
    slider_dir(event).is_some_and(|dir| {
        step_u32(
            bitrate_kbps,
            (dir as i32) * step,
            MIN_BITRATE_KBPS,
            MAX_BITRATE_KBPS,
        )
    })
}

fn cycle_slice<T: PartialEq + Copy>(val: &mut T, items: &[T], dir: isize) -> bool {
    let Some(idx) = items.iter().position(|item| item == val) else {
        return false;
    };
    let len = items.len() as isize;
    let next = (idx as isize + dir).rem_euclid(len) as usize;
    if next != idx {
        *val = items[next];
        true
    } else {
        false
    }
}

fn cycle_device_opt(selected: &mut Option<String>, devices: &[String], dir: isize) -> bool {
    let len = devices.len() + 1;
    let cur = selected
        .as_deref()
        .and_then(|name| devices.iter().position(|d| d == name))
        .map_or(0, |idx| idx + 1);
    let next = ((cur as isize) + dir).rem_euclid(len as isize) as usize;
    let target = if next == 0 {
        None
    } else {
        Some(devices[next - 1].clone())
    };
    if *selected != target {
        *selected = target;
        true
    } else {
        false
    }
}

fn step_u8(val: &mut u8, delta: i16, min: u8, max: u8) -> bool {
    let next = (i16::from(*val) + delta).clamp(i16::from(min), i16::from(max)) as u8;
    if next != *val {
        *val = next;
        true
    } else {
        false
    }
}

fn step_u32(val: &mut u32, delta: i32, min: u32, max: u32) -> bool {
    let next = ((*val as i64) + i64::from(delta)).clamp(i64::from(min), i64::from(max)) as u32;
    if next != *val {
        *val = next;
        true
    } else {
        false
    }
}

fn adjust_scale_mode(scale_mode: &mut ScaleMode, dir: isize) -> bool {
    cycle_slice(
        scale_mode,
        &[ScaleMode::Fit, ScaleMode::Zoom, ScaleMode::Stretch],
        dir,
    )
}

fn adjust_filter_mode(filter_mode: &mut VideoFilterMode, dir: isize) -> bool {
    cycle_slice(
        filter_mode,
        &[
            VideoFilterMode::Bilinear,
            VideoFilterMode::Cas,
            VideoFilterMode::Fsr1,
        ],
        dir,
    )
}

fn adjust_renderer_backend(backend: &mut RendererBackend, dir: isize) -> bool {
    cycle_slice(
        backend,
        &[
            RendererBackend::Auto,
            RendererBackend::D3D11,
            RendererBackend::Vulkan,
            RendererBackend::D3D12,
        ],
        dir,
    )
}

fn adjust_sharpness(filter_mode: VideoFilterMode, sharpness_pct: &mut u8, dir: isize) -> bool {
    if filter_mode == VideoFilterMode::Bilinear {
        return false;
    }
    step_u8(sharpness_pct, (dir as i16) * 5, 0, 100)
}

fn nav_dir(event: HomeNavEvent) -> Option<isize> {
    match event {
        HomeNavEvent::Left => Some(-1),
        HomeNavEvent::Right | HomeNavEvent::Connect => Some(1),
        _ => None,
    }
}

fn slider_dir(event: HomeNavEvent) -> Option<isize> {
    match event {
        HomeNavEvent::Left => Some(-1),
        HomeNavEvent::Right => Some(1),
        _ => None,
    }
}

/// Applies Left/Right/Cross navigation input to the focused row of the Settings modal.
pub(crate) fn apply_settings_nav(
    settings: &mut StreamSettings,
    tab: SettingsTab,
    row: usize,
    event: HomeNavEvent,
    is_streaming: bool,
    audio_devices: &[String],
    mic_devices: &[String],
) -> bool {
    match tab {
        SettingsTab::Video => match row {
            0 if !is_streaming => adjust_preset(&mut settings.preset, event),
            1 if !is_streaming => adjust_codec(&mut settings.codec, event),
            2 if !is_streaming => adjust_bitrate(&mut settings.bitrate_kbps, event, 5_000),
            3 => nav_dir(event).is_some_and(|dir| adjust_scale_mode(&mut settings.scale_mode, dir)),
            4 => {
                nav_dir(event).is_some_and(|dir| adjust_filter_mode(&mut settings.filter_mode, dir))
            }
            5 => slider_dir(event).is_some_and(|dir| {
                adjust_sharpness(settings.filter_mode, &mut settings.sharpness_pct, dir)
            }),
            6 if nav_dir(event).is_some() => {
                settings.vsync = !settings.vsync;
                true
            }
            7 if nav_dir(event).is_some() => {
                settings.auto_reconnect = !settings.auto_reconnect;
                true
            }
            8 => nav_dir(event)
                .is_some_and(|dir| adjust_renderer_backend(&mut settings.renderer_backend, dir)),
            9 if nav_dir(event).is_some() => {
                settings.wan_profile_enabled = !settings.wan_profile_enabled;
                true
            }
            10 if !is_streaming && settings.wan_profile_enabled => {
                adjust_preset(&mut settings.wan_preset, event)
            }
            11 if !is_streaming && settings.wan_profile_enabled => {
                adjust_codec(&mut settings.wan_codec, event)
            }
            12 if !is_streaming && settings.wan_profile_enabled => {
                adjust_bitrate(&mut settings.wan_bitrate_kbps, event, 2_000)
            }
            _ => false,
        },
        SettingsTab::Audio => match row {
            0 => slider_dir(event)
                .is_some_and(|dir| step_u8(&mut settings.volume_pct, (dir as i16) * 5, 0, 100)),
            1 => nav_dir(event).is_some_and(|dir| {
                cycle_device_opt(&mut settings.audio_device, audio_devices, dir)
            }),
            2 => nav_dir(event)
                .is_some_and(|dir| cycle_device_opt(&mut settings.mic_device, mic_devices, dir)),
            _ => false,
        },
        SettingsTab::Input => match row {
            0 if nav_dir(event).is_some() => {
                settings.touchpad_touch = !settings.touchpad_touch;
                true
            }
            1 if nav_dir(event).is_some() => {
                settings.kbm_enabled = !settings.kbm_enabled;
                true
            }
            2 if settings.kbm_enabled => slider_dir(event).is_some_and(|dir| {
                step_u8(&mut settings.kbm_sensitivity_pct, (dir as i16) * 5, 1, 100)
            }),
            3 if settings.kbm_enabled => slider_dir(event).is_some_and(|dir| {
                step_u8(&mut settings.kbm_deadzone_pct, (dir as i16) * 2, 0, 40)
            }),
            4 if settings.kbm_enabled => slider_dir(event)
                .is_some_and(|dir| step_u8(&mut settings.kbm_curve_pct, (dir as i16) * 5, 50, 150)),
            5 if settings.kbm_enabled && nav_dir(event).is_some() => {
                settings.kbm_invert_y = !settings.kbm_invert_y;
                true
            }
            _ => false,
        },
    }
}

/// Applies gamepad navigation input to the Streaming HUD Quick Settings drawer.
pub(crate) fn apply_hud_nav(
    settings: &mut StreamSettings,
    row: usize,
    event: HomeNavEvent,
) -> (bool, HudNavAction) {
    match row {
        0 => (
            nav_dir(event).is_some_and(|dir| adjust_scale_mode(&mut settings.scale_mode, dir)),
            HudNavAction::None,
        ),
        1 => (
            nav_dir(event).is_some_and(|dir| adjust_filter_mode(&mut settings.filter_mode, dir)),
            HudNavAction::None,
        ),
        2 => (
            slider_dir(event).is_some_and(|dir| {
                adjust_sharpness(settings.filter_mode, &mut settings.sharpness_pct, dir)
            }),
            HudNavAction::None,
        ),
        3 => (
            slider_dir(event)
                .is_some_and(|dir| step_u8(&mut settings.volume_pct, (dir as i16) * 5, 0, 100)),
            HudNavAction::None,
        ),
        4 => {
            if nav_dir(event).is_some() {
                settings.vsync = !settings.vsync;
                (true, HudNavAction::None)
            } else {
                (false, HudNavAction::None)
            }
        }
        5 if event == HomeNavEvent::Connect => (false, HudNavAction::ToggleMic),
        6 if event == HomeNavEvent::Connect => (false, HudNavAction::GoHome),
        7 if event == HomeNavEvent::Connect => (false, HudNavAction::OpenSettings),
        8 if event == HomeNavEvent::Connect => (false, HudNavAction::Disconnect),
        _ => (false, HudNavAction::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(buttons: u32, left_y: i16) -> ControllerState {
        ControllerState {
            buttons,
            left_y,
            ..Default::default()
        }
    }

    fn state_with_xy(buttons: u32, left_x: i16, left_y: i16) -> ControllerState {
        ControllerState {
            buttons,
            left_x,
            left_y,
            ..Default::default()
        }
    }

    #[test]
    fn button_navigation_fires_once_on_rising_edge() {
        let mut nav = GamepadHomeNav::default();
        let t0 = Instant::now();

        assert_eq!(
            nav.update_at(&state_with(buttons::DPAD_DOWN, 0), t0),
            vec![HomeNavEvent::NextConsole]
        );
        assert!(
            nav.update_at(
                &state_with(buttons::DPAD_DOWN, 0),
                t0 + Duration::from_secs(1)
            )
            .is_empty()
        );
        assert!(
            nav.update_at(&state_with(0, 0), t0 + Duration::from_secs(2))
                .is_empty()
        );
        assert_eq!(
            nav.update_at(
                &state_with(
                    buttons::DPAD_LEFT
                        | buttons::DPAD_RIGHT
                        | buttons::CROSS
                        | buttons::TRIANGLE
                        | buttons::SQUARE
                        | buttons::OPTIONS
                        | buttons::CIRCLE,
                    0
                ),
                t0 + Duration::from_secs(3)
            ),
            vec![
                HomeNavEvent::Left,
                HomeNavEvent::Right,
                HomeNavEvent::Connect,
                HomeNavEvent::Wake,
                HomeNavEvent::Discover,
                HomeNavEvent::ToggleSettings,
                HomeNavEvent::Back,
            ]
        );
    }

    #[test]
    fn left_stick_vertical_and_horizontal_repeat_at_220ms_intervals() {
        let mut nav = GamepadHomeNav::default();
        let t0 = Instant::now();

        assert_eq!(
            nav.update_at(&state_with_xy(0, 22_000, -24_000), t0),
            vec![HomeNavEvent::PrevConsole, HomeNavEvent::Right]
        );
        assert!(
            nav.update_at(
                &state_with_xy(0, 22_000, -24_000),
                t0 + Duration::from_millis(120)
            )
            .is_empty()
        );
        assert_eq!(
            nav.update_at(
                &state_with_xy(0, 22_000, -24_000),
                t0 + Duration::from_millis(220)
            ),
            vec![HomeNavEvent::PrevConsole, HomeNavEvent::Right]
        );
        assert!(
            nav.update_at(
                &state_with_xy(0, 5_000, 5_000),
                t0 + Duration::from_millis(300)
            )
            .is_empty()
        );
        assert_eq!(
            nav.update_at(
                &state_with_xy(0, -20_000, 20_000),
                t0 + Duration::from_millis(310)
            ),
            vec![HomeNavEvent::NextConsole, HomeNavEvent::Left]
        );
    }

    #[test]
    fn release_guard_ignores_held_inputs_after_stream_exit() {
        let mut nav = GamepadHomeNav::default();
        let t0 = Instant::now();
        nav.arm_release_guard();

        assert!(
            nav.update_at(&state_with(buttons::CROSS, -25_000), t0)
                .is_empty()
        );
        assert!(
            nav.update_at(
                &state_with_xy(0, -25_000, 0),
                t0 + Duration::from_millis(300)
            )
            .is_empty()
        );
        assert!(
            nav.update_at(&state_with(0, 0), t0 + Duration::from_millis(320))
                .is_empty()
        );
        assert_eq!(
            nav.update_at(
                &state_with(buttons::CROSS, 0),
                t0 + Duration::from_millis(350)
            ),
            vec![HomeNavEvent::Connect]
        );
    }

    #[test]
    fn hud_chord_fires_once_on_simultaneous_l1_r1_options() {
        let mut nav = GamepadHomeNav::default();
        assert!(!nav.poll_hud_chord(buttons::L1 | buttons::R1));
        assert!(nav.poll_hud_chord(buttons::L1 | buttons::R1 | buttons::OPTIONS));
        assert!(!nav.poll_hud_chord(buttons::L1 | buttons::R1 | buttons::OPTIONS));
        assert!(!nav.poll_hud_chord(buttons::L1 | buttons::OPTIONS));
        assert!(nav.poll_hud_chord(buttons::L1 | buttons::R1 | buttons::OPTIONS));
        nav.reset();
        assert!(nav.poll_hud_chord(buttons::L1 | buttons::R1 | buttons::OPTIONS));
    }

    #[test]
    fn settings_navigation_updates_controls_and_respects_streaming_locks() {
        let mut s = StreamSettings::default();
        let audio = vec!["Speakers".to_string(), "Headset".to_string()];
        let mic = vec!["USB Mic".to_string()];

        assert_eq!(settings_row_count(SettingsTab::Video, false, false), 10);
        assert_eq!(settings_row_count(SettingsTab::Video, false, true), 13);
        assert_eq!(settings_row_count(SettingsTab::Audio, false, false), 3);
        assert_eq!(settings_row_count(SettingsTab::Input, false, false), 2);
        assert_eq!(settings_row_count(SettingsTab::Input, true, false), 6);

        // Locked Video rows 0..=2 while streaming.
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            0,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            1,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            2,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));

        // Unlocked Video rows when idle.
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            0,
            HomeNavEvent::Left,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.preset, VideoPreset::P1080Fps30);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            2,
            HomeNavEvent::Right,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.bitrate_kbps, 35_000);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            4,
            HomeNavEvent::Connect,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.filter_mode, VideoFilterMode::Cas);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            5,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.sharpness_pct, 5);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            7,
            HomeNavEvent::Connect,
            true,
            &audio,
            &mic
        ));
        assert!(!s.auto_reconnect);

        // Renderer selector at row 8 and WAN profile rows 9..=12 gate adjustments cleanly.
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            8,
            HomeNavEvent::Right,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.renderer_backend, RendererBackend::D3D11);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            8,
            HomeNavEvent::Right,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.renderer_backend, RendererBackend::Vulkan);
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            10,
            HomeNavEvent::Right,
            false,
            &audio,
            &mic
        ));
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            9,
            HomeNavEvent::Connect,
            true,
            &audio,
            &mic
        ));
        assert!(s.wan_profile_enabled);
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            10,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            10,
            HomeNavEvent::Left,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.wan_preset, VideoPreset::P720Fps30);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            11,
            HomeNavEvent::Left,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.wan_codec, VideoCodec::H264);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Video,
            12,
            HomeNavEvent::Right,
            false,
            &audio,
            &mic
        ));
        assert_eq!(s.wan_bitrate_kbps, 14_000);

        // Audio controls + device wrapping.
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Audio,
            0,
            HomeNavEvent::Left,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.volume_pct, 95);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Audio,
            1,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.audio_device.as_deref(), Some("Speakers"));
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Audio,
            1,
            HomeNavEvent::Left,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.audio_device, None);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Audio,
            2,
            HomeNavEvent::Left,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.mic_device.as_deref(), Some("USB Mic"));

        // Input KB+M toggle gates rows 2..=5.
        assert!(!apply_settings_nav(
            &mut s,
            SettingsTab::Input,
            2,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Input,
            1,
            HomeNavEvent::Connect,
            true,
            &audio,
            &mic
        ));
        assert!(s.kbm_enabled);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Input,
            3,
            HomeNavEvent::Right,
            true,
            &audio,
            &mic
        ));
        assert_eq!(s.kbm_deadzone_pct, 16);
        assert!(apply_settings_nav(
            &mut s,
            SettingsTab::Input,
            5,
            HomeNavEvent::Connect,
            true,
            &audio,
            &mic
        ));
        assert!(s.kbm_invert_y);
    }

    #[test]
    fn hud_navigation_adjusts_live_settings_and_dispatches_actions_only_on_cross() {
        let mut s = StreamSettings::default();
        assert_eq!(
            apply_hud_nav(&mut s, 0, HomeNavEvent::Right),
            (true, HudNavAction::None)
        );
        assert_eq!(s.scale_mode, ScaleMode::Zoom);
        assert_eq!(
            apply_hud_nav(&mut s, 1, HomeNavEvent::Right),
            (true, HudNavAction::None)
        );
        assert_eq!(s.filter_mode, VideoFilterMode::Cas);
        assert_eq!(
            apply_hud_nav(&mut s, 2, HomeNavEvent::Right),
            (true, HudNavAction::None)
        );
        assert_eq!(s.sharpness_pct, 5);
        assert_eq!(
            apply_hud_nav(&mut s, 3, HomeNavEvent::Left),
            (true, HudNavAction::None)
        );
        assert_eq!(s.volume_pct, 95);
        assert_eq!(
            apply_hud_nav(&mut s, 4, HomeNavEvent::Connect),
            (true, HudNavAction::None)
        );
        assert!(!s.vsync);

        // Action rows only fire on Cross (`Connect`), never Left/Right.
        assert_eq!(
            apply_hud_nav(&mut s, 8, HomeNavEvent::Left),
            (false, HudNavAction::None)
        );
        assert_eq!(
            apply_hud_nav(&mut s, 5, HomeNavEvent::Connect),
            (false, HudNavAction::ToggleMic)
        );
        assert_eq!(
            apply_hud_nav(&mut s, 6, HomeNavEvent::Connect),
            (false, HudNavAction::GoHome)
        );
        assert_eq!(
            apply_hud_nav(&mut s, 7, HomeNavEvent::Connect),
            (false, HudNavAction::OpenSettings)
        );
        assert_eq!(
            apply_hud_nav(&mut s, 8, HomeNavEvent::Connect),
            (false, HudNavAction::Disconnect)
        );
    }
}
