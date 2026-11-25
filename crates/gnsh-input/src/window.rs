use crate::ffi;
use crate::gamepad::GamepadManager;
use crate::kbm::{KbmBindings, KbmEmulator, KbmInput};
use anyhow::Context;
use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use windows::Win32::Foundation::HWND;

/// Summary of window lifecycle and global shortcut events observed during a poll pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WindowEventSummary {
    pub quit: bool,
    pub resized: Option<(u32, u32)>,
    pub toggle_fullscreen: bool,
    pub toggle_hud: bool,
    pub toggle_mouse_lock: bool,
    pub escape_pressed: bool,
    pub toggle_mic: bool,
    pub go_home: bool,
    pub take_screenshot: bool,
    pub toggle_recording: bool,
    pub save_replay_clip: bool,
    pub audio_devices_changed: bool,
    pub last_kbm_input: Option<KbmInput>,
}

/// Desktop SDL3 window exposing a Win32 `HWND` and translating events into `egui::RawInput`.
pub struct AppWindow {
    window: *mut ffi::SDL_Window,
    hwnd: HWND,
    last_modifiers: egui::Modifiers,
    mouse_locked: bool,
}

impl AppWindow {
    /// Creates a resizable SDL3 window, starts text input, and resolves its Win32 `HWND`.
    pub fn new(title: &str, width: u32, height: u32) -> anyhow::Result<Self> {
        Self::new_with_vulkan(title, width, height, false)
    }

