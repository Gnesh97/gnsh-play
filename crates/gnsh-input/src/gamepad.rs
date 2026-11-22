use crate::ffi;
use gnsh_proto::controller::{ControllerState, TouchPoint, buttons};
use std::ffi::CStr;
use std::os::raw::c_int;

const STICK_DEADZONE: i16 = 2200;
const STANDARD_GRAVITY: f32 = 9.80665;
const PS5_TOUCHPAD_WIDTH: f32 = 1919.0;
const PS5_TOUCHPAD_HEIGHT: f32 = 942.0;
const TOUCH_POS_DEADBAND: u16 = 4;

const BUTTON_MAP: &[(ffi::SDL_GamepadButton, u32)] = &[
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_SOUTH,
        buttons::CROSS,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_EAST,
        buttons::CIRCLE,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_WEST,
        buttons::SQUARE,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_NORTH,
        buttons::TRIANGLE,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_DPAD_UP,
        buttons::DPAD_UP,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_DPAD_DOWN,
        buttons::DPAD_DOWN,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_DPAD_LEFT,
        buttons::DPAD_LEFT,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_DPAD_RIGHT,
        buttons::DPAD_RIGHT,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_LEFT_SHOULDER,
        buttons::L1,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER,
        buttons::R1,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_LEFT_STICK,
        buttons::L3,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_RIGHT_STICK,
        buttons::R3,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_START,
        buttons::OPTIONS,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_BACK,
        buttons::SHARE,
    ),
    (
        ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_TOUCHPAD,
        buttons::TOUCHPAD,
    ),
    (ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_GUIDE, buttons::PS),
];

/// Zeroes stick axis readings whose magnitude falls below `deadzone`.
pub fn apply_stick_deadzone(value: i16, deadzone: i16) -> i16 {
    if deadzone <= 0 {
        return value;
    }
    if i32::from(value).abs() < i32::from(deadzone) {
        0
    } else {
        value
    }
}

/// Maps an SDL trigger axis (`0..=32767`, negative clamped to `0`) proportionally to `0..=255`.
pub fn scale_trigger_u8(raw: i16) -> u8 {
    if raw <= 0 {
        return 0;
    }
    ((i32::from(raw) * 255) / 32767) as u8
}

/// Converts SDL gamepad button states into the PlayStation controller bitmask.
pub fn map_sdl_buttons<F>(mut is_pressed: F) -> u32
where
    F: FnMut(ffi::SDL_GamepadButton) -> bool,
{
    let mut mask = 0_u32;
    for &(sdl_btn, ps_btn) in BUTTON_MAP {
        if is_pressed(sdl_btn) {
            mask |= ps_btn;
        }
    }
    mask
}

#[derive(Debug)]
struct TouchpadTracker {
    next_id: u8,
    active_ids: [i8; 2],
    last_pos: [(u16, u16); 2],
}

impl Default for TouchpadTracker {
    fn default() -> Self {
        Self {
            next_id: 0,
            active_ids: [-1, -1],
            last_pos: [(0, 0); 2],
        }
    }
}

impl TouchpadTracker {
    fn reset(&mut self) {
        self.active_ids = [-1, -1];
        self.last_pos = [(0, 0); 2];
    }

    fn update(
        &mut self,
        raw: [(bool, f32, f32); 2],
        allow_capacitive: bool,
        clicked: bool,
    ) -> [TouchPoint; 2] {
        let mut touches = [TouchPoint::default(); 2];
        // Suppress bare capacitive touch when click-only mode is active so resting a thumb does
        // not synthesize a click in PS5 games that misinterpret Remote Play 0xd0 touch events.
        if !allow_capacitive && !clicked {
            self.active_ids = [-1, -1];
            return touches;
        }

        for (i, &(down, x, y)) in raw.iter().enumerate() {
            if !down {
                self.active_ids[i] = -1;
                continue;
            }
            let px = (x.clamp(0.0, 1.0) * PS5_TOUCHPAD_WIDTH).round() as u16;
            let py = (y.clamp(0.0, 1.0) * PS5_TOUCHPAD_HEIGHT).round() as u16;
            if self.active_ids[i] < 0 {
                let id = self.next_id as i8;
                self.next_id = (self.next_id + 1) & 0x7f;
                self.active_ids[i] = id;
                self.last_pos[i] = (px, py);
            } else {
                let (lx, ly) = self.last_pos[i];
                if px.abs_diff(lx) >= TOUCH_POS_DEADBAND || py.abs_diff(ly) >= TOUCH_POS_DEADBAND {
                    self.last_pos[i] = (px, py);
                }
            }
            touches[i] = TouchPoint {
                id: self.active_ids[i],
                x: self.last_pos[i].0,
                y: self.last_pos[i].1,
            };
        }
        touches
    }
}

