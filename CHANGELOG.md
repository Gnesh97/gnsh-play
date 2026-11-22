# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added (2026-11-22)
- Initialized repository structure with `README.md`, `.gitignore`, and `CHANGELOG.md`.
- Configured private GitHub remote (`Gnesh97/gnsh-play`).
- Added initial architecture overview ([`docs/architecture.md`](docs/architecture.md)) and updated [`README.md`](README.md) with project navigation links.
- Scaffolded the Rust Cargo workspace (`Cargo.toml`, `.gitignore`), added `streetpea/chiaki-ng` as `third_party/chiaki-ng`, built static `libchiaki.a` via MSYS2 MINGW64, and implemented `crates/gnsh-chiaki-sys` FFI bindings verified with a passing `chiaki_lib_init` smoke test.
- Implemented `crates/gnsh-proto` (PS5 LAN DDP discovery/wake-up, PSN OAuth account-ID browser helper, 8-digit PIN registration, controller mapping, and `Ps5Session` with built-in Opus PCM decode) and Windows DPAPI encrypted storage in `crates/gnsh-app/src/storage.rs` (`8` workspace unit tests passing).
- Implemented `crates/gnsh-media` (FFmpeg 9.1 `D3D11VA` zero-copy video decoder with automatic software fallback, native Direct3D 11 DXGI flip-model renderer with HLSL `NV12` `Texture2DArray` & `YUV420P` BT.709 shaders, and SDL3 `AudioOutput` latency-capped PCM stream; `11` workspace unit tests passing).
- Implemented `crates/gnsh-input` (SDL3 3.4.20 window/HWND/clipboard/event integration and `GamepadManager` DualSense polling with stick deadzones, trigger scaling, touchpad coordinates, gyro/accel sensors, rumble, and LED feedback) and `crates/gnsh-app` (`egui` 0.37 Direct3D 11 painter, Home/Register/Streaming HUD GUI, and `--help`/`--discover` CLI modes; `18` workspace tests passing with `clippy -D warnings` clean).
- Added [`scripts/stage-dist.ps1`](scripts/stage-dist.ps1) and verified standalone release bundle (`dist/gnsh-play.exe` + `97` MinGW64 DLLs, `141.57 MiB`) executing `--help` and `--discover` without MSYS2 on `PATH`.
