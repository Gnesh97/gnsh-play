# gnsh-play

Native Windows x64 PlayStation 5 Remote Play client built in Rust (`AGPL-3.0-only`), powered by `streetpea/chiaki-ng` (`libchiaki`), FFmpeg (`D3D11VA` + software fallback), native Direct3D 11, SDL3, and `egui`.

## Features (v0.1.0 — Local LAN, PS5 Only)

- **LAN PS5 Discovery & Wake-Up**: DDP subnet broadcast discovery and standby wake-up via `libchiaki` (`gnsh-proto`), plus `4 MiB` runtime `SO_RCVBUF` socket tuning (`UDP_RECV_BUF_BYTES`).
- **PSN Account ID & 8-Digit PIN Linking**: Browser OAuth redirect URL helper (or direct Base64 Account ID) + PS5 registration flow persisted in Windows DPAPI encrypted storage (`%APPDATA%\gnsh-play\consoles.json`).
- **Low-Latency Media Pipeline**: FFmpeg `D3D11VA` (`thread_count = 1`, `thread_type = 2` slice-only, `AV_CODEC_FLAG_LOW_DELAY | AV_CODEC_FLAG2_FAST`) zero-copy hardware H.264/HEVC/**HEVC HDR10** decoding (`NV12` & `P010`, padded surface UV crop, `EAGAIN` drain-and-retry, and automatic `YUV420P`/`NV12`/`YUV420P10LE`/`P010LE` software fallback), native DXGI flip-model Direct3D 11 shader rendering with `Fit`/`Zoom`/`Stretch` aspect scaling, 1-frame waitable swapchain (`IDXGISwapChain2`), `IDXGIOutput6` Windows HDR10 passthrough (`R10G10B10A2_UNORM` / ST.2084 PQ) or automatic BT.2020->ACES SDR tone-mapping, `200 nits` `egui` HDR paper-white conversion, single-pass 5-tap HLSL **CAS sharpening** (`0..=100%`), D3D11 SRV cache, decode-paced `Condvar` frame wakeup + 1000 Hz controller polling when `VSync` is off, pre-input VSync latency wait ordering, zero-`egui` pass when HUD is hidden (`F3`), `chiaki_opus_decoder` + SDL3 30 ms capped stereo playback with selectable audio device & live `0..=100%` volume gain, **4-channel USB DualSense audio haptics** (with automatic motor-rumble fallback), and **Opus microphone uplink** (`gnsh-media`, `gnsh-proto`).
- **Native Input, KB+M Emulation, DualSense Features, PS5 OSK & Keep-Awake**: SDL3 windowing + Win32 `HWND` hosting, `egui` 0.37 Direct3D 11 overlay UI, native auto-focused UTF-8 **PS5 On-Screen Keyboard (`OSK`) modal**, `Home` / `PS Home` button, virtual DualSense **Keyboard & Mouse emulation** (`WASD` normalized stick, smoothed mouse-to-right-stick with deadzone boost + `Invert Y`, `F8` relative mouse lock, interactive `Keybinds...` remapping modal with conflict-free swap for keyboard keys & mouse buttons `1..=5`, serializable `KbmBindings` + simultaneous physical gamepad merge), and `SDL_Gamepad` polling with raw 0-deadzone sticks, L2/R2 analog triggers, **DualSense adaptive triggers & orange mic LED** (`SDL_SendGamepadEffect`), DualSense mute button / `Ctrl+M` mic toggle, click-only default + capacitive toggle touchpad (`Touchpad Touch`), gyro/accel sensors, rumble, lightbar LED feedback, and automatic Win32 display/sleep keep-awake + 1 ms OS timer (`SetThreadExecutionState` + `timeBeginPeriod`) while streaming (`gnsh-input`, `gnsh-app`).
- **Persistent Settings, Telemetry & File Logging**: Saves stream preferences (`preset` 720p/1080p × 30/60 FPS, `codec`, `scale_mode`, `bitrate_kbps` clamped `2_000..=100_000`, `sharpness_pct`, `volume_pct`, `audio_device`, `kbm_enabled`, `kbm_sensitivity_pct`, `kbm_deadzone_pct`, `kbm_invert_y`, `kbm_bindings`, `vsync`, `touchpad_touch`) to `%APPDATA%\gnsh-play\settings.json`, renders live HUD telemetry (`FPS`, rolling `Mbps`, live Win32 ICMP `RTT`, 200 ms × 10 `Loss %`, per-frame PTS `Pipe` p50/p95 ms + `Buf` lag, `HDR10`/`HDR->SDR`, `CAS %`, `UDP Buf KiB`, live `VSync`/`Scale`/`CAS`/`Vol`/`Audio`/`KB+M` controls), and writes non-blocking logs + panic traces to `%APPDATA%\gnsh-play\gnsh-play.log`.
- **Console-less Standalone Distribution**: Release builds use `windows_subsystem = "windows"` (no console window on double-click) while restoring CLI output via `AttachConsole` for `--help` / `--discover`, staged with resolved MinGW64 runtime DLLs by `scripts/stage-dist.ps1`.

## Workspace Architecture

| Crate / Path | Role |
| :--- | :--- |
| [`crates/gnsh-chiaki-sys`](crates/gnsh-chiaki-sys) | Raw bindgen FFI bindings to static `third_party/chiaki-ng` (`libchiaki.a`) |
| [`crates/gnsh-proto`](crates/gnsh-proto) | Safe Rust wrapper for LAN discovery, wake-up, PSN OAuth exchange, PIN registration, controller state, and `Ps5Session` |
| [`crates/gnsh-media`](crates/gnsh-media) | FFmpeg `D3D11VA`/SW `VideoDecoder`, DXGI/D3D11 HLSL `D3D11Renderer`, and SDL3 `AudioOutput` / `HapticsAudioOutput` / `MicCapture` |
| [`crates/gnsh-input`](crates/gnsh-input) | SDL3 windowing, Win32 `HWND`/clipboard integration, `egui` event translation, `GamepadManager`, and `KbmEmulator` |
| [`crates/gnsh-app`](crates/gnsh-app) | `gnsh-play` CLI/GUI application (`main.rs`), D3D11 `egui` painter (`egui_d3d11.rs`), and DPAPI storage (`storage.rs`) |
| [`scripts/stage-dist.ps1`](scripts/stage-dist.ps1) | Release build and standalone `dist/` DLL staging script |

## Prerequisites (Windows x64)

- **Rustup** (`stable-x86_64-pc-windows-gnu`)
- **MSYS2 MINGW64** (`C:\msys64\mingw64`) with GCC, CMake, Ninja, Clang (`LIBCLANG_PATH`), OpenSSL, Opus, json-c, miniupnpc, libevent, curl, FFmpeg, and SDL3
- Initialized git submodule `third_party/chiaki-ng` built into `third_party/build/chiaki-ng/`

## Build, Test & Package

```powershell
$env:PATH = "C:\msys64\mingw64\bin;C:\msys64\usr\bin;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:LIBCLANG_PATH = "C:\msys64\mingw64\bin"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Stage standalone release bundle into dist\
powershell -ExecutionPolicy Bypass -File .\scripts\stage-dist.ps1
```

## Running `gnsh-play`

```powershell
# Print CLI usage
.\dist\gnsh-play.exe --help

# Scan local LAN subnets for PS5 consoles and exit
.\dist\gnsh-play.exe --discover

# Launch GUI
.\dist\gnsh-play.exe
```

### Shortcuts & Controls

- **`F3`**: Toggle the streaming HUD overlay (`FPS`, rolling `Mbps`, live `RTT`, `Loss %`, `Pipe` p50/p95 latency, decode latency, `D3D11VA`/`SW` backend indicator, `UDP Buf`, active gamepad/KB+M, `Haptics` mode, `Mic` mute button, `PS Home` button, `Touchpad Touch`, `VSync`, `Scale`, `CAS`, `Vol`, `Audio`, `KB+M`, Disconnect)
- **`F8`**: Toggle cursor capture (`KB+M` virtual DualSense mode; auto-enables `KB+M` when pressed while streaming)
- **`F11`**: Toggle fullscreen mode
- **`Home`**: Send the PlayStation Home button (`PS Home`) when no text modal is open
- **`Ctrl+M` / DualSense Mute Button**: Toggle uplink microphone mute state (syncs controller orange mic LED)
- **`Escape`**: Unlock mouse cursor if locked, cancel open OSK/modal, or disconnect active stream

## License & Attribution

`gnsh-play` is licensed under **`AGPL-3.0-only`** because it statically links `streetpea/chiaki-ng` (`third_party/chiaki-ng`, AGPL-3.0).

## Documentation

- [Architecture Overview](docs/architecture.md) — Workspace crate boundaries, low-latency media/input threading, Direct3D 11 / Direct3D 12 / Vulkan pipelines, and security model.
- [Changelog](CHANGELOG.md) — Atomic change log updated on every commit.