struct ActiveGamepad {
    ptr: *mut ffi::SDL_Gamepad,
    instance_id: u32,
    name: String,
    is_dualsense: bool,
}

/// Manages an active SDL3 gamepad for controller polling, sensors, LED, and rumble.
pub struct GamepadManager {
    active: Option<ActiveGamepad>,
    touch_tracker: TouchpadTracker,
    mic_btn_down: bool,
    mic_toggle_pending: bool,
}

impl GamepadManager {
    /// Initializes SDL3 and opens the first connected gamepad if available.
    pub fn new() -> anyhow::Result<Self> {
        crate::init_sdl()?;
        let mut manager = Self {
            active: None,
            touch_tracker: TouchpadTracker::default(),
            mic_btn_down: false,
            mic_toggle_pending: false,
        };
        manager.open_first_available(None);
        Ok(manager)
    }

    /// Opens the newly connected gamepad `which` when no gamepad is currently active.
    pub fn on_device_added(&mut self, which: u32) {
        if self.active.is_none() {
            let _ = self.try_open(which);
        }
    }

    /// Closes the active gamepad if it matches `which`, then falls back to another connected gamepad.
    pub fn on_device_removed(&mut self, which: u32) {
        let is_active = self
            .active
            .as_ref()
            .is_some_and(|gp| gp.instance_id == which);
        if is_active {
            tracing::info!("Gamepad removed (id={which})");
            self.close_active();
            self.open_first_available(Some(which));
        }
    }

    /// Returns the name of the active connected gamepad, if any.
    pub fn connected_name(&self) -> Option<&str> {
        self.active.as_ref().map(|gp| gp.name.as_str())
    }

    /// Returns `true` once when the DualSense mute button was newly pressed since the last check.
    pub fn take_mic_toggle(&mut self) -> bool {
        std::mem::take(&mut self.mic_toggle_pending)
    }

