#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all
)]
pub(crate) mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub mod gamepad;
pub mod kbm;
pub mod window;

pub use gamepad::{GamepadManager, apply_stick_deadzone, map_sdl_buttons, scale_trigger_u8};
pub use kbm::{KbmAction, KbmBindings, KbmConfig, KbmEmulator, KbmInput, merge_controller_states};
pub use window::{AppWindow, WindowEventSummary};

use std::ffi::CStr;
use std::sync::OnceLock;

static SDL_INIT: OnceLock<Result<(), String>> = OnceLock::new();

/// Initializes SDL3 video and gamepad subsystems once per process.
pub fn init_sdl() -> anyhow::Result<()> {
    let res = SDL_INIT.get_or_init(|| unsafe {
        if ffi::SDL_InitSubSystem(ffi::GNSH_SDL_INIT_VIDEO | ffi::GNSH_SDL_INIT_GAMEPAD) {
            Ok(())
        } else {
            Err(sdl_error())
        }
    });
    match res {
        Ok(()) => Ok(()),
        Err(err) => Err(anyhow::anyhow!("SDL_InitSubSystem failed: {err}")),
    }
}

pub(crate) fn sdl_error() -> String {
    unsafe {
        let ptr = ffi::SDL_GetError();
        if ptr.is_null() {
            "unknown SDL error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
