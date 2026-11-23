use crate::ffi;
use gnsh_proto::controller::{ControllerState, buttons};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Instant;

const MOUSE_DECAY_TAU_SECS: f32 = 0.018;
const MOUSE_BASE_SCALE: f32 = 1.0 / 35.0;
const MOUSE_VEL_CUTOFF: f32 = 0.008;
const MOUSE_VEL_ZERO_SNAP: f32 = 0.002;
const MOUSE_RESPONSE_GAMMA: f32 = 0.85;

/// A single keyboard key or mouse button input usable in bindable presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KbmInput {
    Key(u32),
    MouseButton(u8),
}

impl KbmInput {
    /// Returns a concise human-readable label for display in tooltips and binding UI.
    pub fn label(self) -> String {
        match self {
            Self::MouseButton(1) => "LMB".to_string(),
            Self::MouseButton(2) => "MMB".to_string(),
            Self::MouseButton(3) => "RMB".to_string(),
            Self::MouseButton(4) => "Mouse4".to_string(),
            Self::MouseButton(5) => "Mouse5".to_string(),
            Self::MouseButton(btn) => format!("Mouse{btn}"),
            Self::Key(code) => keycode_label(code),
        }
    }
}

/// Complete customizable keyboard + mouse to DualSense mapping schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KbmBindings {
    pub move_up: KbmInput,
    pub move_down: KbmInput,
    pub move_left: KbmInput,
    pub move_right: KbmInput,
    pub l2: KbmInput,
    pub r2: KbmInput,
    pub cross: KbmInput,
    pub circle: KbmInput,
    pub square: KbmInput,
    pub triangle: KbmInput,
    pub l1: KbmInput,
    pub r1: KbmInput,
    pub l3: KbmInput,
    pub r3: KbmInput,
    pub dpad_up: KbmInput,
    pub dpad_down: KbmInput,
    pub dpad_left: KbmInput,
    pub dpad_right: KbmInput,
    pub touchpad: KbmInput,
    pub options: KbmInput,
    pub share: KbmInput,
    pub ps: KbmInput,
}

impl Default for KbmBindings {
    fn default() -> Self {
        Self {
            move_up: KbmInput::Key(ffi::SDLK_W),
            move_down: KbmInput::Key(ffi::SDLK_S),
            move_left: KbmInput::Key(ffi::SDLK_A),
            move_right: KbmInput::Key(ffi::SDLK_D),
            l2: KbmInput::MouseButton(3),
            r2: KbmInput::MouseButton(1),
            cross: KbmInput::Key(ffi::SDLK_SPACE),
            circle: KbmInput::Key(ffi::SDLK_C),
            square: KbmInput::Key(ffi::SDLK_R),
            triangle: KbmInput::Key(ffi::SDLK_F),
            l1: KbmInput::Key(ffi::SDLK_Q),
            r1: KbmInput::Key(ffi::SDLK_E),
            l3: KbmInput::Key(ffi::SDLK_LSHIFT),
            r3: KbmInput::Key(ffi::SDLK_V),
            dpad_up: KbmInput::Key(ffi::SDLK_UP),
            dpad_down: KbmInput::Key(ffi::SDLK_DOWN),
            dpad_left: KbmInput::Key(ffi::SDLK_LEFT),
            dpad_right: KbmInput::Key(ffi::SDLK_RIGHT),
            touchpad: KbmInput::Key(ffi::SDLK_TAB),
            options: KbmInput::Key(ffi::SDLK_RETURN),
            share: KbmInput::Key(ffi::SDLK_BACKSPACE),
            ps: KbmInput::Key(ffi::SDLK_P),
        }
    }
}

