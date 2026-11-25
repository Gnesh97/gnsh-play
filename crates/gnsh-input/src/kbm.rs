use crate::ffi;
use gnsh_proto::controller::{ControllerState, buttons};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Instant;

const MOUSE_MIN_SAMPLE_WINDOW_SECS: f32 = 0.008;
const MOUSE_REF_FRAME_SECS: f32 = 1.0 / 60.0;
const MOUSE_ACTIVE_SMOOTH_TAU_SECS: f32 = 0.005;
const MOUSE_STOP_DECAY_TAU_SECS: f32 = 0.0035;
const MOUSE_BASE_SCALE: f32 = 1.0 / 30.0;
const MOUSE_MAX_FLICK_VEL: f32 = 3.4;
const MOUSE_FLICK_DRAIN_RATE: f32 = 0.60;
const MOUSE_VEL_CUTOFF: f32 = 0.008;
const MOUSE_VEL_ZERO_SNAP: f32 = 0.002;
const MOUSE_RESPONSE_GAMMA: f32 = 1.15;

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

/// Bindable DualSense actions exposed in the KB+M keybind editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KbmAction {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    L2,
    R2,
    L1,
    R1,
    L3,
    R3,
    Cross,
    Circle,
    Square,
    Triangle,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Touchpad,
    Options,
    Share,
    Ps,
}

impl KbmAction {
    /// Complete ordered list of all bindable DualSense actions.
    pub const ALL: [KbmAction; 22] = [
        Self::MoveUp,
        Self::MoveDown,
        Self::MoveLeft,
        Self::MoveRight,
        Self::L2,
        Self::R2,
        Self::L1,
        Self::R1,
        Self::L3,
        Self::R3,
        Self::Cross,
        Self::Circle,
        Self::Square,
        Self::Triangle,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
        Self::Touchpad,
        Self::Options,
        Self::Share,
        Self::Ps,
    ];

    /// Human-readable display label for the keybind UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::MoveUp => "Left Stick Up",
            Self::MoveDown => "Left Stick Down",
            Self::MoveLeft => "Left Stick Left",
            Self::MoveRight => "Left Stick Right",
            Self::L2 => "L2 (Aim / Brake)",
            Self::R2 => "R2 (Fire / Gas)",
            Self::L1 => "L1",
            Self::R1 => "R1",
            Self::L3 => "L3 (Sprint)",
            Self::R3 => "R3 (Melee)",
            Self::Cross => "Cross (X)",
            Self::Circle => "Circle (O)",
            Self::Square => "Square ([])",
            Self::Triangle => "Triangle (/\\)",
            Self::DpadUp => "D-Pad Up",
            Self::DpadDown => "D-Pad Down",
            Self::DpadLeft => "D-Pad Left",
            Self::DpadRight => "D-Pad Right",
            Self::Touchpad => "Touchpad Click",
            Self::Options => "Options",
            Self::Share => "Create / Share",
            Self::Ps => "PS Button",
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
    fn slot_mut(&mut self, action: KbmAction) -> &mut KbmInput {
        match action {
            KbmAction::MoveUp => &mut self.move_up,
            KbmAction::MoveDown => &mut self.move_down,
            KbmAction::MoveLeft => &mut self.move_left,
            KbmAction::MoveRight => &mut self.move_right,
            KbmAction::L2 => &mut self.l2,
            KbmAction::R2 => &mut self.r2,
            KbmAction::L1 => &mut self.l1,
            KbmAction::R1 => &mut self.r1,
            KbmAction::L3 => &mut self.l3,
            KbmAction::R3 => &mut self.r3,
            KbmAction::Cross => &mut self.cross,
            KbmAction::Circle => &mut self.circle,
            KbmAction::Square => &mut self.square,
            KbmAction::Triangle => &mut self.triangle,
            KbmAction::DpadUp => &mut self.dpad_up,
            KbmAction::DpadDown => &mut self.dpad_down,
            KbmAction::DpadLeft => &mut self.dpad_left,
            KbmAction::DpadRight => &mut self.dpad_right,
            KbmAction::Touchpad => &mut self.touchpad,
            KbmAction::Options => &mut self.options,
            KbmAction::Share => &mut self.share,
            KbmAction::Ps => &mut self.ps,
        }
    }

