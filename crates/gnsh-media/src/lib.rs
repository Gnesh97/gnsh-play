pub mod annex_b;
pub mod audio;
pub mod d3d11_renderer;
pub mod d3d12_renderer;
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unused_imports,
    clippy::all
)]
pub(crate) mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
pub mod recorder;
pub mod renderer;
pub mod screenshot;
pub mod video_decoder;
pub mod vulkan_renderer;

pub use annex_b::{
    build_capture_path, contains_keyframe_nal, default_screenshot_dir, default_video_dir,
    extract_parameter_sets, is_header_only_annex_b,
};
pub use audio::{
    AudioOutput, HapticsAudioOutput, MicCapture, list_playback_devices, list_recording_devices,
    volume_pct_to_gain,
};
pub use d3d11_renderer::D3D11Renderer;
pub use d3d12_renderer::D3D12Renderer;
pub use recorder::{
    BufferedPacket, BufferedPacketKind, MAX_REPLAY_BUFFER_SECS, MIN_REPLAY_BUFFER_SECS, MkvMuxer,
    ReplayClipSnapshot, ReplayRingBuffer,
};
pub use renderer::{
    RendererBackend, ScaleMode, UiMeshDraw, UiOverlayRenderer, UiTextureWrap, VideoFilterMode,
    VideoRenderOptions, VideoRenderer,
};
pub use screenshot::{
    ScreenshotPixelFormat, ScreenshotPixels, extract_screenshot_pixels, save_screenshot_png,
};
pub use video_decoder::{
    D3D11Frame, D3D12Frame, DecodedFrame, DecoderHardware, MediaCodec, VideoDecoder, VulkanFrame,
};
pub use vulkan_renderer::{VulkanPlaceboRenderer, VulkanVideoDevice, compute_placebo_target_rect};
