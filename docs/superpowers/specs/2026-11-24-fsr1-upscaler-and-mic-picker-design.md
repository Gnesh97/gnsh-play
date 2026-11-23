# Design Specification: Thin `VideoRenderer` Boundary, 2-Pass AMD FSR 1.0 (`EASU` + `RCAS`), and Live Microphone Device Picker

**Date:** 2026-11-24  
**Target Branch:** `dev`  
**Crates:** `gnsh-media`, `gnsh-app`

---

## 1. Goals & Strategic Rationale

PS5 Remote Play transmits compressed 2D video frames (`H.264`, `HEVC`, `HEVC HDR10`) without game depth buffers or motion vectors. Temporal upscalers (FSR 2/3, DLSS) cannot operate without those buffers, leaving spatial filters (`Bilinear`, `CAS`, `FSR 1.0`) as the practical image-enhancement primitives.

- **Windows D3D11 Primary Backend:** Direct3D 11 + `D3D11VA` zero-copy decoding + Flip-Discard `IDXGISwapChain2` remains the lowest-latency, zero-extra-DLL path on Windows.
- **Thin `VideoRenderer` Trait Boundary:** Decoupling `gnsh-app`'s video loop from D3D11-specific draw calls through `VideoRenderer`, `VideoFilterMode`, `VideoRenderOptions`, and `ScaleMode` lets future backends (such as Vulkan + `libplacebo` for user `.glsl` hooks or Linux/SteamOS support) plug in cleanly while D3D11 specifics remain isolated to `D3D11VA` and `EguiD3d11Painter`.
- **Unified Filter UX:** Users select `Filter: Bilinear | CAS | FSR 1.0` alongside a single `0–100%` sharpness slider (`CAS` or `RCAS`).
- **Live Microphone Input Selector:** Users can list and switch recording devices (`Mic:`) in both the Home bar and streaming HUD without restarting the Remote Play session.

---

## 2. Architecture & Component Design

### 2.1 `gnsh-media::renderer` (`VideoRenderer`, `VideoFilterMode`, `VideoRenderOptions`, `ScaleMode`)

- **`ScaleMode` (`Fit`, `Zoom`, `Stretch`)** moves into `crates/gnsh-media/src/renderer.rs` and is re-exported from `d3d11_renderer` and `gnsh-media` root for source compatibility.
- **`VideoFilterMode` (`Bilinear`, `Cas`, `Fsr1`)** selects the active video scaling/sharpening stage:
  - `Bilinear`: Single-pass YUV/P010 conversion and hardware bilinear sampling (sharpness forced to `0.0`).
  - `Cas`: Single-pass YUV/P010 conversion + 5-tap HLSL Contrast Adaptive Sharpening (`0.0..=1.0`).
  - `Fsr1`: Universal 2-pass AMD FSR 1.0 spatial upscaling (`EASU`) and sharpening (`RCAS`).
- **`VideoRenderOptions`** bundles `{ filter_mode: VideoFilterMode, sharpness: f32, scale_mode: ScaleMode }`.
- **`VideoRenderer` trait** defines backend-agnostic video lifecycle and presentation methods (`viewport_size`, `resize`, `is_display_hdr`, `is_hdr_output_active`, `set_hdr_output`, `wait_for_frame_latency`, `clear_video_cache`, `begin_frame`, `draw_video_frame`, `present`).

### 2.2 `D3D11Renderer` 2-Pass AMD FSR 1.0 GPU Pipeline (`crates/gnsh-media/src/d3d11_renderer.rs`)

- **Lazy 10-bit Intermediate Render Targets (`DXGI_FORMAT_R10G10B10A2_UNORM`):**
  - Allocated on demand only when `VideoFilterMode::Fsr1` requires them (`fsr_src_rt` sized to source frame `fw × fh`, `fsr_easu_rt` sized to swapchain viewport `vw × vh`).
  - `R10G10B10A2_UNORM` preserves 10-bit precision for both SDR (`G22`) and HDR10 PQ (`G2084`/`BT.2020`) outputs without format reallocation when toggling HDR.
  - Released on `clear_video_cache()` (and `fsr_easu_rt` on `resize()`).