    /// Creates a resizable SDL3 window optionally configured for Vulkan surface creation.
    pub fn new_with_vulkan(
        title: &str,
        width: u32,
        height: u32,
        enable_vulkan: bool,
    ) -> anyhow::Result<Self> {
        crate::init_sdl()?;

        let c_title = CString::new(title).context("window title contains interior NUL")?;
        let w = width.clamp(1, i32::MAX as u32) as c_int;
        let h = height.clamp(1, i32::MAX as u32) as c_int;
        let flags = ffi::GNSH_SDL_WINDOW_RESIZABLE
            | if enable_vulkan {
                ffi::GNSH_SDL_WINDOW_VULKAN
            } else {
                0
            };

        let window = unsafe { ffi::SDL_CreateWindow(c_title.as_ptr(), w, h, flags) };
        if window.is_null() {
            anyhow::bail!("SDL_CreateWindow failed: {}", crate::sdl_error());
        }

        if !unsafe { ffi::SDL_StartTextInput(window) } {
            tracing::warn!("SDL_StartTextInput failed: {}", crate::sdl_error());
        }

        let props = unsafe { ffi::SDL_GetWindowProperties(window) };
        if props == 0 {
            let err = crate::sdl_error();
            unsafe { ffi::SDL_DestroyWindow(window) };
            anyhow::bail!("SDL_GetWindowProperties failed: {err}");
        }

        let hwnd_ptr = unsafe {
            ffi::SDL_GetPointerProperty(
                props,
                c"SDL.window.win32.hwnd".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        if hwnd_ptr.is_null() {
            let err = crate::sdl_error();
            unsafe { ffi::SDL_DestroyWindow(window) };
            anyhow::bail!("Missing Win32 HWND property on SDL window: {err}");
        }

        Ok(Self {
            window,
            hwnd: HWND(hwnd_ptr),
            last_modifiers: egui::Modifiers::default(),
            mouse_locked: false,
        })
    }

    /// Returns the native Win32 `HWND` associated with this window.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Queries the Vulkan instance extensions required by SDL for surface creation.
    pub fn vulkan_instance_extensions(&self) -> anyhow::Result<Vec<CString>> {
        let mut count: u32 = 0;
        // SAFETY: SDL returns an SDL-owned static array of C strings for `count` extensions.
        let ptr = unsafe { ffi::SDL_Vulkan_GetInstanceExtensions(&mut count) };
        if ptr.is_null() {
            anyhow::bail!(
                "SDL_Vulkan_GetInstanceExtensions failed: {}",
                crate::sdl_error()
            );
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, count as usize) };
        let mut out = Vec::with_capacity(slice.len());
        for &ext in slice {
            if !ext.is_null() {
                out.push(unsafe { CStr::from_ptr(ext) }.to_owned());
            }
        }
        Ok(out)
    }

    /// Creates a `VkSurfaceKHR` bound to this window and the supplied `VkInstance`.
    pub fn create_vulkan_surface(
        &self,
        vk_instance: *mut std::ffi::c_void,
    ) -> anyhow::Result<*mut std::ffi::c_void> {
        if vk_instance.is_null() {
            anyhow::bail!("vk_instance cannot be null");
        }
        let mut surface: ffi::VkSurfaceKHR = std::ptr::null_mut();
        // SAFETY: `self.window` was created with `SDL_WINDOW_VULKAN` and `vk_instance` is valid.
        let ok = unsafe {
            ffi::SDL_Vulkan_CreateSurface(
                self.window,
                vk_instance.cast(),
                std::ptr::null(),
                &mut surface,
            )
        };
        if !ok || surface.is_null() {
            anyhow::bail!("SDL_Vulkan_CreateSurface failed: {}", crate::sdl_error());
        }
        Ok(surface.cast())
    }

    /// Destroys a `VkSurfaceKHR` previously returned by `create_vulkan_surface`.
    pub fn destroy_vulkan_surface(
        vk_instance: *mut std::ffi::c_void,
        vk_surface: *mut std::ffi::c_void,
    ) {
        if !vk_instance.is_null() && !vk_surface.is_null() {
            // SAFETY: Matches SDL_Vulkan_CreateSurface with null custom allocator callbacks.
            unsafe {
                ffi::SDL_Vulkan_DestroySurface(
                    vk_instance.cast(),
                    vk_surface.cast(),
                    std::ptr::null(),
                );
            }
        }
    }

    /// Returns the current window client area `(width, height)` clamped to at least `(1, 1)`.
    pub fn size(&self) -> (u32, u32) {
        let mut w: c_int = 0;
        let mut h: c_int = 0;
        let ok = unsafe { ffi::SDL_GetWindowSize(self.window, &mut w, &mut h) };
        if ok {
            (w.max(1) as u32, h.max(1) as u32)
        } else {
            (1, 1)
        }
    }

    /// Toggles fullscreen mode on the SDL window.
    pub fn toggle_fullscreen(&mut self) -> anyhow::Result<()> {
        let flags = unsafe { ffi::SDL_GetWindowFlags(self.window) };
        let is_fullscreen = (flags & ffi::GNSH_SDL_WINDOW_FULLSCREEN) != 0;
        if !unsafe { ffi::SDL_SetWindowFullscreen(self.window, !is_fullscreen) } {
            anyhow::bail!("SDL_SetWindowFullscreen failed: {}", crate::sdl_error());
        }
        Ok(())
    }

    /// Writes UTF-8 text to the system clipboard.
    pub fn set_clipboard_text(&self, text: &str) {
        if let Ok(c_text) = CString::new(text)
            && !unsafe { ffi::SDL_SetClipboardText(c_text.as_ptr()) }
        {
            tracing::debug!("SDL_SetClipboardText failed: {}", crate::sdl_error());
        }
    }

    /// Reads UTF-8 text from the system clipboard, returning `None` when unavailable or empty.
    pub fn get_clipboard_text(&self) -> Option<String> {
        let ptr = unsafe { ffi::SDL_GetClipboardText() };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { ffi::SDL_free(ptr.cast()) };
        if text.is_empty() { None } else { Some(text) }
    }

    /// Enables or disables SDL3 relative mouse mode for camera/aim capture.
    pub fn set_relative_mouse_mode(&mut self, enabled: bool) {
        if self.mouse_locked == enabled {
            return;
        }
        // SAFETY: self.window is a valid SDL_Window* owned by self.
        if unsafe { ffi::SDL_SetWindowRelativeMouseMode(self.window, enabled) } {
            self.mouse_locked = enabled;
        } else {
            tracing::warn!(
                "SDL_SetWindowRelativeMouseMode({enabled}) failed: {}",
                crate::sdl_error()
            );
        }
    }

    /// Returns whether relative mouse capture is currently active.
    pub fn is_relative_mouse_mode(&self) -> bool {
        self.mouse_locked
    }

    /// Polls all pending SDL events, updates `raw_input` / `kbm`, and dispatches gamepad hotplug events.
    pub fn poll_events(
        &mut self,
        gamepads: &mut GamepadManager,
        kbm: &mut KbmEmulator,
        raw_input: &mut egui::RawInput,
    ) -> WindowEventSummary {
        raw_input.events.clear();
        let mut summary = WindowEventSummary::default();

        let mut event = ffi::SDL_Event::default();
        while unsafe { ffi::SDL_PollEvent(&mut event) } {
            let event_type = unsafe { event.type_ };
            match event_type {
                ffi::SDL_EventType_SDL_EVENT_QUIT
                | ffi::SDL_EventType_SDL_EVENT_WINDOW_CLOSE_REQUESTED => {
                    summary.quit = true;
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_RESIZED
                | ffi::SDL_EventType_SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED => {
                    let win = unsafe { event.window };
                    summary.resized = Some((win.data1.max(1) as u32, win.data2.max(1) as u32));
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_FOCUS_GAINED => {
                    raw_input.focused = true;
                    raw_input.events.push(egui::Event::WindowFocused(true));
                    let mods = current_modifiers();
                    if mods != self.last_modifiers {
                        self.last_modifiers = mods;
                        raw_input.events.push(egui::Event::ModifiersChanged(mods));
                    }
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_FOCUS_LOST => {
                    raw_input.focused = false;
                    self.last_modifiers = egui::Modifiers::default();
                    if self.mouse_locked {
                        self.set_relative_mouse_mode(false);
                    }
                    kbm.clear_state();
                    raw_input.events.push(egui::Event::WindowFocused(false));
                }
                ffi::SDL_EventType_SDL_EVENT_WINDOW_MOUSE_LEAVE => {
                    raw_input.events.push(egui::Event::PointerGone);
                }
                ffi::SDL_EventType_SDL_EVENT_MOUSE_MOTION => {
                    let motion = unsafe { event.motion };
                    if self.mouse_locked {
                        kbm.on_mouse_motion(motion.xrel, motion.yrel);
                    } else {
                        raw_input
                            .events
                            .push(egui::Event::PointerMoved(egui::pos2(motion.x, motion.y)));
                    }
                }
                ffi::SDL_EventType_SDL_EVENT_MOUSE_BUTTON_DOWN
                | ffi::SDL_EventType_SDL_EVENT_MOUSE_BUTTON_UP => {
                    let btn = unsafe { event.button };
                    if event_type == ffi::SDL_EventType_SDL_EVENT_MOUSE_BUTTON_DOWN && btn.down {
                        summary.last_kbm_input = Some(KbmInput::MouseButton(btn.button));
                    }
                    if self.mouse_locked {
                        kbm.on_mouse_button(btn.button, btn.down);
                    } else {
                        let button = match btn.button {
                            1 => Some(egui::PointerButton::Primary),
                            3 => Some(egui::PointerButton::Secondary),
                            2 => Some(egui::PointerButton::Middle),
                            _ => None,
                        };
                        if let Some(button) = button {
                            raw_input.events.push(egui::Event::PointerButton {
                                pos: egui::pos2(btn.x, btn.y),
                                button,
                                pressed: btn.down,
                                modifiers: current_modifiers(),
                            });
                        }
                    }
                }
                ffi::SDL_EventType_SDL_EVENT_MOUSE_WHEEL => {
                    let wheel = unsafe { event.wheel };
                    let sign =
                        if wheel.direction == ffi::SDL_MouseWheelDirection_SDL_MOUSEWHEEL_FLIPPED {
                            -1.0
                        } else {
                            1.0
                        };
                    raw_input.events.push(egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(wheel.x * sign * 2.0, wheel.y * sign * 2.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Wheel,
                        modifiers: current_modifiers(),
                    });
                }
                ffi::SDL_EventType_SDL_EVENT_TEXT_INPUT => {
                    if self.mouse_locked {
                        continue;
                    }
                    let text_ev = unsafe { event.text };
                    if !text_ev.text.is_null() {
                        let text = unsafe { CStr::from_ptr(text_ev.text) }
                            .to_string_lossy()
                            .into_owned();
                        let filtered: String = text
                            .chars()
                            .filter(|&c| !matches!(c, '\r' | '\n' | '\t') && !c.is_control())
                            .collect();
                        if !filtered.is_empty() {
                            raw_input.events.push(egui::Event::Text(filtered));
                        }
                    }
                }
                ffi::SDL_EventType_SDL_EVENT_KEY_DOWN | ffi::SDL_EventType_SDL_EVENT_KEY_UP => {
                    let key_ev = unsafe { event.key };
                    let pressed = event_type == ffi::SDL_EventType_SDL_EVENT_KEY_DOWN;
                    self.handle_key_event(key_ev, pressed, kbm, raw_input, &mut summary);
                }
                ffi::SDL_EventType_SDL_EVENT_GAMEPAD_ADDED => {
                    let which = unsafe { event.gdevice.which };
                    gamepads.on_device_added(which);
                    summary.audio_devices_changed = true;
                }
                ffi::SDL_EventType_SDL_EVENT_GAMEPAD_REMOVED => {
                    let which = unsafe { event.gdevice.which };
                    gamepads.on_device_removed(which);
                    summary.audio_devices_changed = true;
                }
                ffi::SDL_EventType_SDL_EVENT_AUDIO_DEVICE_ADDED
                | ffi::SDL_EventType_SDL_EVENT_AUDIO_DEVICE_REMOVED => {
                    summary.audio_devices_changed = true;
                }
                _ => {}
            }
        }

        let (w, h) = self.size();
        raw_input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(w as f32, h as f32),
        ));

        summary
    }

    fn handle_key_event(
        &mut self,
        key_ev: ffi::SDL_KeyboardEvent,
        pressed: bool,
        kbm: &mut KbmEmulator,
        raw_input: &mut egui::RawInput,
        summary: &mut WindowEventSummary,
    ) {
        let modifiers = sdl_mods_to_egui(key_ev.mod_);
        if modifiers != self.last_modifiers {
            self.last_modifiers = modifiers;
            raw_input
                .events
                .push(egui::Event::ModifiersChanged(modifiers));
        }

        if pressed && !key_ev.repeat {
            match key_ev.key {
                ffi::SDLK_F11 => summary.toggle_fullscreen = true,
                ffi::SDLK_F12 => summary.take_screenshot = true,
                ffi::SDLK_F9 => summary.toggle_recording = true,
                ffi::SDLK_F10 => summary.save_replay_clip = true,
                ffi::SDLK_F3 => summary.toggle_hud = true,
                ffi::SDLK_F8 => summary.toggle_mouse_lock = true,
                ffi::SDLK_ESCAPE => summary.escape_pressed = true,
                ffi::SDLK_HOME if !modifiers.ctrl && !modifiers.alt => summary.go_home = true,
                _ => {}
            }
            let candidate = KbmInput::Key(key_ev.key);
            if !KbmBindings::is_reserved_shortcut(candidate) {
                summary.last_kbm_input = Some(candidate);
            }
        }

        if pressed && modifiers.ctrl && !modifiers.alt {
            match key_ev.key {
                ffi::SDLK_M if !key_ev.repeat => summary.toggle_mic = true,
                ffi::SDLK_C if !self.mouse_locked => raw_input.events.push(egui::Event::Copy),
                ffi::SDLK_X if !self.mouse_locked => raw_input.events.push(egui::Event::Cut),
                ffi::SDLK_V if !self.mouse_locked => {
                    if let Some(clipboard) = self.get_clipboard_text() {
                        raw_input.events.push(egui::Event::Paste(clipboard));
                    }
                }
                _ => {}
            }
        }

        if self.mouse_locked {
            if !key_ev.repeat {
                kbm.on_key(key_ev.key, pressed);
            }
            return;
        }

        if let Some(key) = map_sdl_key(key_ev.key) {
            raw_input.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: key_ev.repeat,
                modifiers,
            });
        }
    }
}

