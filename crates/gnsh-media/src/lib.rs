pub mod audio;
pub mod d3d11_renderer;
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all
)]
pub(crate) mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}
pub mod renderer;
pub mod video_decoder;

pub use audio::{
    AudioOutput, HapticsAudioOutput, MicCapture, list_playback_devices, list_recording_devices,
    volume_pct_to_gain,
};
pub use d3d11_renderer::D3D11Renderer;
pub use renderer::{ScaleMode, VideoFilterMode, VideoRenderOptions, VideoRenderer};
pub use video_decoder::{D3D11Frame, DecodedFrame, MediaCodec, VideoDecoder};