    /// Polls buttons, deadzoned sticks, triggers, touchpad fingers, gyro, and accel.
    ///
    /// When `allow_capacitive_touch` is `false`, finger coordinates are only forwarded while the
    /// physical touchpad click button is held (workaround for Remote Play touch-as-click issues).
    pub fn poll_state(&mut self, allow_capacitive_touch: bool) -> Option<ControllerState> {
        let (gp_ptr, is_dualsense) = {
            let gp = self.active.as_ref()?;
            (gp.ptr, gp.is_dualsense)
        };
        if !unsafe { ffi::SDL_GamepadConnected(gp_ptr) } {
            return None;
        }

        if is_dualsense {
            // SAFETY: gp_ptr is a connected DualSense SDL_Gamepad* owned by self.active.
            let mic_down = unsafe {
                ffi::SDL_GetGamepadButton(gp_ptr, ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_MISC1)
            };
            if mic_down && !self.mic_btn_down {
                self.mic_toggle_pending = true;
            }
            self.mic_btn_down = mic_down;
        } else {
            self.mic_btn_down = false;
        }

        let buttons = map_sdl_buttons(|btn| unsafe { ffi::SDL_GetGamepadButton(gp_ptr, btn) });

        let left_x = apply_stick_deadzone(
            unsafe { ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFTX) },
            STICK_DEADZONE,
        );
        let left_y = apply_stick_deadzone(
            unsafe { ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFTY) },
            STICK_DEADZONE,
        );
        let right_x = apply_stick_deadzone(
            unsafe {
                ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHTX)
            },
            STICK_DEADZONE,
        );
        let right_y = apply_stick_deadzone(
            unsafe {
                ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHTY)
            },
            STICK_DEADZONE,
        );

        let l2 = scale_trigger_u8(unsafe {
            ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFT_TRIGGER)
        });
        let r2 = scale_trigger_u8(unsafe {
            ffi::SDL_GetGamepadAxis(gp_ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHT_TRIGGER)
        });

        let raw_touches = poll_touchpad_raw(gp_ptr);
        let touchpad_clicked = (buttons & buttons::TOUCHPAD) != 0;
        let touches =
            self.touch_tracker
                .update(raw_touches, allow_capacitive_touch, touchpad_clicked);
        let gyro = read_sensor(gp_ptr, ffi::SDL_SensorType_SDL_SENSOR_GYRO, [0.0; 3]);
        let accel_mps2 = read_sensor(
            gp_ptr,
            ffi::SDL_SensorType_SDL_SENSOR_ACCEL,
            [0.0, STANDARD_GRAVITY, 0.0],
        );
        let accel = [
            accel_mps2[0] / STANDARD_GRAVITY,
            accel_mps2[1] / STANDARD_GRAVITY,
            accel_mps2[2] / STANDARD_GRAVITY,
        ];

        Some(ControllerState {
            buttons,
            l2,
            r2,
            left_x,
            left_y,
            right_x,
            right_y,
            touches,
            gyro,
            accel,
            ..Default::default()
        })
    }

    /// Triggers low/high-frequency rumble motors on the active gamepad (500 ms fallback or DS5 HID).
    pub fn set_rumble(&self, left: u8, right: u8) {
        let Some(gp) = self.active.as_ref() else {
            return;
        };
        if gp.is_dualsense {
            let packet = build_dualsense_rumble_packet(left, right);
            self.send_dualsense_effect(&packet, "rumble");
            return;
        }
        let ok = unsafe {
            ffi::SDL_RumbleGamepad(gp.ptr, u16::from(left) * 257, u16::from(right) * 257, 500)
        };
        if !ok {
            tracing::debug!("SDL_RumbleGamepad failed: {}", crate::sdl_error());
        }
    }

    /// Updates the lightbar LED color on the active gamepad.
    pub fn set_led(&self, r: u8, g: u8, b: u8) {
        let Some(gp) = self.active.as_ref() else {
            return;
        };
        let ok = unsafe { ffi::SDL_SetGamepadLED(gp.ptr, r, g, b) };
        if !ok {
            tracing::debug!("SDL_SetGamepadLED failed: {}", crate::sdl_error());
        }
    }

    /// Updates the DualSense orange microphone mute LED when a PS5 controller is active.
    pub fn set_mic_led(&self, muted: bool) {
        let packet = build_dualsense_mic_led_packet(muted);
        self.send_dualsense_effect(&packet, "mic_led");
    }

    /// Sends DualSense adaptive trigger effects (`L2`/`R2`) via SDL3 when a PS5 controller is active.
    pub fn set_trigger_effects(
        &self,
        type_left: u8,
        left: &[u8; 10],
        type_right: u8,
        right: &[u8; 10],
    ) {
        let packet = build_dualsense_trigger_effect_packet(type_left, left, type_right, right);
        self.send_dualsense_effect(&packet, "trigger_effects");
    }

    /// Resets left and right DualSense adaptive triggers back to off (`0x05`).
    pub fn clear_trigger_effects(&self) {
        self.set_trigger_effects(0x05, &[0u8; 10], 0x05, &[0u8; 10]);
    }

    fn send_dualsense_effect(&self, packet: &[u8; 47], label: &str) {
        let Some(gp) = self.active.as_ref() else {
            return;
        };
        if !gp.is_dualsense {
            return;
        }
        // SAFETY: gp.ptr is an open SDL_Gamepad* owned by self.active, and packet points to a
        // 47-byte stack-allocated DualSense effect state buffer valid for the duration of the call.
        let ok = unsafe {
            ffi::SDL_SendGamepadEffect(gp.ptr, packet.as_ptr().cast(), packet.len() as c_int)
        };
        if !ok {
            tracing::debug!(
                "SDL_SendGamepadEffect({label}) failed: {}",
                crate::sdl_error()
            );
        }
    }

    fn open_first_available(&mut self, skip_id: Option<u32>) {
        let mut count: c_int = 0;
        let ids = unsafe { ffi::SDL_GetGamepads(&mut count) };
        if ids.is_null() {
            return;
        }
        if count > 0 {
            let slice = unsafe { std::slice::from_raw_parts(ids, count as usize) };
            for &id in slice {
                if Some(id) == skip_id {
                    continue;
                }
                if self.try_open(id) {
                    break;
                }
            }
        }
        unsafe { ffi::SDL_free(ids.cast()) };
    }

    fn try_open(&mut self, id: u32) -> bool {
        let ptr = unsafe { ffi::SDL_OpenGamepad(id) };
        if ptr.is_null() {
            tracing::debug!("SDL_OpenGamepad({id}) failed: {}", crate::sdl_error());
            return false;
        }

        let instance_id = {
            let queried = unsafe { ffi::SDL_GetGamepadID(ptr) };
            if queried == 0 { id } else { queried }
        };

        let name = unsafe {
            let name_ptr = ffi::SDL_GetGamepadName(ptr);
            if name_ptr.is_null() {
                "Unknown Gamepad".to_string()
            } else {
                CStr::from_ptr(name_ptr).to_string_lossy().into_owned()
            }
        };

        enable_sensor_if_present(ptr, ffi::SDL_SensorType_SDL_SENSOR_ACCEL);
        enable_sensor_if_present(ptr, ffi::SDL_SensorType_SDL_SENSOR_GYRO);
        // SAFETY: ptr is a non-null SDL_Gamepad* freshly opened by SDL_OpenGamepad on this thread.
        let is_dualsense =
            unsafe { ffi::SDL_GetGamepadType(ptr) } == ffi::SDL_GamepadType_SDL_GAMEPAD_TYPE_PS5;

        tracing::info!("Opened gamepad: {name} (id={instance_id}, dualsense={is_dualsense})");
        self.touch_tracker.reset();
        self.mic_btn_down = false;
        self.mic_toggle_pending = false;
        self.active = Some(ActiveGamepad {
            ptr,
            instance_id,
            name,
            is_dualsense,
        });
        true
    }

    fn close_active(&mut self) {
        self.set_rumble(0, 0);
        self.clear_trigger_effects();
        self.set_mic_led(false);
        self.touch_tracker.reset();
        self.mic_btn_down = false;
        self.mic_toggle_pending = false;
        if let Some(gp) = self.active.take()
            && !gp.ptr.is_null()
        {
            unsafe {
                ffi::SDL_CloseGamepad(gp.ptr);
            }
        }
    }
}

