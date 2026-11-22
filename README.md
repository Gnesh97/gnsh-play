# gnsh-play

Native Windows x64 PlayStation 5 Remote Play client built in Rust (`AGPL-3.0-only`), powered by `streetpea/chiaki-ng` (`libchiaki`), FFmpeg (`D3D11VA` + software fallback), native Direct3D 11, SDL3, and `egui`.

## Features (v0.1.0 — Local LAN, PS5 Only)

- **LAN PS5 Discovery & Wake-Up**: DDP subnet broadcast discovery and standby wake-up via `libchiaki` (`gnsh-proto`), plus `4 MiB` runtime `SO_RCVBUF` socket tuning (`UDP_RECV_BUF_BYTES`).
- **PSN Account ID & 8-Digit PIN Linking**: Browser OAuth redirect URL helper (or direct Base64 Account ID) + PS5 registration flow persisted in Windows DPAPI encrypted storage (`%APPDATA%\gnsh-play\consoles.json`).
- **Low-Latency Media Pipeline**: FFmpeg `D3D11VA` zero-copy hardware H.264/HEVC video decoding (with padded surface UV crop, `EAGAIN` drain-and-retry, and automatic `YUV420P`/`NV12` software fallback), native DXGI flip-model Direct3D 11 shader rendering, and `chiaki_opus_decoder` + SDL3 latency-capped 48 kHz S16 stereo audio (`gnsh-media`).
- **Native Input & DualSense Features**: SDL3 windowing + Win32 `HWND` hosting, `egui` 0.37 Direct3D 11 overlay UI, and `SDL_Gamepad` polling with stick deadzones, L2/R2 analog triggers, **DualSense adaptive triggers** (`SDL_SendGamepadEffect`), click-only default + capacitive toggle touchpad (`Touchpad Touch`), gyro/accel sensors, rumble, and lightbar LED feedback (`gnsh-input`, `gnsh-app`).
- **Persistent Settings, Telemetry & File Logging**: Saves stream preferences (`preset`, `codec`, `bitrate_kbps` clamped `2_000..=100_000`, `vsync`, `touchpad_touch`) to `%APPDATA%\gnsh-play\settings.json`, renders live HUD telemetry (`FPS`, rolling `Mbps`, live Win32 ICMP `RTT`, 200 ms × 10 `Loss %`, `UDP Buf KiB`), and writes non-blocking logs + panic traces to `%APPDATA%\gnsh-play\gnsh-play.log`.
- **Console-less Standalone Distribution**: Release builds use `windows_subsystem = "windows"` (no console window on double-click) while restoring CLI output via `AttachConsole` for `--help` / `--discover`, staged with resolved MinGW64 runtime DLLs by `scripts/stage-dist.ps1`.

## Workspace Architecture

| Crate / Path | Role |
| :--- | :--- |
| [`crates/gnsh-chiaki-sys`](crates/gnsh-chiaki-sys) | Raw bindgen FFI bindings to static `third_party/chiaki-ng` (`libchiaki.a`) |
| [`crates/gnsh-proto`](crates/gnsh-proto) | Safe Rust wrapper for LAN discovery, wake-up, PSN OAuth exchange, PIN registration, controller state, and `Ps5Session` |
| [`crates/gnsh-media`](crates/gnsh-media) | FFmpeg `D3D11VA`/SW `VideoDecoder`, DXGI/D3D11 HLSL `D3D11Renderer`, and SDL3 `AudioOutput` |
| [`crates/gnsh-input`](crates/gnsh-input) | SDL3 windowing, Win32 `HWND`/clipboard integration, `egui` event translation, and `GamepadManager` |
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

- **`F3`**: Toggle the streaming HUD overlay (`FPS`, rolling `Mbps`, live `RTT`, `Loss %`, decode latency, `D3D11VA`/`SW` backend indicator, `UDP Buf`, active gamepad, `Touchpad Touch`, Disconnect)
- **`F11`**: Toggle fullscreen mode
- **`Escape`**: Close open modal or disconnect active stream

## License & Attribution

`gnsh-play` is licensed under **`AGPL-3.0-only`** because it statically links `streetpea/chiaki-ng` (`third_party/chiaki-ng`, AGPL-3.0).

## Documentation

- [Architecture Overview](docs/architecture.md) — Workspace crate boundaries, low-latency media/input threading, Direct3D 11 / Direct3D 12 / Vulkan pipelines, and security model.
- [Changelog](CHANGELOG.md) — Atomic change log updated on every commit.