    /// Returns the input currently bound to `action`.
    pub fn get(&self, action: KbmAction) -> KbmInput {
        match action {
            KbmAction::MoveUp => self.move_up,
            KbmAction::MoveDown => self.move_down,
            KbmAction::MoveLeft => self.move_left,
            KbmAction::MoveRight => self.move_right,
            KbmAction::L2 => self.l2,
            KbmAction::R2 => self.r2,
            KbmAction::L1 => self.l1,
            KbmAction::R1 => self.r1,
            KbmAction::L3 => self.l3,
            KbmAction::R3 => self.r3,
            KbmAction::Cross => self.cross,
            KbmAction::Circle => self.circle,
            KbmAction::Square => self.square,
            KbmAction::Triangle => self.triangle,
            KbmAction::DpadUp => self.dpad_up,
            KbmAction::DpadDown => self.dpad_down,
            KbmAction::DpadLeft => self.dpad_left,
            KbmAction::DpadRight => self.dpad_right,
            KbmAction::Touchpad => self.touchpad,
            KbmAction::Options => self.options,
            KbmAction::Share => self.share,
            KbmAction::Ps => self.ps,
        }
    }

    /// Assigns `new_input` to `action`, automatically swapping if another action already uses it.
    pub fn set(&mut self, action: KbmAction, new_input: KbmInput) {
        let prev_input = self.get(action);
        if prev_input == new_input {
            return;
        }
        for other in KbmAction::ALL {
            if other != action && self.get(other) == new_input {
                *self.slot_mut(other) = prev_input;
                break;
            }
        }
        *self.slot_mut(action) = new_input;
    }

    /// Returns `true` for window/stream shortcuts reserved against rebinding.
    pub fn is_reserved_shortcut(input: KbmInput) -> bool {
        matches!(
            input,
            KbmInput::Key(
                ffi::SDLK_ESCAPE
                    | ffi::SDLK_F3
                    | ffi::SDLK_F8
                    | ffi::SDLK_F9
                    | ffi::SDLK_F10
                    | ffi::SDLK_F11
                    | ffi::SDLK_F12
            )
        )
    }

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
    pub curve_pct: u8,
    pub invert_y: bool,
}

