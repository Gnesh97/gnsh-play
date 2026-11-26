# Image Quality & Stream Diagnostics Pack Design

Date: 2026-11-26
Status: Approved

## Problem Statement

On 1080p SDR displays, PS5 Remote Play streams show softness and motion compression artifacts/banding even at `100,000 kbps`:
1. PS5's hardware VCN encoder caps real output around `45-55 Mbps` in fast motion and `chiaki-ng`'s `LaunchSpec` enables `adaptiveStreamMode: "resize"`, which can silently lower stream resolution or bitrate on transient loss without HUD visibility.
2. `gnsh-media`'s D3D11/D3D12 pixel shaders sample 4:2:0 chroma without MPEG/H.264/HEVC `LEFT` chroma phase alignment and hardcode BT.709 Limited Range instead of honoring FFmpeg `AVFrame.colorspace` and `AVFrame.color_range`.
3. There is no GPU debanding pass for 8-bit macroblock/gradient banding and no live image tuning (`Vibrance`, `Contrast`, `Gamma`, `HDR->SDR Exposure`).

## Architecture

### 1. Decoded Frame Metadata & Render Options (`gnsh-media`)
- **`YuvColorMatrix` (`Bt601`, `Bt709`, `Bt2020`) & `FrameColorInfo { matrix, full_range }`**:
  - Extracted from `AVFrame.colorspace` and `AVFrame.color_range` inside `video_decoder.rs` (copied onto `sw_frame` on `D3D11Transfer` fallback), defaulting `UNSPECIFIED` to `Bt709` (SDR) or `Bt2020` (HDR) and `Limited` range.
  - Stored on `D3D11Frame`, `D3D12Frame`, `VulkanFrame`, and `Software*` variants; exposed via `DecodedFrame::dimensions()`, `DecodedFrame::color_info()`, and `DecodedFrame::bit_depth()`.
- **`VideoImageAdjustments`**:
  - `deband: bool` (default `false`)
  - `vibrance: f32` (`-0.5..=1.0`, default `0.0`)
  - `contrast: f32` (`0.5..=1.5`, default `1.0`)
  - `gamma: f32` (`0.7..=1.4`, default `1.0`)
  - `hdr_sdr_exposure: f32` (`0.5..=2.0`, default `1.0`)
  - Sanitized via `VideoImageAdjustments::sanitized(self)` and passed inside `VideoRenderOptions`.

### 2. Renderer Pipelines
- **D3D11 & D3D12 (`d3d11_renderer.rs`, `d3d12_renderer.rs`)**:
  - Extend `VideoParams` constant buffer with `color_grade: [f32; 4]` and `signal_params: [f32; 4]`.
  - Apply MPEG/H.264/HEVC `LEFT` 4:2:0 chroma sample offset (`+0.5 / tex_w` in U clamped to active bounds).
  - Dispatch `BT.601`, `BT.709`, or `BT.2020` YUV->RGB matrices and `Limited` vs `Full` range scaling dynamically from `FrameColorInfo`.
  - When `deband` is enabled, run a 4-tap thresholded debanding filter with triangular hash dither before CAS sharpening.
  - Apply `hdr_sdr_exposure` in `pq_bt2020_to_sdr_srgb` and `apply_image_grade` (`gamma`, `contrast`, `vibrance`) on SDR outputs (`FSR 1.0` keeps neutral source->EASU target and applies grading once in `FSR_RCAS_PS_HLSL` or fallback pass? Wait: grading in `draw_source_to_target` even when FSR runs? Wait—let's check FSR path!).
- **Vulkan / `libplacebo` (`vulkan_renderer.rs`)**:
  - Populate `pl_color_repr` from `FrameColorInfo` (`sys` and `levels`).
  - Enable single-iteration `pl_deband_params` (`grain = 0.0`) when `deband` is enabled.
  - Extend `CAS_HOOK_GLSL` (`//!HOOK PRE_OUTPUT`) with `vibrance`, `contrast`, `gamma`, and `exposure` dynamic parameters matching the HLSL formulas.

### 3. Persistence, HUD Diagnostics & Controls (`gnsh-app`)
- **`StoredSettings`**: Add `deband`, `vibrance_pct` (`-50..=100`), `contrast_pct` (`50..=150`), `gamma_pct` (`70..=140`), and `hdr_sdr_exposure_pct` (`50..=200`) with `#[serde(default)]` and backward-compatible normalization (`0` -> `100` for contrast/gamma/exposure).
- **Streaming HUD**: Display decoded resolution `{w}x{h}` in the status pill (with amber warning badge when height drops below requested preset) and bit depth / color space + image adjustments in the hover tooltip. Add compact live image controls + `Reset` button to the Quick Settings drawer.
- **Settings Modal & Gamepad Navigation**: Add `Deband` (+ `Reset Image`), `Vibrance`, `Contrast`, `Gamma`, and `HDR->SDR Exposure` rows to `Settings -> Video` with `ScrollArea` wrapping and matching `gamepad_nav.rs` row indices (`17` / `20` rows).

## Verification Plan
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (unit tests for `FrameColorInfo`, `VideoImageAdjustments`, `StoredSettings`, `gamepad_nav`, and headless D3D11 / D3D12 / Vulkan shader + pixel readback tests)
- `scripts\stage-dist.ps1` and live `PrintWindow` capture verification on D3D11 and Vulkan.
