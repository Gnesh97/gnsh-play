use crate::video_decoder::DecodedFrame;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Graphics backend preference for rendering and decode interop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RendererBackend {
    #[default]
    Auto,
    D3D11,
    Vulkan,
    D3D12,
}

impl RendererBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::D3D11 => "Direct3D 11",
            Self::Vulkan => "Vulkan",
            Self::D3D12 => "Direct3D 12",
        }
    }
}

/// Aspect-ratio presentation mode for decoded video frames inside the render viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScaleMode {
    #[default]
    Fit,
    Zoom,
    Stretch,
}

/// Spatial video scaling and sharpening filter mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VideoFilterMode {
    #[default]
    Bilinear,
    Cas,
    Fsr1,
}

/// Non-destructive color, deband, and tone-mapping adjustments applied during video presentation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoImageAdjustments {
    pub deband: bool,
    /// Smart saturation boost (`-0.5..=1.0`, `0.0` = neutral).
    pub vibrance: f32,
    /// Contrast multiplier (`0.5..=1.5`, `1.0` = neutral).
    pub contrast: f32,
    /// Gamma exponent (`0.7..=1.4`, `1.0` = neutral).
    pub gamma: f32,
    /// Exposure multiplier when tone-mapping 10-bit HDR10 onto SDR displays (`0.5..=2.0`, `1.0` = neutral).
    pub hdr_sdr_exposure: f32,
}

impl Default for VideoImageAdjustments {
    fn default() -> Self {
        Self {
            deband: false,
            vibrance: 0.0,
            contrast: 1.0,
            gamma: 1.0,
            hdr_sdr_exposure: 1.0,
        }
    }
}

impl VideoImageAdjustments {
    /// Clamps every parameter to safe shader bounds and replaces non-finite inputs with defaults.
    pub fn sanitized(self) -> Self {
        let clamp_finite = |val: f32, min: f32, max: f32, fallback: f32| -> f32 {
            if val.is_finite() {
                val.clamp(min, max)
            } else {
                fallback
            }
        };
        Self {
            deband: self.deband,
            vibrance: clamp_finite(self.vibrance, -0.5, 1.0, 0.0),
            contrast: clamp_finite(self.contrast, 0.5, 1.5, 1.0),
            gamma: clamp_finite(self.gamma, 0.7, 1.4, 1.0),
            hdr_sdr_exposure: clamp_finite(self.hdr_sdr_exposure, 0.5, 2.0, 1.0),
        }
    }

    /// Returns `true` when vibrance, contrast, and gamma are effectively identity.
    pub fn is_grade_neutral(self) -> bool {
        let s = self.sanitized();
        s.vibrance.abs() <= 1e-4
            && (s.contrast - 1.0).abs() <= 1e-4
            && (s.gamma - 1.0).abs() <= 1e-4
    }
}

/// Per-frame rendering parameters passed to any `VideoRenderer` backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRenderOptions {
    pub filter_mode: VideoFilterMode,
    pub sharpness: f32,
    pub scale_mode: ScaleMode,
    pub image: VideoImageAdjustments,
}

impl Default for VideoRenderOptions {
    fn default() -> Self {
        Self {
            filter_mode: VideoFilterMode::Bilinear,
            sharpness: 0.0,
            scale_mode: ScaleMode::Fit,
            image: VideoImageAdjustments::default(),
        }
    }
}

/// Texture wrap mode for UI overlay meshes painted onto `Vulkan` or `D3D12` swapchains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiTextureWrap {
    ClampToEdge,
    Repeat,
    MirroredRepeat,
}

/// Draw parameters for one scissored egui mesh rendered over a `UiOverlayRenderer`.
pub struct UiMeshDraw<'a> {
    pub tex_id: u64,
    pub linear_filter: bool,
    pub wrap: UiTextureWrap,
    pub vertices_bytes: &'a [u8],
    pub indices: &'a [u32],
    pub scissor: [i32; 4],
    pub screen_size_pts: [f32; 2],
    pub hdr_paper_white_nits: f32,
}

/// Shared safe UI texture & mesh overlay interface implemented by `VulkanPlaceboRenderer` and `D3D12Renderer`.
pub trait UiOverlayRenderer {
    fn upsert_ui_texture(&mut self, id: u64, w: u32, h: u32, rgba: &[u8]) -> Result<()>;
    fn update_ui_texture_region(
        &mut self,
        id: u64,
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        rgba: &[u8],
    ) -> Result<()>;
    fn free_ui_texture(&mut self, id: u64);
    fn draw_ui_mesh(&mut self, draw: UiMeshDraw<'_>) -> Result<()>;
}

/// Backend-agnostic video presentation interface (implemented by `D3D11Renderer`, `VulkanPlaceboRenderer`, `D3D12Renderer`).
pub trait VideoRenderer {
    fn viewport_size(&self) -> (u32, u32);
    fn resize(&mut self, width: u32, height: u32) -> Result<()>;
    fn is_display_hdr(&self) -> bool;
    fn is_hdr_output_active(&self) -> bool;
    fn set_hdr_output(&mut self, enable: bool) -> Result<bool>;
    fn wait_for_frame_latency(&self, timeout_ms: u32);
    fn clear_video_cache(&mut self);
    fn begin_frame(&self, clear_rgba: [f32; 4]);
    fn draw_video_frame(&mut self, frame: &DecodedFrame, options: VideoRenderOptions)
    -> Result<()>;
    fn present(&self, vsync: bool) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_adjustments_sanitize_and_detect_neutral() {
        let def = VideoImageAdjustments::default();
        assert!(def.is_grade_neutral());
        assert!(!def.deband);

        let dirty = VideoImageAdjustments {
            deband: true,
            vibrance: f32::NAN,
            contrast: 3.0,
            gamma: 0.1,
            hdr_sdr_exposure: f32::INFINITY,
        }
        .sanitized();
        assert!(dirty.deband);
        assert_eq!(dirty.vibrance, 0.0);
        assert_eq!(dirty.contrast, 1.5);
        assert_eq!(dirty.gamma, 0.7);
        assert_eq!(dirty.hdr_sdr_exposure, 1.0);
        assert!(!dirty.is_grade_neutral());
    }
}
