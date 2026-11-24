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

/// Per-frame rendering parameters passed to any `VideoRenderer` backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRenderOptions {
    pub filter_mode: VideoFilterMode,
    pub sharpness: f32,
    pub scale_mode: ScaleMode,
}

impl Default for VideoRenderOptions {
    fn default() -> Self {
        Self {
            filter_mode: VideoFilterMode::Bilinear,
            sharpness: 0.0,
            scale_mode: ScaleMode::Fit,
        }
    }
}

/// Backend-agnostic video presentation interface (implemented by `D3D11Renderer`, extensible to Vulkan/`libplacebo`).
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