impl Default for KbmConfig {
    fn default() -> Self {
        Self {
            sensitivity_pct: 50,
            deadzone_pct: 14,
            curve_pct: 100,
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
    last_right_stick: (i16, i16),
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
        self.last_right_stick = (0, 0);
        self.last_sample = None;
    }

    fn is_pressed(&self, input: KbmInput) -> bool {
        match input {
            KbmInput::Key(code) => self.pressed_keys.contains(&code),
            KbmInput::MouseButton(btn) => self.pressed_mouse.contains(&btn),
        }
    }

    /// Samples current keyboard/mouse state and advances mouse-to-stick emulation.
    pub fn sample_state(&mut self, config: KbmConfig, bindings: &KbmBindings) -> ControllerState {
        let now = Instant::now();
        let sample_dt = match self.last_sample {
            None => Some(MOUSE_MIN_SAMPLE_WINDOW_SECS),
            Some(prev) => {
                let elapsed = now.duration_since(prev).as_secs_f32();
                (elapsed >= MOUSE_MIN_SAMPLE_WINDOW_SECS)
                    .then(|| elapsed.clamp(MOUSE_MIN_SAMPLE_WINDOW_SECS, 0.1))
            }
        };
        let (right_x, right_y) = if let Some(dt_secs) = sample_dt {
            self.last_sample = Some(now);
            let raw_dx = std::mem::take(&mut self.accum_dx);
            let raw_dy = std::mem::take(&mut self.accum_dy);
            let mut vel = (self.vel_x, self.vel_y);
            let stick = step_mouse_stick(&mut vel, (raw_dx, raw_dy), dt_secs, config);
            self.vel_x = vel.0;
            self.vel_y = vel.1;
            self.last_right_stick = stick;
            stick
        } else {
            self.last_right_stick
        };

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

/// Converts raw relative mouse deltas into a `dt`-normalized, progressive-curve,
/// deadzone-boosted right stick value with bounded fast-flick carryover.
pub fn step_mouse_stick(
    vel: &mut (f32, f32),
    raw_delta: (f32, f32),
    dt_secs: f32,
    config: KbmConfig,
) -> (i16, i16) {
    let dt = dt_secs.clamp(0.001, 0.1);
    let norm_dt = (dt / MOUSE_REF_FRAME_SECS).clamp(0.1, 4.0);
    let sens = (f32::from(config.sensitivity_pct.clamp(1, 100)) / 50.0)
        .powf(1.35)
        .clamp(0.02, 6.0);

    let inst_x = (raw_delta.0 * sens * MOUSE_BASE_SCALE) / norm_dt;
    let inst_y = (raw_delta.1 * sens * MOUSE_BASE_SCALE) / norm_dt;
    let inst_mag = (inst_x * inst_x + inst_y * inst_y).sqrt();

    if raw_delta.0.abs() > 1e-5 && inst_x * vel.0 < 0.0 {
        vel.0 = 0.0;
    }
    if raw_delta.1.abs() > 1e-5 && inst_y * vel.1 < 0.0 {
        vel.1 = 0.0;
    }

    if inst_mag > 1.0 {
        // Carry unconsumed fast-flick displacement above full stick deflection (capped).
        let prev_mag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt().max(1.0);
        let excess = (inst_mag - 1.0) * norm_dt;
        let new_mag = (prev_mag + excess).min(MOUSE_MAX_FLICK_VEL);
        let ux = inst_x / inst_mag;
        let uy = inst_y / inst_mag;
        vel.0 = ux * new_mag;
        vel.1 = uy * new_mag;
    } else if inst_mag > 1e-5 {
        let prev_mag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
        if prev_mag > 1.0 {
            let drain = MOUSE_FLICK_DRAIN_RATE * (1.0 - 0.5 * inst_mag) * norm_dt;
            let next_mag = (prev_mag - drain).max(inst_mag);
            let ux = inst_x / inst_mag;
            let uy = inst_y / inst_mag;
            vel.0 = ux * next_mag;
            vel.1 = uy * next_mag;
        } else {
            let alpha = 1.0 - (-dt / MOUSE_ACTIVE_SMOOTH_TAU_SECS).exp();
            vel.0 = vel.0 * (1.0 - alpha) + inst_x * alpha;
            vel.1 = vel.1 * (1.0 - alpha) + inst_y * alpha;
        }
    } else {
        // Shed excess carryover immediately once hand stops so crosshair never overshoots.
        let prev_mag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
        if prev_mag > 1.0 {
            let inv = 1.0 / prev_mag;
            vel.0 *= inv;
            vel.1 *= inv;
        }
        let decay = (-dt / MOUSE_STOP_DECAY_TAU_SECS).exp();
        vel.0 *= decay;
        vel.1 *= decay;
    }

    let mag = (vel.0 * vel.0 + vel.1 * vel.1).sqrt();
    if !mag.is_finite() || mag < MOUSE_VEL_CUTOFF {
        if !mag.is_finite() || mag < MOUSE_VEL_ZERO_SNAP {
            *vel = (0.0, 0.0);
        }
        return (0, 0);
    }

    let clamped = mag.min(1.0);
    let dz = f32::from(config.deadzone_pct.min(40)) / 100.0;
    let raw_curve = if config.curve_pct == 0 {
        100
    } else {
        config.curve_pct.clamp(50, 150)
    };
    let gamma = (MOUSE_RESPONSE_GAMMA * (f32::from(raw_curve) / 100.0).powf(1.2)).clamp(0.45, 2.20);
    let out_mag = (dz + clamped.powf(gamma) * (1.0 - dz)).clamp(0.0, 1.0);
    let nx = vel.0 / mag;
    let ny = (vel.1 / mag) * if config.invert_y { -1.0 } else { 1.0 };

    let mut out_x = nx * out_mag;
    let mut out_y = ny * out_mag;
    if dz > 0.0 && nx.abs() > 0.02 && ny.abs() > 0.02 {
        // Lift minor axis gently above console axial deadzones to keep shallow diagonals smooth.
        let axial_floor = dz * 0.35;
        let lift_axis = |val: f32, dir: f32| -> f32 {
            let blend = ((dir.abs() - 0.02) / 0.18).clamp(0.0, 1.0);
            let min_v = axial_floor * blend;
            if val.abs() < min_v {
                min_v.copysign(val)
            } else {
                val
            }
        };
        out_x = lift_axis(out_x, nx);
        out_y = lift_axis(out_y, ny);
        let norm = (out_x * out_x + out_y * out_y).sqrt();
        if norm > 1.0 {
            out_x /= norm;
            out_y /= norm;
        }
    }

    let rx = (out_x * 32767.0).round().clamp(-32767.0, 32767.0) as i16;
    let ry = (out_y * 32767.0).round().clamp(-32767.0, 32767.0) as i16;
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
            curve_pct: 100,
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
        for _ in 0..4 {
            final_stick = step_mouse_stick(&mut vel, (0.0, 0.0), 0.016, cfg);
        }
        assert_eq!(final_stick, (0, 0));
    }

    #[test]
    fn mouse_stick_progressive_curve_and_flick_carryover() {
        let cfg = KbmConfig {
            sensitivity_pct: 50,
            deadzone_pct: 15,
            curve_pct: 100,
            invert_y: false,
        };

        // Equal physical speed (500 px/s) across 2 x 8 ms vs 1 x 16 ms produces matching output.
        let mut vel_8ms = (0.0, 0.0);
        step_mouse_stick(&mut vel_8ms, (4.0, 0.0), 0.008, cfg);
        let (rx_8ms, _) = step_mouse_stick(&mut vel_8ms, (4.0, 0.0), 0.008, cfg);
        let mut vel_16ms = (0.0, 0.0);
        let (rx_16ms, _) = step_mouse_stick(&mut vel_16ms, (8.0, 0.0), 0.016, cfg);
        assert!(
            (rx_8ms - rx_16ms).abs() <= 2,
            "8ms ({rx_8ms}) and 16ms ({rx_16ms}) windows should match for equal speed"
        );

        // Slow micro-aim stays gentle while fast turns reach full stick deflection.
        let mut vel_slow_aim = (0.0, 0.0);
        let (rx_slow_aim, _) = step_mouse_stick(&mut vel_slow_aim, (2.0, 0.0), 0.016, cfg);
        assert!(
            rx_slow_aim > 0 && rx_slow_aim < 10_000,
            "slow aim should stay gentle (< 10000), got {rx_slow_aim}"
        );
        let mut vel_fast_turn = (0.0, 0.0);
        let (rx_fast_turn, _) = step_mouse_stick(&mut vel_fast_turn, (35.0, 0.0), 0.016, cfg);
        assert_eq!(rx_fast_turn, 32_767);

        // Fast flick holds 100% carryover while still moving, then stops cleanly within 24 ms on idle.
        let mut vel_flick = (0.0, 0.0);
        let (rx_flick, _) = step_mouse_stick(&mut vel_flick, (80.0, 0.0), 0.008, cfg);
        assert_eq!(rx_flick, 32_767);
        let (rx_follow, _) = step_mouse_stick(&mut vel_flick, (2.0, 0.0), 0.008, cfg);
        assert_eq!(rx_follow, 32_767);
        let mut stick = (i16::MAX, 0);
        for _ in 0..3 {
            stick = step_mouse_stick(&mut vel_flick, (0.0, 0.0), 0.008, cfg);
        }
        assert_eq!(stick, (0, 0));

        // Lower curve_pct boosts mid-speed output relative to higher curve_pct.
        let mut vel_low_curve = (0.0, 0.0);
        let (rx_low_curve, _) = step_mouse_stick(
            &mut vel_low_curve,
            (6.0, 0.0),
            0.016,
            KbmConfig {
                curve_pct: 60,
                ..cfg
            },
        );
        let mut vel_high_curve = (0.0, 0.0);
        let (rx_high_curve, _) = step_mouse_stick(
            &mut vel_high_curve,
            (6.0, 0.0),
            0.016,
            KbmConfig {
                curve_pct: 140,
                ..cfg
            },
        );
        assert!(
            rx_low_curve > rx_high_curve,
            "expected curve 60% ({rx_low_curve}) > curve 140% ({rx_high_curve})"
        );

        // Opposite mouse movement during active flick carryover reverses sign immediately.
        let mut vel_rev = (0.0, 0.0);
        step_mouse_stick(&mut vel_rev, (80.0, 0.0), 0.008, cfg);
        let (rx_rev, _) = step_mouse_stick(&mut vel_rev, (-4.0, 0.0), 0.008, cfg);
        assert!(
            rx_rev < 0,
            "expected immediate negative reversal during carryover, got {rx_rev}"
        );
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

    #[test]
    fn rebinding_covers_all_actions_swaps_duplicates_and_respects_reserved_keys() {
        let mut bindings = KbmBindings::default();
        for action in KbmAction::ALL {
            let current = bindings.get(action);
            bindings.set(action, current);
            assert!(!KbmAction::label(action).is_empty());
        }

        let orig_cross = bindings.get(KbmAction::Cross);
        let orig_square = bindings.get(KbmAction::Square);
        assert_ne!(orig_cross, orig_square);

        // Assigning Square's key to Cross must swap them without dropping either binding.
        bindings.set(KbmAction::Cross, orig_square);
        assert_eq!(bindings.get(KbmAction::Cross), orig_square);
        assert_eq!(bindings.get(KbmAction::Square), orig_cross);

        assert!(KbmBindings::is_reserved_shortcut(KbmInput::Key(
            ffi::SDLK_ESCAPE
        )));
        assert!(KbmBindings::is_reserved_shortcut(KbmInput::Key(
            ffi::SDLK_F3
        )));
        assert!(KbmBindings::is_reserved_shortcut(KbmInput::Key(
            ffi::SDLK_F8
        )));
        assert!(KbmBindings::is_reserved_shortcut(KbmInput::Key(
            ffi::SDLK_F11
        )));
        assert!(!KbmBindings::is_reserved_shortcut(KbmInput::Key(
            ffi::SDLK_E
        )));
        assert!(!KbmBindings::is_reserved_shortcut(KbmInput::MouseButton(1)));
    }
}