impl KbmBindings {
    /// Returns a concise multi-line summary of the active keyboard and mouse bindings.
    pub fn summary_tooltip(&self) -> String {
        format!(
            "Virtual DualSense via Keyboard + Mouse (F8 locks/unlocks cursor)\n\
             Move: {}/{}/{}/{}  |  Look: Mouse\n\
             R2/L2: {} / {}  |  R1/L1: {} / {}  |  R3/L3: {} / {}\n\
             Cross/Circle/Square/Triangle: {} / {} / {} / {}\n\
             D-Pad: {}/{}/{}/{}  |  Touchpad: {}  |  Options/Share/PS: {} / {} / {}",
            self.move_up.label(),
            self.move_left.label(),
            self.move_down.label(),
            self.move_right.label(),
            self.r2.label(),
            self.l2.label(),
            self.r1.label(),
            self.l1.label(),
            self.r3.label(),
            self.l3.label(),
            self.cross.label(),
            self.circle.label(),
            self.square.label(),
            self.triangle.label(),
            self.dpad_up.label(),
            self.dpad_down.label(),
            self.dpad_left.label(),
            self.dpad_right.label(),
            self.touchpad.label(),
            self.options.label(),
            self.share.label(),
            self.ps.label(),
        )
    }
}

/// Tuning parameters controlling mouse-to-right-stick response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KbmConfig {
    pub sensitivity_pct: u8,
    pub deadzone_pct: u8,
    pub invert_y: bool,
}

impl Default for KbmConfig {
    fn default() -> Self {
        Self {
            sensitivity_pct: 50,
            deadzone_pct: 14,
            invert_y: false,
        }
    }
}

/// Stateful keyboard and mouse emulator outputting virtual DualSense `ControllerState`s.
#[derive(Debug, Default)]
pub struct KbmEmulator {
    pressed_keys: HashSet<u32>,
    pressed_mouse: HashSet<u8>,
    accum_dx: f32,
    accum_dy: f32,
    vel_x: f32,
    vel_y: f32,
    last_sample: Option<Instant>,
}

impl KbmEmulator {
    /// Updates pressed key tracking.
    pub fn on_key(&mut self, keycode: u32, pressed: bool) {
        if pressed {
            self.pressed_keys.insert(keycode);
        } else {
            self.pressed_keys.remove(&keycode);
        }
    }

    /// Updates pressed mouse button tracking.
    pub fn on_mouse_button(&mut self, button: u8, pressed: bool) {
        if pressed {
            self.pressed_mouse.insert(button);
        } else {
            self.pressed_mouse.remove(&button);
        }
    }

    /// Accumulates relative mouse motion since the last controller sample tick.
    pub fn on_mouse_motion(&mut self, dx: f32, dy: f32) {
        if dx.is_finite() {
            self.accum_dx += dx;
        }
        if dy.is_finite() {
            self.accum_dy += dy;
        }
    }

    /// Releases all held virtual inputs and zeroes mouse smoothing state.
    pub fn clear_state(&mut self) {
        self.pressed_keys.clear();
        self.pressed_mouse.clear();
        self.accum_dx = 0.0;
        self.accum_dy = 0.0;
        self.vel_x = 0.0;
        self.vel_y = 0.0;
        self.last_sample = None;
    }

    fn is_pressed(&self, input: KbmInput) -> bool {
        match input {
            KbmInput::Key(code) => self.pressed_keys.contains(&code),
            KbmInput::MouseButton(btn) => self.pressed_mouse.contains(&btn),
        }
    }

