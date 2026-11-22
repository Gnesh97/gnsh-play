# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Added native UTF-8 PS5 On-Screen Keyboard (`OSK`) integration (`enable_keyboard: true`, `SessionEvent::KeyboardOpen` / `KeyboardTextChange` / `KeyboardRemoteClose`, `Ps5Session::keyboard_set_text` / `keyboard_accept` / `keyboard_reject` in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs)) with a centered auto-focused `egui` modal (`Enter` submits, `Esc` cancels without disconnecting, live UTF-8 text sync) in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs).
- Added PlayStation Home shortcut (`Home` key in [`crates/gnsh-input/src/window.rs`](crates/gnsh-input/src/window.rs), `Ps5Session::go_home` in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs), and HUD `PS Home` button in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs)).
- Implemented 4-channel USB DualSense voice-coil haptics output (`HapticsAudioOutput` in [`crates/gnsh-media/src/audio.rs`](crates/gnsh-media/src/audio.rs)) with 3 kHz stereo-to-rear-channel remixing, SDL3 hotplug re-detection, and automatic motor-rumble fallback when no 4-channel USB endpoint is present.
- Implemented Opus uplink microphone streaming (`ChiakiOpusEncoder` + `Ps5Session::set_mic_muted` / `send_mic_pcm` in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs)), 48 kHz stereo `MicCapture` in [`crates/gnsh-media/src/audio.rs`](crates/gnsh-media/src/audio.rs), DualSense mute button (`MISC1`) + orange mic LED (`DS5EffectsState_t`) in [`crates/gnsh-input/src/gamepad.rs`](crates/gnsh-input/src/gamepad.rs), `Ctrl+M` shortcut in [`crates/gnsh-input/src/window.rs`](crates/gnsh-input/src/window.rs), and HUD mute button + haptics mode indicator in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs).
- Added RAII Win32 `SleepInhibitor` (`SetThreadExecutionState` with `ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED` + 1 ms `timeBeginPeriod`/`timeEndPeriod` timer) in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs) so Windows display/sleep timers pause and sleep intervals remain precise while streaming.
- Added live packet-arrival-to-present local pipeline latency telemetry (`Pipe: p50 ms (p95 ms, Buf N)` via per-frame FFmpeg PTS passthrough in [`crates/gnsh-media/src/video_decoder.rs`](crates/gnsh-media/src/video_decoder.rs)) and a live `VSync` toggle in the streaming HUD ([`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs), [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs)).

### Changed
- Reduced end-to-end streaming and input latency across the full pipeline (`AV_CODEC_FLAG2_FAST` + slice-only `thread_type = 2` + single-thread `D3D11VA` decode in [`crates/gnsh-media/src/video_decoder.rs`](crates/gnsh-media/src/video_decoder.rs); 1-frame `IDXGISwapChain2` waitable object, per-texture NV12 SRV cache, and cached constant-buffer uploads in [`crates/gnsh-media/src/d3d11_renderer.rs`](crates/gnsh-media/src/d3d11_renderer.rs); 30 ms playback / 40 ms haptics SDL3 queue caps in [`crates/gnsh-media/src/audio.rs`](crates/gnsh-media/src/audio.rs); raw zero-deadzone DualSense analog sticks in [`crates/gnsh-input/src/gamepad.rs`](crates/gnsh-input/src/gamepad.rs); `Condvar` decode-paced frame wakeup + 1000 Hz controller polling when VSync is off, pre-input `VSync` latency wait ordering, zero-`egui` pass when HUD is hidden, and `THREAD_PRIORITY_ABOVE_NORMAL` media workers in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs)).

### Fixed
- Wired DualSense haptic PCM streams (`session.haptics_sink`) to rumble conversion in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs), switched PS5 rumble commands to Sony's 47-byte `DS5EffectsState_t` HID packet (`SDL_SendGamepadEffect`) in [`crates/gnsh-input/src/gamepad.rs`](crates/gnsh-input/src/gamepad.rs), and added a 250 ms idle auto-stop watchdog in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs) so controller vibration works in PS5 DualSense games (`32` workspace tests passing).

## [0.1.0] - 2026-11-22

### Added
- Wired Sony DualSense adaptive triggers (`L2`/`R2`) via SDL3 `SDL_SendGamepadEffect` (47-byte `DS5EffectsState_t`) in [`crates/gnsh-input/src/gamepad.rs`](crates/gnsh-input/src/gamepad.rs) and [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs), clearing trigger tension cleanly on stream stop and gamepad close (`30` workspace tests passing).
- Configured console-less Windows release GUI subsystem (`windows_subsystem = "windows"`) with `AttachConsole(ATTACH_PARENT_PROCESS)` CLI restoration (`--help`, `--discover`) in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs).
- Added persistent `%APPDATA%\gnsh-play\gnsh-play.log` file logging (with 2 MiB startup rotation + panic hook capture) and session lifecycle tracing in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs).
- Tuned Takion UDP `SO_RCVBUF` to `4 MiB` (`UDP_RECV_BUF_BYTES`) at runtime on `SessionEvent::Connected` via [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs) without modifying `third_party/chiaki-ng`.
- Added live stream HUD telemetry (rolling `Mbps`, `RTT ms`, `Loss %`, and `UDP Buf KiB`) in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs) + `Ps5Session::telemetry()`.
- Persisted stream and controller preferences (`preset`, `codec`, clamped `bitrate_kbps`, `vsync`, `touchpad_touch`) to `%APPDATA%\gnsh-play\settings.json` via `SettingsStore` in [`crates/gnsh-app/src/storage.rs`](crates/gnsh-app/src/storage.rs).
- Initialized repository structure with `README.md`, `.gitignore`, `CHANGELOG.md`, and private GitHub remote (`Gnesh97/gnsh-play`).
- Added initial architecture overview ([`docs/architecture.md`](docs/architecture.md)).
- Scaffolded the Rust Cargo workspace (`Cargo.toml`), added `streetpea/chiaki-ng` as `third_party/chiaki-ng`, built static `libchiaki.a` via MSYS2 MINGW64, and implemented `crates/gnsh-chiaki-sys` FFI bindings.
- Implemented `crates/gnsh-proto` (PS5 LAN DDP discovery/wake-up, PSN OAuth account-ID browser helper, 8-digit PIN registration, controller mapping, and `Ps5Session` with Opus decode) and Windows DPAPI encrypted storage in `crates/gnsh-app/src/storage.rs`.
- Implemented `crates/gnsh-media` (FFmpeg 9.1 `D3D11VA` zero-copy video decoder with automatic software fallback, native Direct3D 11 DXGI flip-model renderer with HLSL `NV12` `Texture2DArray` & `YUV420P` BT.709 shaders, and SDL3 `AudioOutput`).
- Implemented `crates/gnsh-input` (SDL3 3.4.20 window/HWND/clipboard/event integration and `GamepadManager`) and `crates/gnsh-app` (`egui` 0.37 Direct3D 11 painter, Home/Register/Streaming HUD GUI, and `--help`/`--discover` CLI modes).
- Added [`scripts/stage-dist.ps1`](scripts/stage-dist.ps1) to bundle standalone `dist/gnsh-play.exe` with resolved MinGW64 runtime DLLs.

### Fixed
- Eliminated ~40 ms synchronous console/file log stalls on `libchiaki`'s `takion_thread_func` UDP receive loop (which overflowed `GKCrypt`'s `256 KiB` keystream buffer during `100 Mbps` foliage bursts) by offloading tracing writes to a bounded non-blocking background log thread (`gnsh-log-writer`) in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs), downgrading per-unit `Missing unit` warnings to `debug` in [`crates/gnsh-proto/src/log.rs`](crates/gnsh-proto/src/log.rs), and deduplicating `ActiveStream::shutdown` logs.
- Prevented high-entropy foliage video freezes by disabling `enable_idr_on_fec_failure` (matching `chiaki-ng` default) and expanding the bounded video channel to `32` frames in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs), adding `EAGAIN` drain-and-retry + `avcodec_flush_buffers` recovery in [`crates/gnsh-media/src/video_decoder.rs`](crates/gnsh-media/src/video_decoder.rs) and [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs), and releasing D3D11VA frames before `VideoDecoder` teardown in `ActiveStream::shutdown`.
- Added live 1 s Win32 ICMP RTT probing (`iphlpapi`) in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs) with Senkusha fallback (since `libchiaki` only measures `rtt_us` once at handshake) and aligned [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs) HUD `Loss %` sampling with a `200 ms × 10` rolling average window.
- Clamped PS5 stream bitrate to `2_000..=100_000` kbps (`MIN_BITRATE_KBPS..=MAX_BITRATE_KBPS`) in [`crates/gnsh-proto/src/session.rs`](crates/gnsh-proto/src/session.rs) and enforced `SliderClamping::Always` in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs), preventing `>100 Mbps` burst FEC disconnects and `>250 Mbps` 256-slot FEC IDR stalls (black screen / `0 FPS`).
- Fixed DualSense capacitive touchpad touches falsely registering as touchpad clicks (e.g. `streetpea/chiaki-ng#894`) by defaulting [`crates/gnsh-input/src/gamepad.rs`](crates/gnsh-input/src/gamepad.rs) + [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs) to click-only touchpad mode with an optional `Touchpad Touch` toggle (Home bar & live HUD), 7-bit incremental touch tracking IDs, 4 px noise deadband, and exact `1919x942` scaling.
- Cropped `D3D11VA` GPU surface alignment padding (e.g., `1920x1152` HEVC textures) in [`crates/gnsh-media/src/d3d11_renderer.rs`](crates/gnsh-media/src/d3d11_renderer.rs) via per-frame texture-desc UV rect + sub-texel luma/chroma clamp, eliminating the bottom green bar on HEVC streams.
- Enabled `egui` `default_fonts` feature in [`Cargo.toml`](Cargo.toml) (fixing missing glyph boxes in `gnsh-play` UI) and added `default_fonts_are_embedded` unit test in [`crates/gnsh-app/src/egui_d3d11.rs`](crates/gnsh-app/src/egui_d3d11.rs).

### Changed
- Raised default LAN bitrate to `30_000` kbps and set the bitrate slider range to `2_000..=100_000` kbps in [`crates/gnsh-app/src/main.rs`](crates/gnsh-app/src/main.rs).