- **Routing Logic (`draw_video_frame`):**
  - Computes whether upscaling is beneficial via `should_run_fsr_easu(fw, fh, view_w, view_h, scale_mode)` using `aspect_scale`: upscaling is enabled when `scaled_w > frame_w || scaled_h > frame_h`.
  - **Downscale / 1:1 with `sharpness <= 0.001`:** Draws directly in a single pass to the swapchain backbuffer (bypassing all FSR intermediate targets).
  - **Downscale / 1:1 with `sharpness > 0.001`:** Renders normalized YUV/P010 at viewport scale into `fsr_easu_rt` (clearing letterbox margins to opaque black), unbinds RTV, and runs `FSR_RCAS_PS_HLSL` onto the backbuffer clamped strictly to the active video rectangle (`compute_rcas_bounds`) to avoid sharpening letterbox borders.
  - **Upscale (`upscale == true`):**
    1. *Color Normalization Pass:* Renders `NV12` / `P010` / `YUV420P` (plus ACES HDR-to-SDR tone mapping when needed) at `1:1` into `fsr_src_rt` (`fw × fh`) using `ScaleMode::Stretch` and `sharpness = 0.0`.
    2. *`FSR_EASU_PS_HLSL` (12-Tap Edge-Adaptive Spatial Upsampling):* Uses hardware `GatherRed`/`GatherGreen`/`GatherBlue` (`sampLinear` with `CLAMP`) across the 12-tap cross (`b, c, e, f, g, h, i, j, k, l, n, o`), computes local gradient direction/anisotropy across inner crosses (`f, g, j, k`), evaluates a rotated elliptical Lanczos2 kernel, and clamps to the inner 2×2 min/max bounds to eliminate ringing.
    3. *Optional `FSR_RCAS_PS_HLSL` (5-Tap Robust Contrast-Adaptive Sharpening):* When `sharpness > 0.001`, `EASU` renders into `fsr_easu_rt` and `RCAS` sharpens the upscaled pixels onto the backbuffer using luma noise attenuation (`nz`) and bounded negative lobe clamping (`-0.1875`). When `sharpness <= 0.001`, `EASU` renders directly to the swapchain backbuffer.
- **Hazard & Error Safety:**
  - Unbinds RTVs (`OMSetRenderTargets(None, None)`) and SRVs (`PSSetShaderResources(0, [None; 3])`) between passes.
  - If intermediate texture allocation fails, logs a warning and falls back cleanly to single-pass rendering on the backbuffer.

### 2.3 SDL3 Recording Device Picker & Hot-Swap (`crates/gnsh-media/src/audio.rs`)

- **`list_recording_devices() -> Vec<String>`:** Queries `SDL_GetAudioRecordingDevices` after `ensure_sdl_audio_init()`, resolves trimmed non-empty device names with deduplication, and always calls `SDL_free`.
- **`MicCapture::new_for_device(device_name: Option<&str>)` & `switch_device(&mut self, device_name: Option<&str>)`:** Opens a 48 kHz stereo `S16` capture stream targeting the named device (falling back to `SDL_AUDIO_DEVICE_DEFAULT_RECORDING` with a warning if unavailable), preserving `self.muted` across live stream switches and cleanly destroying the previous `SDL_AudioStream`.

### 2.4 Persistence, Legacy Migration, and `gnsh-app` UI (`storage.rs`, `stream.rs`, `ui.rs`, `main.rs`)

- **`StoredSettings` (`crates/gnsh-app/src/storage.rs`):**
  - Adds `filter_mode: VideoFilterMode` (default `Bilinear`) and `mic_device: Option<String>` (default `None`, normalized by trim / non-empty filter).
  - *Backward-compatible migration:* When loading `settings.json` that omits `"filter_mode"`, sets `filter_mode = VideoFilterMode::Cas` if `sharpness_pct > 0`, otherwise `VideoFilterMode::Bilinear`.
- **Live Controls & Hotplug (`crates/gnsh-app/src/ui.rs`, `stream.rs`, `main.rs`):**
  - Home top bar and Streaming HUD share `render_filter_controls` (`Filter: Bilinear | CAS | FSR 1.0` + unified `0..=100%` slider labeled `CAS` / `RCAS` / `Sharp` and disabled when `Bilinear` is active) and `Mic:` combo box alongside `Audio:`.
  - Streaming HUD telemetry badge reports `CAS: N%`, `FSR 1.0 (RCAS N%)`, or `FSR 1.0`.
  - `SDL_EVENT_AUDIO_DEVICE_ADDED` / `REMOVED` / `GAMEPAD_*` refreshes both `audio_devices` and `mic_devices`.

---

## 3. Verification Plan

1. **Shader Compilation & Math Tests (`gnsh-media`):** Compile `FSR_EASU_PS_HLSL` and `FSR_RCAS_PS_HLSL` (`ps_5_0`) via `D3DCompile`; unit-test `should_run_fsr_easu` bypass decisions and `compute_rcas_bounds` pixel clamping across `Fit`/`Zoom`/`Stretch`.
2. **Audio Enumeration Tests (`gnsh-media`):** Unit-test `list_recording_devices()` returns clean non-empty names.
3. **Persistence & Migration Tests (`gnsh-app`):** Unit-test `filter_mode`/`mic_device` roundtrip, whitespace trimming, and legacy `settings.json` migration (`sharpness_pct > 0` without `filter_mode` vs `0`).
4. **Workspace Gates:** Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo build --release -p gnsh-app` (`--help` smoke test).