    /// Samples current keyboard/mouse state and advances smoothed mouse-to-stick emulation.
    pub fn sample_state(&mut self, config: KbmConfig, bindings: &KbmBindings) -> ControllerState {
        let now = Instant::now();
        let dt_secs = self
            .last_sample
            .map(|prev| now.duration_since(prev).as_secs_f32().clamp(0.0005, 0.1))
            .unwrap_or(0.004);
        self.last_sample = Some(now);

        let raw_dx = std::mem::take(&mut self.accum_dx);
        let raw_dy = std::mem::take(&mut self.accum_dy);
        let mut vel = (self.vel_x, self.vel_y);
        let (right_x, right_y) = step_mouse_stick(&mut vel, (raw_dx, raw_dy), dt_secs, config);
        self.vel_x = vel.0;
        self.vel_y = vel.1;

        let lx = f32::from(i8::from(self.is_pressed(bindings.move_right)))
            - f32::from(i8::from(self.is_pressed(bindings.move_left)));
        let ly = f32::from(i8::from(self.is_pressed(bindings.move_down)))
            - f32::from(i8::from(self.is_pressed(bindings.move_up)));
        let (left_x, left_y) = normalize_digital_stick(lx, ly);

        let mut btn_mask: u32 = 0;
        let digital_map = [
            (bindings.cross, buttons::CROSS),
            (bindings.circle, buttons::CIRCLE),
            (bindings.square, buttons::SQUARE),
            (bindings.triangle, buttons::TRIANGLE),
            (bindings.l1, buttons::L1),
            (bindings.r1, buttons::R1),
            (bindings.l3, buttons::L3),
            (bindings.r3, buttons::R3),
            (bindings.dpad_up, buttons::DPAD_UP),
            (bindings.dpad_down, buttons::DPAD_DOWN),
            (bindings.dpad_left, buttons::DPAD_LEFT),
            (bindings.dpad_right, buttons::DPAD_RIGHT),
            (bindings.touchpad, buttons::TOUCHPAD),
            (bindings.options, buttons::OPTIONS),
            (bindings.share, buttons::SHARE),
            (bindings.ps, buttons::PS),
        ];
        for (binding, flag) in digital_map {
            if self.is_pressed(binding) {
                btn_mask |= flag;
            }
        }

        ControllerState {
            buttons: btn_mask,
            l2: if self.is_pressed(bindings.l2) { 255 } else { 0 },
            r2: if self.is_pressed(bindings.r2) { 255 } else { 0 },
            left_x,
            left_y,
            right_x,
            right_y,
            ..Default::default()
        }
    }
}

/// Integrates raw relative mouse deltas into a smoothed, deadzone-boosted right stick value.
pub fn step_mouse_stick(
    vel: &mut (f32, f32),
    raw_delta: (f32, f32),
    dt_secs: f32,
    config: KbmConfig,
) -> (i16, i16) {
    let dt = dt_secs.clamp(0.0005, 0.1);
    let decay = (-dt / MOUSE_DECAY_TAU_SECS).exp();
    let sens = (f32::from(config.sensitivity_pct.clamp(1, 100)) / 50.0).clamp(0.02, 4.0);

    vel.0 = vel.0 * decay + raw_delta.0 * sens * MOUSE_BASE_SCALE;
    vel.1 = vel.1 * decay + raw_delta.1 * sens * MOUSE_BASE_SCALE;

    let mag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
    if !mag.is_finite() || mag < MOUSE_VEL_CUTOFF {
        if mag < MOUSE_VEL_ZERO_SNAP {
            *vel = (0.0, 0.0);
        }
        return (0, 0);
    }

    let clamped = mag.min(1.0);
    let dz = f32::from(config.deadzone_pct.min(40)) / 100.0;
    let out_mag = (dz + clamped.powf(MOUSE_RESPONSE_GAMMA) * (1.0 - dz)).clamp(0.0, 1.0);
    let nx = vel.0 / mag;
    let ny = (vel.1 / mag) * if config.invert_y { -1.0 } else { 1.0 };

    let rx = (nx * out_mag * 32767.0).round().clamp(-32767.0, 32767.0) as i16;
    let ry = (ny * out_mag * 32767.0).round().clamp(-32767.0, 32767.0) as i16;
    (rx, ry)
}