impl Drop for GamepadManager {
    fn drop(&mut self) {
        self.close_active();
    }
}

fn enable_sensor_if_present(gamepad: *mut ffi::SDL_Gamepad, sensor: ffi::SDL_SensorType) {
    unsafe {
        if ffi::SDL_GamepadHasSensor(gamepad, sensor)
            && !ffi::SDL_SetGamepadSensorEnabled(gamepad, sensor, true)
        {
            tracing::debug!(
                "SDL_SetGamepadSensorEnabled({sensor}) failed: {}",
                crate::sdl_error()
            );
        }
    }
}

fn read_sensor(
    gamepad: *mut ffi::SDL_Gamepad,
    sensor: ffi::SDL_SensorType,
    default: [f32; 3],
) -> [f32; 3] {
    let mut data = default;
    unsafe {
        if ffi::SDL_GamepadSensorEnabled(gamepad, sensor) {
            let mut raw = [0.0_f32; 3];
            if ffi::SDL_GetGamepadSensorData(gamepad, sensor, raw.as_mut_ptr(), 3) {
                data = raw;
            }
        }
    }
    data
}

fn poll_touchpad_raw(gamepad: *mut ffi::SDL_Gamepad) -> [(bool, f32, f32); 2] {
    let mut raw = [(false, 0.0_f32, 0.0_f32); 2];
    let num_touchpads = unsafe { ffi::SDL_GetNumGamepadTouchpads(gamepad) };
    if num_touchpads <= 0 {
        return raw;
    }

    let num_fingers = unsafe { ffi::SDL_GetNumGamepadTouchpadFingers(gamepad, 0) }.clamp(0, 2);
    for finger_idx in 0..num_fingers {
        let mut down = false;
        let mut x = 0.0_f32;
        let mut y = 0.0_f32;
        let ok = unsafe {
            ffi::SDL_GetGamepadTouchpadFinger(
                gamepad,
                0,
                finger_idx,
                &mut down,
                &mut x,
                &mut y,
                std::ptr::null_mut(),
            )
        };
        if ok && down {
            raw[finger_idx as usize] = (true, x, y);
        }
    }
    raw
}

