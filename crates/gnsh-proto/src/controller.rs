use gnsh_chiaki_sys::{
    ChiakiControllerState, ChiakiControllerTouch,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_BOX,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_CROSS,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_DOWN,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_LEFT,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_RIGHT,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_UP,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_L1,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_L3,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_MOON,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_OPTIONS,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_PS,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_PYRAMID,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_R1,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_R3,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_SHARE,
    chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_TOUCHPAD,
};

pub mod buttons {
    use super::*;

    pub const CROSS: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_CROSS;
    pub const CIRCLE: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_MOON;
    pub const SQUARE: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_BOX;
    pub const TRIANGLE: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_PYRAMID;
    pub const DPAD_LEFT: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_LEFT;
    pub const DPAD_RIGHT: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_RIGHT;
    pub const DPAD_UP: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_UP;
    pub const DPAD_DOWN: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_DPAD_DOWN;
    pub const L1: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_L1;
    pub const R1: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_R1;
    pub const L3: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_L3;
    pub const R3: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_R3;
    pub const OPTIONS: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_OPTIONS;
    pub const SHARE: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_SHARE;
    pub const TOUCHPAD: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_TOUCHPAD;
    pub const PS: u32 = chiaki_controller_button_t_CHIAKI_CONTROLLER_BUTTON_PS;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPoint {
    pub id: i8,
    pub x: u16,
    pub y: u16,
}

impl Default for TouchPoint {
    fn default() -> Self {
        Self { id: -1, x: 0, y: 0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ControllerState {
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    pub touches: [TouchPoint; 2],
    pub gyro: [f32; 3],
    pub accel: [f32; 3],
    pub orient: [f32; 4],
}

impl Default for ControllerState {
    fn default() -> Self {
        Self {
            buttons: 0,
            l2: 0,
            r2: 0,
            left_x: 0,
            left_y: 0,
            right_x: 0,
            right_y: 0,
            touches: [TouchPoint::default(); 2],
            gyro: [0.0; 3],
            accel: [0.0, 1.0, 0.0],
            orient: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

impl ControllerState {
    pub fn to_chiaki(&self) -> ChiakiControllerState {
        ChiakiControllerState {
            buttons: self.buttons,
            l2_state: self.l2,
            r2_state: self.r2,
            left_x: self.left_x,
            left_y: self.left_y,
            right_x: self.right_x,
            right_y: self.right_y,
            touch_id_next: 0,
            touches: [
                ChiakiControllerTouch {
                    x: self.touches[0].x,
                    y: self.touches[0].y,
                    id: self.touches[0].id,
                },
                ChiakiControllerTouch {
                    x: self.touches[1].x,
                    y: self.touches[1].y,
                    id: self.touches[1].id,
                },
            ],
            gyro_x: self.gyro[0],
            gyro_y: self.gyro[1],
            gyro_z: self.gyro[2],
            accel_x: self.accel[0],
            accel_y: self.accel[1],
            accel_z: self.accel[2],
            orient_x: self.orient[0],
            orient_y: self.orient[1],
            orient_z: self.orient[2],
            orient_w: self.orient[3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_idle_and_active_controller_state() {
        let idle = ControllerState::default().to_chiaki();
        assert_eq!(idle.buttons, 0);
        assert_eq!(idle.touches[0].id, -1);
        assert_eq!(idle.touches[1].id, -1);
        assert_eq!(idle.orient_w, 1.0);

        let active = ControllerState {
            buttons: buttons::CROSS | buttons::OPTIONS,
            l2: 128,
            r2: 255,
            left_x: -32768,
            left_y: 32767,
            right_x: 1000,
            right_y: -1000,
            touches: [
                TouchPoint {
                    id: 1,
                    x: 960,
                    y: 540,
                },
                TouchPoint::default(),
            ],
            ..Default::default()
        }
        .to_chiaki();

        assert_eq!(active.buttons, buttons::CROSS | buttons::OPTIONS);
        assert_eq!(active.l2_state, 128);
        assert_eq!(active.r2_state, 255);
        assert_eq!(active.left_x, -32768);
        assert_eq!(active.touches[0].id, 1);
        assert_eq!(active.touches[0].x, 960);
    }
}