/// Merges a physical gamepad state and virtual KB+M state so both can act simultaneously.
pub fn merge_controller_states(
    primary: &ControllerState,
    secondary: &ControllerState,
) -> ControllerState {
    let left_p = i64::from(primary.left_x).pow(2) + i64::from(primary.left_y).pow(2);
    let left_s = i64::from(secondary.left_x).pow(2) + i64::from(secondary.left_y).pow(2);
    let (left_x, left_y) = if left_s > left_p {
        (secondary.left_x, secondary.left_y)
    } else {
        (primary.left_x, primary.left_y)
    };

    let right_p = i64::from(primary.right_x).pow(2) + i64::from(primary.right_y).pow(2);
    let right_s = i64::from(secondary.right_x).pow(2) + i64::from(secondary.right_y).pow(2);
    let (right_x, right_y) = if right_s > right_p {
        (secondary.right_x, secondary.right_y)
    } else {
        (primary.right_x, primary.right_y)
    };

    ControllerState {
        buttons: primary.buttons | secondary.buttons,
        l2: primary.l2.max(secondary.l2),
        r2: primary.r2.max(secondary.r2),
        left_x,
        left_y,
        right_x,
        right_y,
        touches: primary.touches,
        gyro: primary.gyro,
        accel: primary.accel,
        orient: primary.orient,
    }
}

fn normalize_digital_stick(x: f32, y: f32) -> (i16, i16) {
    if x == 0.0 && y == 0.0 {
        return (0, 0);
    }
    let len = (x * x + y * y).sqrt();
    let nx = x / len;
    let ny = y / len;
    (
        (nx * 32767.0).round().clamp(-32767.0, 32767.0) as i16,
        (ny * 32767.0).round().clamp(-32767.0, 32767.0) as i16,
    )
}