/// Encodes Sony DualSense 47-byte trigger effect HID state matching SDL's `DS5EffectsState_t`.
pub fn build_dualsense_trigger_effect_packet(
    type_left: u8,
    left: &[u8; 10],
    type_right: u8,
    right: &[u8; 10],
) -> [u8; 47] {
    let mut buf = [0u8; 47];
    buf[0] = 0x04 | 0x08;
    buf[1] = 0x40;
    buf[10] = type_right;
    buf[11..21].copy_from_slice(right);
    buf[21] = type_left;
    buf[22..32].copy_from_slice(left);
    buf
}

/// Encodes Sony DualSense 47-byte rumble HID state matching `chiaki-ng`'s `SetDualSenseRumble`.
pub fn build_dualsense_rumble_packet(left: u8, right: u8) -> [u8; 47] {
    let mut buf = [0u8; 47];
    buf[0] = 0x02;
    buf[1] = 0x40;
    buf[2] = right;
    buf[3] = left;
    buf[38] = 0x04;
    buf
}

/// Encodes Sony DualSense 47-byte microphone LED & mute-bits HID packet (`DS5EffectsState_t`).
pub fn build_dualsense_mic_led_packet(muted: bool) -> [u8; 47] {
    let mut buf = [0u8; 47];
    buf[1] = 0x01 | 0x02;
    buf[8] = u8::from(muted);
    buf[9] = if muted { 0x08 } else { 0x00 };
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_47_byte_dualsense_trigger_effect_packet() {
        let left = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let right = [11, 12, 13, 14, 15, 16, 17, 18, 19, 20];
        let packet = build_dualsense_trigger_effect_packet(0x21, &left, 0x26, &right);
        assert_eq!(packet.len(), 47);
        assert_eq!(packet[0], 0x0c);
        assert_eq!(packet[1], 0x40);
        assert_eq!(packet[10], 0x26);
        assert_eq!(&packet[11..21], &right);
        assert_eq!(packet[21], 0x21);
        assert_eq!(&packet[22..32], &left);
    }

    #[test]
    fn builds_47_byte_dualsense_rumble_packet() {
        let packet = build_dualsense_rumble_packet(180, 90);
        assert_eq!(packet.len(), 47);
        assert_eq!(packet[0], 0x02);
        assert_eq!(packet[1], 0x40);
        assert_eq!(packet[2], 90);
        assert_eq!(packet[3], 180);
        assert_eq!(packet[36], 0x00);
        assert_eq!(packet[38], 0x04);
    }

    #[test]
    fn builds_47_byte_dualsense_mic_led_packet() {
        let muted = build_dualsense_mic_led_packet(true);
        assert_eq!(muted.len(), 47);
        assert_eq!(muted[1], 0x03);
        assert_eq!(muted[8], 0x01);
        assert_eq!(muted[9], 0x08);

        let live = build_dualsense_mic_led_packet(false);
        assert_eq!(live.len(), 47);
        assert_eq!(live[1], 0x03);
        assert_eq!(live[8], 0x00);
        assert_eq!(live[9], 0x00);
    }

    #[test]
    fn deadzone_zeroes_interior_and_preserves_extremes() {
        assert_eq!(apply_stick_deadzone(0, 2200), 0);
        assert_eq!(apply_stick_deadzone(2199, 2200), 0);
        assert_eq!(apply_stick_deadzone(-2199, 2200), 0);
        assert_eq!(apply_stick_deadzone(2200, 2200), 2200);
        assert_eq!(apply_stick_deadzone(-2200, 2200), -2200);
        assert_eq!(apply_stick_deadzone(i16::MAX, 2200), i16::MAX);
        assert_eq!(apply_stick_deadzone(i16::MIN, 2200), i16::MIN);
        assert_eq!(apply_stick_deadzone(-100, 0), -100);
        assert_eq!(apply_stick_deadzone(500, -10), 500);
    }

    #[test]
    fn trigger_scaling_clamps_negative_and_scales_to_u8() {
        assert_eq!(scale_trigger_u8(i16::MIN), 0);
        assert_eq!(scale_trigger_u8(-1), 0);
        assert_eq!(scale_trigger_u8(0), 0);
        assert_eq!(scale_trigger_u8(i16::MAX), 255);
        assert_eq!(scale_trigger_u8(16384), 127);
    }

    #[test]
    fn button_mapping_covers_all_ps_buttons() {
        assert_eq!(map_sdl_buttons(|_| false), 0);

        let all_expected = buttons::CROSS
            | buttons::CIRCLE
            | buttons::SQUARE
            | buttons::TRIANGLE
            | buttons::DPAD_UP
            | buttons::DPAD_DOWN
            | buttons::DPAD_LEFT
            | buttons::DPAD_RIGHT
            | buttons::L1
            | buttons::R1
            | buttons::L3
            | buttons::R3
            | buttons::OPTIONS
            | buttons::SHARE
            | buttons::TOUCHPAD
            | buttons::PS;
        assert_eq!(map_sdl_buttons(|_| true), all_expected);

        let subset = map_sdl_buttons(|btn| {
            matches!(
                btn,
                ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_SOUTH
                    | ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_START
                    | ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_TOUCHPAD
                    | ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_GUIDE
            )
        });
        assert_eq!(
            subset,
            buttons::CROSS | buttons::OPTIONS | buttons::TOUCHPAD | buttons::PS
        );

        let unmapped =
            map_sdl_buttons(|btn| btn == ffi::SDL_GamepadButton_SDL_GAMEPAD_BUTTON_MISC1);
        assert_eq!(unmapped, 0);
    }

    #[test]
    fn touchpad_click_only_suppresses_bare_touch_and_allows_clicked_position() {
        let mut tracker = TouchpadTracker::default();
        let bare = tracker.update([(true, 0.5, 0.5), (false, 0.0, 0.0)], false, false);
        assert_eq!(bare, [TouchPoint::default(); 2]);

        let clicked = tracker.update([(true, 0.25, 0.5), (false, 0.0, 0.0)], false, true);
        assert_eq!(clicked[0].id, 0);
        assert_eq!(clicked[0].x, (0.25_f32 * PS5_TOUCHPAD_WIDTH).round() as u16);
        assert_eq!(clicked[0].y, (0.5_f32 * PS5_TOUCHPAD_HEIGHT).round() as u16);
        assert_eq!(clicked[1].id, -1);

        let released = tracker.update([(true, 0.25, 0.5), (false, 0.0, 0.0)], false, false);
        assert_eq!(released, [TouchPoint::default(); 2]);
    }

    #[test]
    fn touchpad_capacitive_mode_increments_7bit_ids_and_clamps_bounds() {
        let mut tracker = TouchpadTracker {
            next_id: 127,
            ..Default::default()
        };
        let first = tracker.update([(true, 1.5, -0.2), (false, 0.0, 0.0)], true, false);
        assert_eq!(first[0].id, 127);
        assert_eq!(first[0].x, 1919);
        assert_eq!(first[0].y, 0);

        // Noise smaller than deadband preserves stabilized coordinates without new movement.
        let jitter = tracker.update([(true, 0.999, 0.001), (false, 0.0, 0.0)], true, false);
        assert_eq!(jitter[0], first[0]);

        // Lift then touch again wraps 7-bit tracking ID cleanly from 127 -> 0.
        let lifted = tracker.update([(false, 0.0, 0.0), (false, 0.0, 0.0)], true, false);
        assert_eq!(lifted[0].id, -1);
        let second = tracker.update([(true, 0.5, 1.0), (false, 0.0, 0.0)], true, false);
        assert_eq!(second[0].id, 0);
        assert_eq!(second[0].y, 942);
    }
}