impl Drop for AppWindow {
    fn drop(&mut self) {
        if !self.window.is_null() {
            unsafe {
                if self.mouse_locked {
                    let _ = ffi::SDL_SetWindowRelativeMouseMode(self.window, false);
                }
                let _ = ffi::SDL_StopTextInput(self.window);
                ffi::SDL_DestroyWindow(self.window);
            }
            self.window = std::ptr::null_mut();
        }
    }
}

fn current_modifiers() -> egui::Modifiers {
    sdl_mods_to_egui(unsafe { ffi::SDL_GetModState() })
}

fn sdl_mods_to_egui(mods: ffi::SDL_Keymod) -> egui::Modifiers {
    let ctrl = (mods & ffi::GNSH_SDL_KMOD_CTRL) != 0;
    let shift = (mods & ffi::GNSH_SDL_KMOD_SHIFT) != 0;
    let alt = (mods & ffi::GNSH_SDL_KMOD_ALT) != 0;
    let gui = (mods & ffi::GNSH_SDL_KMOD_GUI) != 0;
    let mac_cmd = cfg!(target_os = "macos") && gui;
    let command = if cfg!(target_os = "macos") {
        mac_cmd
    } else {
        ctrl
    };
    egui::Modifiers {
        alt,
        ctrl,
        shift,
        mac_cmd,
        command,
    }
}