fn keycode_label(code: u32) -> String {
    match code {
        ffi::SDLK_W => "W".to_string(),
        ffi::SDLK_A => "A".to_string(),
        ffi::SDLK_S => "S".to_string(),
        ffi::SDLK_D => "D".to_string(),
        ffi::SDLK_Q => "Q".to_string(),
        ffi::SDLK_E => "E".to_string(),
        ffi::SDLK_R => "R".to_string(),
        ffi::SDLK_F => "F".to_string(),
        ffi::SDLK_C => "C".to_string(),
        ffi::SDLK_V => "V".to_string(),
        ffi::SDLK_P => "P".to_string(),
        ffi::SDLK_SPACE => "Space".to_string(),
        ffi::SDLK_LSHIFT => "LShift".to_string(),
        ffi::SDLK_RSHIFT => "RShift".to_string(),
        ffi::SDLK_LCTRL => "LCtrl".to_string(),
        ffi::SDLK_RCTRL => "RCtrl".to_string(),
        ffi::SDLK_LALT => "LAlt".to_string(),
        ffi::SDLK_RALT => "RAlt".to_string(),
        ffi::SDLK_TAB => "Tab".to_string(),
        ffi::SDLK_RETURN | ffi::SDLK_KP_ENTER => "Enter".to_string(),
        ffi::SDLK_BACKSPACE => "Backspace".to_string(),
        ffi::SDLK_UP => "Up".to_string(),
        ffi::SDLK_DOWN => "Down".to_string(),
        ffi::SDLK_LEFT => "Left".to_string(),
        ffi::SDLK_RIGHT => "Right".to_string(),
        c if (ffi::SDLK_A..=ffi::SDLK_Z).contains(&c) => {
            char::from((b'A') + (c - ffi::SDLK_A) as u8).to_string()
        }
        c if (ffi::SDLK_0..=ffi::SDLK_9).contains(&c) => {
            char::from((b'0') + (c - ffi::SDLK_0) as u8).to_string()
        }
        c if (ffi::SDLK_F1..=ffi::SDLK_F12).contains(&c) => {
            format!("F{}", 1 + (c - ffi::SDLK_F1))
        }
        other => format!("Key({other:#x})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_wasd_cardinal_diagonal_buttons_and_triggers() {
        let bindings = KbmBindings::default();
        let config = KbmConfig::default();
        let mut emu = KbmEmulator::default();

        // Cardinal Up (W) + Square (R) + R2 (LMB).
        emu.on_key(ffi::SDLK_W, true);
        emu.on_key(ffi::SDLK_R, true);
        emu.on_mouse_button(1, true);
        let up_state = emu.sample_state(config, &bindings);
        assert_eq!((up_state.left_x, up_state.left_y), (0, -32767));
        assert_eq!(up_state.r2, 255);
        assert_eq!(up_state.l2, 0);
        assert_ne!(up_state.buttons & buttons::SQUARE, 0);

        // Add Right (D) -> normalized diagonal (~23170, -23170).
        emu.on_key(ffi::SDLK_D, true);
        emu.on_key(ffi::SDLK_R, false);
        emu.on_mouse_button(1, false);
        emu.on_mouse_button(3, true);
        let diag_state = emu.sample_state(config, &bindings);
        assert!((diag_state.left_x - 23170).abs() <= 2);
        assert!((diag_state.left_y + 23170).abs() <= 2);
        assert_eq!(diag_state.r2, 0);
        assert_eq!(diag_state.l2, 255);

        emu.clear_state();
        let cleared = emu.sample_state(config, &bindings);
        assert_eq!(
            (
                cleared.left_x,
                cleared.left_y,
                cleared.buttons,
                cleared.l2,
                cleared.r2
            ),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn mouse_stick_applies_deadzone_boost_invert_y_and_decays_to_zero() {
        let mut vel = (0.0, 0.0);
        let cfg = KbmConfig {
            sensitivity_pct: 50,
            deadzone_pct: 20,
            invert_y: false,
        };
        let (rx, ry) = step_mouse_stick(&mut vel, (10.0, -5.0), 0.016, cfg);
        // Magnitude must exceed the 20% deadzone floor (6553) and point +X, -Y.
        let mag = ((rx as f32).powi(2) + (ry as f32).powi(2)).sqrt();
        assert!(mag >= 6553.0, "expected deadzone boost >= 6553, got {mag}");
        assert!(rx > 0 && ry < 0);

        // Inverted Y flips vertical output sign while horizontal stays positive.
        let mut vel_inv = (0.0, 0.0);
        let cfg_inv = KbmConfig {
            invert_y: true,
            ..cfg
        };
        let (rx_inv, ry_inv) = step_mouse_stick(&mut vel_inv, (10.0, -5.0), 0.016, cfg_inv);
        assert_eq!(rx_inv, rx);
        assert_eq!(ry_inv, -ry);

        // Idle frames decay smoothly back to exact (0, 0).
        let mut final_stick = (rx, ry);
        for _ in 0..30 {
            final_stick = step_mouse_stick(&mut vel, (0.0, 0.0), 0.016, cfg);
        }
        assert_eq!(final_stick, (0, 0));
    }

    #[test]
    fn merges_gamepad_and_kbm_without_dropping_active_axes() {
        let pad = ControllerState {
            buttons: buttons::CROSS,
            l2: 200,
            r2: 0,
            left_x: -12_000,
            left_y: 4_000,
            right_x: 0,
            right_y: 0,
            ..Default::default()
        };
        let kbm = ControllerState {
            buttons: buttons::R1,
            l2: 0,
            r2: 255,
            left_x: 0,
            left_y: 0,
            right_x: 18_000,
            right_y: -9_000,
            ..Default::default()
        };

        let merged = merge_controller_states(&pad, &kbm);
        assert_eq!(merged.buttons, buttons::CROSS | buttons::R1);
        assert_eq!((merged.l2, merged.r2), (200, 255));
        assert_eq!((merged.left_x, merged.left_y), (-12_000, 4_000));
        assert_eq!((merged.right_x, merged.right_y), (18_000, -9_000));
    }
}
