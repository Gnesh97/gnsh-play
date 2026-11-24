use gnsh_proto::controller::{ControllerState, buttons};
use std::time::{Duration, Instant};

const STICK_THRESHOLD: i16 = 16_000;
const REPEAT_DELAY: Duration = Duration::from_millis(220);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeNavEvent {
    PrevConsole,
    NextConsole,
    Connect,
    Wake,
    Discover,
    ToggleSettings,
    PrevSettingsTab,
    NextSettingsTab,
    Back,
}

#[derive(Debug)]
pub(crate) struct GamepadHomeNav {
    prev_buttons: u32,
    prev_stick_dir: i8,
    last_repeat: Instant,
    wait_for_release: bool,
}

impl Default for GamepadHomeNav {
    fn default() -> Self {
        Self {
            prev_buttons: 0,
            prev_stick_dir: 0,
            last_repeat: Instant::now(),
            wait_for_release: false,
        }
    }
}

fn stick_vertical_dir(left_y: i16) -> i8 {
    if left_y < -STICK_THRESHOLD {
        -1
    } else if left_y > STICK_THRESHOLD {
        1
    } else {
        0
    }
}

impl GamepadHomeNav {
    /// Suppresses Home actions after leaving a stream until held buttons and stick settle.
    pub(crate) fn arm_release_guard(&mut self) {
        self.wait_for_release = true;
        self.prev_buttons = 0;
        self.prev_stick_dir = 0;
    }

    /// Resets edge-detection state when no gamepad is connected.
    pub(crate) fn reset(&mut self) {
        self.prev_buttons = 0;
        self.prev_stick_dir = 0;
        self.wait_for_release = false;
    }

    pub(crate) fn update(&mut self, state: &ControllerState) -> Vec<HomeNavEvent> {
        self.update_at(state, Instant::now())
    }

    pub(crate) fn update_at(&mut self, state: &ControllerState, now: Instant) -> Vec<HomeNavEvent> {
        let stick_dir = stick_vertical_dir(state.left_y);

        if self.wait_for_release {
            if state.buttons == 0 && stick_dir == 0 {
                self.wait_for_release = false;
                self.prev_buttons = 0;
                self.prev_stick_dir = 0;
            }
            return Vec::new();
        }

        let mut events = Vec::new();
        let newly_pressed = state.buttons & !self.prev_buttons;
        self.prev_buttons = state.buttons;

        let button_bindings = [
            (buttons::DPAD_UP, HomeNavEvent::PrevConsole),
            (buttons::DPAD_DOWN, HomeNavEvent::NextConsole),
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

        if stick_dir != 0 {
            let should_emit = stick_dir != self.prev_stick_dir
                || now.saturating_duration_since(self.last_repeat) >= REPEAT_DELAY;
            if should_emit {
                events.push(if stick_dir < 0 {
                    HomeNavEvent::PrevConsole
                } else {
                    HomeNavEvent::NextConsole
                });
                self.last_repeat = now;
            }
        }
        self.prev_stick_dir = stick_dir;

        events
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
                    buttons::CROSS
                        | buttons::TRIANGLE
                        | buttons::SQUARE
                        | buttons::OPTIONS
                        | buttons::CIRCLE,
                    0
                ),
                t0 + Duration::from_secs(3)
            ),
            vec![
                HomeNavEvent::Connect,
                HomeNavEvent::Wake,
                HomeNavEvent::Discover,
                HomeNavEvent::ToggleSettings,
                HomeNavEvent::Back,
            ]
        );
    }

    #[test]
    fn left_stick_vertical_repeats_at_220ms_intervals() {
        let mut nav = GamepadHomeNav::default();
        let t0 = Instant::now();

        assert_eq!(
            nav.update_at(&state_with(0, -24_000), t0),
            vec![HomeNavEvent::PrevConsole]
        );
        assert!(
            nav.update_at(&state_with(0, -24_000), t0 + Duration::from_millis(120))
                .is_empty()
        );
        assert_eq!(
            nav.update_at(&state_with(0, -24_000), t0 + Duration::from_millis(220)),
            vec![HomeNavEvent::PrevConsole]
        );
        // Deadzone stops repeat cleanly.
        assert!(
            nav.update_at(&state_with(0, 5_000), t0 + Duration::from_millis(300))
                .is_empty()
        );
        assert_eq!(
            nav.update_at(&state_with(0, 20_000), t0 + Duration::from_millis(310)),
            vec![HomeNavEvent::NextConsole]
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
            nav.update_at(&state_with(0, -25_000), t0 + Duration::from_millis(300))
                .is_empty()
        );
        // Settled completely (zero buttons + centered stick) disarms guard without firing.
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
}
