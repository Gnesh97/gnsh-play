use crate::ffi;
use gnsh_proto::controller::{ControllerState, TouchPoint, buttons};
use std::ffi::CStr;
use std::os::raw::c_int;

const STICK_DEADZONE: i16 = 2200;
const STANDARD_GRAVITY: f32 = 9.80665;
const PS5_TOUCHPAD_WIDTH: f32 = 1920.0;
const PS5_TOUCHPAD_HEIGHT: f32 = 1080.0;

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

struct ActiveGamepad {
    ptr: *mut ffi::SDL_Gamepad,
    instance_id: u32,
    name: String,
}

/// Manages an active SDL3 gamepad for controller polling, sensors, LED, and rumble.
pub struct GamepadManager {
    active: Option<ActiveGamepad>,
}

impl GamepadManager {
    /// Initializes SDL3 and opens the first connected gamepad if available.
    pub fn new() -> anyhow::Result<Self> {
        crate::init_sdl()?;
        let mut manager = Self { active: None };
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

    /// Polls buttons, deadzoned sticks, triggers, touchpad fingers, gyro, and accel.
    pub fn poll_state(&self) -> Option<ControllerState> {
        let gp = self.active.as_ref()?;
        if !unsafe { ffi::SDL_GamepadConnected(gp.ptr) } {
            return None;
        }

        let buttons = map_sdl_buttons(|btn| unsafe { ffi::SDL_GetGamepadButton(gp.ptr, btn) });

        let left_x = apply_stick_deadzone(
            unsafe { ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFTX) },
            STICK_DEADZONE,
        );
        let left_y = apply_stick_deadzone(
            unsafe { ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFTY) },
            STICK_DEADZONE,
        );
        let right_x = apply_stick_deadzone(
            unsafe {
                ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHTX)
            },
            STICK_DEADZONE,
        );
        let right_y = apply_stick_deadzone(
            unsafe {
                ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHTY)
            },
            STICK_DEADZONE,
        );

        let l2 = scale_trigger_u8(unsafe {
            ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_LEFT_TRIGGER)
        });
        let r2 = scale_trigger_u8(unsafe {
            ffi::SDL_GetGamepadAxis(gp.ptr, ffi::SDL_GamepadAxis_SDL_GAMEPAD_AXIS_RIGHT_TRIGGER)
        });

        let touches = poll_touchpad(gp.ptr);
        let gyro = read_sensor(gp.ptr, ffi::SDL_SensorType_SDL_SENSOR_GYRO, [0.0; 3]);
        let accel_mps2 = read_sensor(
            gp.ptr,
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

    /// Triggers low/high-frequency rumble motors on the active gamepad for 500 ms.
    pub fn set_rumble(&self, left: u8, right: u8) {
        let Some(gp) = self.active.as_ref() else {
            return;
        };
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

        tracing::info!("Opened gamepad: {name} (id={instance_id})");
        self.active = Some(ActiveGamepad {
            ptr,
            instance_id,
            name,
        });
        true
    }

    fn close_active(&mut self) {
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

fn poll_touchpad(gamepad: *mut ffi::SDL_Gamepad) -> [TouchPoint; 2] {
    let mut touches = [TouchPoint::default(); 2];
    let num_touchpads = unsafe { ffi::SDL_GetNumGamepadTouchpads(gamepad) };
    if num_touchpads <= 0 {
        return touches;
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
            touches[finger_idx as usize] = TouchPoint {
                id: finger_idx as i8,
                x: (x.clamp(0.0, 1.0) * PS5_TOUCHPAD_WIDTH).round() as u16,
                y: (y.clamp(0.0, 1.0) * PS5_TOUCHPAD_HEIGHT).round() as u16,
            };
        }
    }
    touches
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