fn map_sdl_key(key: ffi::SDL_Keycode) -> Option<egui::Key> {
    Some(match key {
        ffi::SDLK_RETURN | ffi::SDLK_KP_ENTER => egui::Key::Enter,
        ffi::SDLK_ESCAPE => egui::Key::Escape,
        ffi::SDLK_BACKSPACE => egui::Key::Backspace,
        ffi::SDLK_TAB => egui::Key::Tab,
        ffi::SDLK_SPACE => egui::Key::Space,
        ffi::SDLK_DELETE => egui::Key::Delete,
        ffi::SDLK_INSERT => egui::Key::Insert,
        ffi::SDLK_HOME => egui::Key::Home,
        ffi::SDLK_END => egui::Key::End,
        ffi::SDLK_PAGEUP => egui::Key::PageUp,
        ffi::SDLK_PAGEDOWN => egui::Key::PageDown,
        ffi::SDLK_LEFT => egui::Key::ArrowLeft,
        ffi::SDLK_RIGHT => egui::Key::ArrowRight,
        ffi::SDLK_UP => egui::Key::ArrowUp,
        ffi::SDLK_DOWN => egui::Key::ArrowDown,
        ffi::SDLK_A => egui::Key::A,
        ffi::SDLK_B => egui::Key::B,
        ffi::SDLK_C => egui::Key::C,
        ffi::SDLK_D => egui::Key::D,
        ffi::SDLK_E => egui::Key::E,
        ffi::SDLK_F => egui::Key::F,
        ffi::SDLK_G => egui::Key::G,
        ffi::SDLK_H => egui::Key::H,
        ffi::SDLK_I => egui::Key::I,
        ffi::SDLK_J => egui::Key::J,
        ffi::SDLK_K => egui::Key::K,
        ffi::SDLK_L => egui::Key::L,
        ffi::SDLK_M => egui::Key::M,
        ffi::SDLK_N => egui::Key::N,
        ffi::SDLK_O => egui::Key::O,
        ffi::SDLK_P => egui::Key::P,
        ffi::SDLK_Q => egui::Key::Q,
        ffi::SDLK_R => egui::Key::R,
        ffi::SDLK_S => egui::Key::S,
        ffi::SDLK_T => egui::Key::T,
        ffi::SDLK_U => egui::Key::U,
        ffi::SDLK_V => egui::Key::V,
        ffi::SDLK_W => egui::Key::W,
        ffi::SDLK_X => egui::Key::X,
        ffi::SDLK_Y => egui::Key::Y,
        ffi::SDLK_Z => egui::Key::Z,
        ffi::SDLK_0 => egui::Key::Num0,
        ffi::SDLK_1 => egui::Key::Num1,
        ffi::SDLK_2 => egui::Key::Num2,
        ffi::SDLK_3 => egui::Key::Num3,
        ffi::SDLK_4 => egui::Key::Num4,
        ffi::SDLK_5 => egui::Key::Num5,
        ffi::SDLK_6 => egui::Key::Num6,
        ffi::SDLK_7 => egui::Key::Num7,
        ffi::SDLK_8 => egui::Key::Num8,
        ffi::SDLK_9 => egui::Key::Num9,
        ffi::SDLK_F1 => egui::Key::F1,
        ffi::SDLK_F2 => egui::Key::F2,
        ffi::SDLK_F3 => egui::Key::F3,
        ffi::SDLK_F4 => egui::Key::F4,
        ffi::SDLK_F5 => egui::Key::F5,
        ffi::SDLK_F6 => egui::Key::F6,
        ffi::SDLK_F7 => egui::Key::F7,
        ffi::SDLK_F8 => egui::Key::F8,
        ffi::SDLK_F9 => egui::Key::F9,
        ffi::SDLK_F10 => egui::Key::F10,
        ffi::SDLK_F11 => egui::Key::F11,
        ffi::SDLK_F12 => egui::Key::F12,
        ffi::SDLK_MINUS => egui::Key::Minus,
        ffi::SDLK_EQUALS => egui::Key::Equals,
        ffi::SDLK_PLUS => egui::Key::Plus,
        ffi::SDLK_COMMA => egui::Key::Comma,
        ffi::SDLK_PERIOD => egui::Key::Period,
        ffi::SDLK_SLASH => egui::Key::Slash,
        ffi::SDLK_SEMICOLON => egui::Key::Semicolon,
        ffi::SDLK_APOSTROPHE => egui::Key::Quote,
        ffi::SDLK_LEFTBRACKET => egui::Key::OpenBracket,
        ffi::SDLK_RIGHTBRACKET => egui::Key::CloseBracket,
        ffi::SDLK_BACKSLASH => egui::Key::Backslash,
        ffi::SDLK_GRAVE => egui::Key::Backtick,
        _ => return None,
    })
}
