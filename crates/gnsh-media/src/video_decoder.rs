use crate::annex_b::is_header_only_annex_b;
use crate::ffi;
use anyhow::{Context, Result, bail};
use std::ffi::CStr;
use std::os::raw::c_void;
use std::sync::Arc;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, ID3D11Device, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};
use windows::core::Interface as _;

const MAX_VIDEO_PACKET_BYTES: usize = 16 * 1024 * 1024;
const MAX_VIDEO_DIMENSION: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCodec {
    H264,
    Hevc,
    HevcHdr,
}

pub(crate) struct AvFrameGuard {
    pub(crate) ptr: *mut ffi::AVFrame,
}

impl AvFrameGuard {
    pub(crate) fn as_ptr(&self) -> *mut ffi::AVFrame {
        self.ptr
    }
}

unsafe impl Send for AvFrameGuard {}
unsafe impl Sync for AvFrameGuard {}

impl Drop for AvFrameGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                ffi::av_frame_free(&mut self.ptr);
            }
        }
    }
}

/// Color matrix standard signalled by FFmpeg for a decoded YUV frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YuvColorMatrix {
    Bt601,
    #[default]
    Bt709,
    Bt2020,
}

/// Decoded frame color space & range metadata preserved from FFmpeg `AVFrame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameColorInfo {
    pub matrix: YuvColorMatrix,
    pub full_range: bool,
}

impl Default for FrameColorInfo {
    fn default() -> Self {
        Self {
            matrix: YuvColorMatrix::Bt709,
            full_range: false,
        }
    }
}

impl FrameColorInfo {
    /// Maps FFmpeg `AVColorSpace` and `AVColorRange` values with stream SDR/HDR fallback.
    pub fn from_av_values(
        colorspace: ffi::AVColorSpace,
        color_range: ffi::AVColorRange,
        is_hdr: bool,
    ) -> Self {
        let matrix = match colorspace {
            ffi::AVColorSpace_AVCOL_SPC_BT470BG
            | ffi::AVColorSpace_AVCOL_SPC_SMPTE170M
            | ffi::AVColorSpace_AVCOL_SPC_SMPTE240M => YuvColorMatrix::Bt601,
            ffi::AVColorSpace_AVCOL_SPC_BT2020_NCL | ffi::AVColorSpace_AVCOL_SPC_BT2020_CL => {
                YuvColorMatrix::Bt2020
            }
            ffi::AVColorSpace_AVCOL_SPC_BT709 => YuvColorMatrix::Bt709,
            _ if is_hdr => YuvColorMatrix::Bt2020,
            _ => YuvColorMatrix::Bt709,
        };
        let full_range = color_range == ffi::AVColorRange_AVCOL_RANGE_JPEG;
        Self { matrix, full_range }
    }

    /// Compact HUD tooltip label describing matrix and quantization range.
    pub fn label(self) -> &'static str {
        match (self.matrix, self.full_range) {
            (YuvColorMatrix::Bt601, false) => "BT.601 Lim",
            (YuvColorMatrix::Bt601, true) => "BT.601 Full",
            (YuvColorMatrix::Bt709, false) => "BT.709 Lim",
            (YuvColorMatrix::Bt709, true) => "BT.709 Full",
            (YuvColorMatrix::Bt2020, false) => "BT.2020 Lim",
            (YuvColorMatrix::Bt2020, true) => "BT.2020 Full",
        }
    }
}

pub struct D3D11Frame {
    pub texture: ID3D11Texture2D,
    pub slice_index: u32,
    pub width: u32,
    pub height: u32,
    pub pts: i64,
    pub is_hdr: bool,
    pub color_info: FrameColorInfo,
    // Retains FFmpeg's AVBufferRef until the texture slice is finished rendering.
    pub(crate) av_frame: Arc<AvFrameGuard>,
}

pub struct D3D12Frame {
    pub texture: ID3D12Resource,
    pub subresource_index: u32,
    pub fence: Option<ID3D12Fence>,
    pub fence_value: u64,
    pub width: u32,
    pub height: u32,
    pub pts: i64,
    pub is_hdr: bool,
    pub color_info: FrameColorInfo,
    // Keeps FFmpeg's D3D12VA resource pool slot alive until the graphics queue finishes reading it.
    pub(crate) _av_frame: Arc<AvFrameGuard>,
}

impl D3D12Frame {
    pub(crate) fn pin_guard(&self) -> Arc<AvFrameGuard> {
        Arc::clone(&self._av_frame)
    }
}

pub struct VulkanFrame {
    pub width: u32,
    pub height: u32,
    pub pts: i64,
    pub is_hdr: bool,
    pub color_info: FrameColorInfo,
    // Retains FFmpeg's AVVkFrame pool slot until libplacebo signals timeline semaphore completion.
    pub(crate) av_frame: Arc<AvFrameGuard>,
}

/// Hardware acceleration mode for `VideoDecoder`.
#[derive(Debug, Clone)]
pub enum DecoderHardware {
    None,
    D3D11ZeroCopy(ID3D11Device),
    D3D11Transfer,
    D3D12ZeroCopy(ID3D12Device),
    VulkanZeroCopy(Arc<crate::vulkan_renderer::VulkanVideoDevice>),
}

pub enum DecodedFrame {
    D3D11(D3D11Frame),
    D3D12(D3D12Frame),
    Vulkan(VulkanFrame),
    SoftwareYuv420 {
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
        y_stride: usize,
        u_stride: usize,
        v_stride: usize,
        width: u32,
        height: u32,
        pts: i64,
        is_hdr: bool,
        color_info: FrameColorInfo,
    },
    SoftwareNv12 {
        y: Vec<u8>,
        uv: Vec<u8>,
        y_stride: usize,
        uv_stride: usize,
        width: u32,
        height: u32,
        pts: i64,
        is_hdr: bool,
        color_info: FrameColorInfo,
    },
    SoftwareP010 {
        y: Vec<u8>,
        uv: Vec<u8>,
        y_stride: usize,
        uv_stride: usize,
        width: u32,
        height: u32,
        pts: i64,
        is_hdr: bool,
        color_info: FrameColorInfo,
    },
}

impl DecodedFrame {
    /// Returns the active coded video dimensions `(width, height)` in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::D3D11(frame) => (frame.width, frame.height),
            Self::D3D12(frame) => (frame.width, frame.height),
            Self::Vulkan(frame) => (frame.width, frame.height),
            Self::SoftwareYuv420 { width, height, .. }
            | Self::SoftwareNv12 { width, height, .. }
            | Self::SoftwareP010 { width, height, .. } => (*width, *height),
        }
    }

    /// Returns color matrix and quantization range reported by FFmpeg for this frame.
    pub fn color_info(&self) -> FrameColorInfo {
        match self {
            Self::D3D11(frame) => frame.color_info,
            Self::D3D12(frame) => frame.color_info,
            Self::Vulkan(frame) => frame.color_info,
            Self::SoftwareYuv420 { color_info, .. }
            | Self::SoftwareNv12 { color_info, .. }
            | Self::SoftwareP010 { color_info, .. } => *color_info,
        }
    }

    /// Returns bit depth per channel (`10` for HDR10/P010 streams, `8` for SDR).
    pub fn bit_depth(&self) -> u8 {
        if self.is_hdr() || matches!(self, Self::SoftwareP010 { .. }) {
            10
        } else {
            8
        }
    }

    /// Returns the FFmpeg presentation timestamp carried through decoding.
    pub fn pts(&self) -> i64 {
        match self {
            Self::D3D11(frame) => frame.pts,
            Self::D3D12(frame) => frame.pts,
            Self::Vulkan(frame) => frame.pts,
            Self::SoftwareYuv420 { pts, .. }
            | Self::SoftwareNv12 { pts, .. }
            | Self::SoftwareP010 { pts, .. } => *pts,
        }
    }

    /// Returns whether this decoded frame carries an HDR10 signal.
    pub fn is_hdr(&self) -> bool {
        match self {
            Self::D3D11(frame) => frame.is_hdr,
            Self::D3D12(frame) => frame.is_hdr,
            Self::Vulkan(frame) => frame.is_hdr,
            Self::SoftwareYuv420 { is_hdr, .. }
            | Self::SoftwareNv12 { is_hdr, .. }
            | Self::SoftwareP010 { is_hdr, .. } => *is_hdr,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HwKind {
    None,
    D3D11ZeroCopy,
    D3D11Transfer,
    D3D12ZeroCopy,
    VulkanZeroCopy,
}

pub struct VideoDecoder {
    codec: MediaCodec,
    ctx: *mut ffi::AVCodecContext,
    hw_device_ctx: *mut ffi::AVBufferRef,
    pkt: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    sw_frame: *mut ffi::AVFrame,
    hw_active: bool,
    hw_kind: HwKind,
    decoded_any: bool,
    pending_header: Vec<u8>,
    bootstrap_packets: Vec<(Vec<u8>, i64)>,
    bootstrap_bytes: usize,
}

const MAX_PENDING_HEADER_BYTES: usize = 64 * 1024;
const MAX_BOOTSTRAP_PACKETS: usize = 16;
const MAX_BOOTSTRAP_BYTES: usize = 8 * 1024 * 1024;

unsafe impl Send for VideoDecoder {}

enum DrainOutcome {
    Done,
    FallbackRetry,
}

impl VideoDecoder {
    pub fn new(codec: MediaCodec, d3d11_device: Option<ID3D11Device>) -> Result<Self> {
        let hw = match d3d11_device {
            Some(dev) => DecoderHardware::D3D11ZeroCopy(dev),
            None => DecoderHardware::None,
        };
        Self::new_with_mode(codec, hw)
    }

    pub fn new_with_mode(codec: MediaCodec, hw: DecoderHardware) -> Result<Self> {
        let try_d3d11_transfer = matches!(
            hw,
            DecoderHardware::VulkanZeroCopy(_) | DecoderHardware::D3D12ZeroCopy(_)
        );
        if !matches!(hw, DecoderHardware::None) {
            match Self::open_internal(codec, hw) {
                Ok(dec) => return Ok(dec),
                Err(err) => tracing::warn!("Hardware video decoder init failed: {err:#}"),
            }
        }
        if try_d3d11_transfer {
            match Self::open_internal(codec, DecoderHardware::D3D11Transfer) {
                Ok(dec) => return Ok(dec),
                Err(err) => tracing::warn!("D3D11Transfer fallback init failed: {err:#}"),
            }
        }
        Self::open_internal(codec, DecoderHardware::None)
    }

    pub fn is_hw_active(&self) -> bool {
        self.hw_active
    }

    /// Flushes buffered reference frames in FFmpeg after severe packet corruption.
    pub fn flush(&mut self) {
        unsafe {
            ffi::avcodec_flush_buffers(self.ctx);
        }
    }

    pub fn decode(&mut self, annex_b: &[u8]) -> Result<Vec<DecodedFrame>> {
        self.decode_with_pts(annex_b, 0)
    }

    /// Decodes one Annex-B unit while tagging `AVPacket.pts` so output frames preserve lag identity.
    pub fn decode_with_pts(&mut self, annex_b: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if annex_b.is_empty() {
            return Ok(Vec::new());
        }
        if annex_b.len() > MAX_VIDEO_PACKET_BYTES {
            bail!(
                "video packet size {} exceeds 16 MiB safety limit",
                annex_b.len()
            );
        }
        let _ = i32::try_from(annex_b.len()).context("video packet size exceeds i32")?;

        if is_header_only_annex_b(annex_b, self.codec) {
            if annex_b.len() <= MAX_PENDING_HEADER_BYTES {
                if self.pending_header.len() + annex_b.len() > MAX_PENDING_HEADER_BYTES {
                    self.pending_header.clear();
                }
                self.pending_header.extend_from_slice(annex_b);
            }
            return Ok(Vec::new());
        }

        let combined: Vec<u8>;
        let packet_bytes: &[u8] = if self.pending_header.is_empty() {
            annex_b
        } else {
            let hdr = std::mem::take(&mut self.pending_header);
            let total = hdr.len() + annex_b.len();
            if total > MAX_VIDEO_PACKET_BYTES {
                bail!("combined video packet size {total} exceeds 16 MiB safety limit");
            }
            let _ = i32::try_from(total).context("combined video packet size exceeds i32")?;
            let mut buf = Vec::with_capacity(total);
            buf.extend_from_slice(&hdr);
            buf.extend_from_slice(annex_b);
            combined = buf;
            &combined
        };

        if !self.decoded_any
            && self.bootstrap_packets.len() < MAX_BOOTSTRAP_PACKETS
            && self.bootstrap_bytes.saturating_add(packet_bytes.len()) <= MAX_BOOTSTRAP_BYTES
        {
            self.bootstrap_bytes += packet_bytes.len();
            self.bootstrap_packets.push((packet_bytes.to_vec(), pts));
        }

        let mut frames = Vec::new();
        if matches!(
            self.decode_single_packet(packet_bytes, pts, &mut frames)?,
            DrainOutcome::FallbackRetry
        ) {
            self.replay_bootstrap(&mut frames)?;
        }

        Ok(frames)
    }

    fn replay_bootstrap(&mut self, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        for _attempt in 0..3 {
            frames.clear();
            let buffered = self.bootstrap_packets.clone();
            let mut retry = false;
            for (pkt_data, pkt_pts) in &buffered {
                if matches!(
                    self.decode_single_packet(pkt_data, *pkt_pts, frames)?,
                    DrainOutcome::FallbackRetry
                ) {
                    retry = true;
                    break;
                }
            }
            if !retry {
                return Ok(());
            }
        }
        Ok(())
    }

    fn decode_single_packet(
        &mut self,
        annex_b: &[u8],
        pts: i64,
        frames: &mut Vec<DecodedFrame>,
    ) -> Result<DrainOutcome> {
        let pkt_len = i32::try_from(annex_b.len()).context("video packet size exceeds i32")?;
        let mut send_err = unsafe {
            (*self.pkt).data = annex_b.as_ptr().cast_mut();
            (*self.pkt).size = pkt_len;
            (*self.pkt).pts = pts;
            (*self.pkt).dts = pts;
            ffi::avcodec_send_packet(self.ctx, self.pkt)
        };

        if send_err == ffi::GNSH_AVERROR_EAGAIN {
            if matches!(self.drain_frames(frames)?, DrainOutcome::FallbackRetry) {
                return Ok(DrainOutcome::FallbackRetry);
            }
            send_err = unsafe {
                (*self.pkt).data = annex_b.as_ptr().cast_mut();
                (*self.pkt).size = pkt_len;
                (*self.pkt).pts = pts;
                (*self.pkt).dts = pts;
                ffi::avcodec_send_packet(self.ctx, self.pkt)
            };
        }

        if send_err < 0 && send_err != ffi::GNSH_AVERROR_EAGAIN {
            if !self.decoded_any && self.fallback_next(&av_err_string(send_err))? {
                return Ok(DrainOutcome::FallbackRetry);
            }
            bail!("avcodec_send_packet failed: {}", av_err_string(send_err));
        }

        self.drain_frames(frames)
    }

    fn fallback_next(&mut self, reason: &str) -> Result<bool> {
        let next_hw = match self.hw_kind {
            HwKind::VulkanZeroCopy => {
                tracing::warn!(
                    "Vulkan Video unavailable ({reason}); switching to D3D11VA hardware transfer"
                );
                DecoderHardware::D3D11Transfer
            }
            HwKind::D3D12ZeroCopy => {
                tracing::warn!(
                    "D3D12VA unavailable ({reason}); switching to D3D11VA hardware transfer"
                );
                DecoderHardware::D3D11Transfer
            }
            HwKind::D3D11ZeroCopy | HwKind::D3D11Transfer => {
                tracing::warn!(
                    "D3D11VA unavailable ({reason}); switching to multi-threaded software decode"
                );
                DecoderHardware::None
            }
            HwKind::None => return Ok(false),
        };
        let mut next = Self::open_internal(self.codec, next_hw)?;
        next.pending_header = std::mem::take(&mut self.pending_header);
        next.bootstrap_packets = std::mem::take(&mut self.bootstrap_packets);
        next.bootstrap_bytes = std::mem::take(&mut self.bootstrap_bytes);
        *self = next;
        Ok(true)
    }

    fn drain_frames(&mut self, frames: &mut Vec<DecodedFrame>) -> Result<DrainOutcome> {
        loop {
            let r = unsafe { ffi::avcodec_receive_frame(self.ctx, self.frame) };
            if r == ffi::GNSH_AVERROR_EAGAIN || r == ffi::GNSH_AVERROR_EOF {
                return Ok(DrainOutcome::Done);
            }
            if r < 0 {
                if !self.decoded_any && self.fallback_next(&av_err_string(r))? {
                    return Ok(DrainOutcome::FallbackRetry);
                }
                bail!("avcodec_receive_frame failed: {}", av_err_string(r));
            }

            let is_hdr_codec = self.codec == MediaCodec::HevcHdr;
            let fmt = unsafe { (*self.frame).format };
            if !self.decoded_any {
                let unexpected_hw = match self.hw_kind {
                    HwKind::VulkanZeroCopy => fmt != ffi::AVPixelFormat_AV_PIX_FMT_VULKAN,
                    HwKind::D3D12ZeroCopy => fmt != ffi::AVPixelFormat_AV_PIX_FMT_D3D12,
                    HwKind::D3D11ZeroCopy | HwKind::D3D11Transfer => {
                        fmt != ffi::AVPixelFormat_AV_PIX_FMT_D3D11
                    }
                    HwKind::None => false,
                };
                if unexpected_hw {
                    unsafe {
                        ffi::av_frame_unref(self.frame);
                    }
                    if self.fallback_next(&format!("FFmpeg fell back to pix_fmt {fmt}"))? {
                        return Ok(DrainOutcome::FallbackRetry);
                    }
                }
            }

            let transfer_hw = self.hw_kind == HwKind::D3D11Transfer;
            let decoded = if transfer_hw && fmt == ffi::AVPixelFormat_AV_PIX_FMT_D3D11 {
                let xfer_err =
                    unsafe { ffi::av_hwframe_transfer_data(self.sw_frame, self.frame, 0) };
                if xfer_err < 0 {
                    unsafe {
                        ffi::av_frame_unref(self.sw_frame);
                        ffi::av_frame_unref(self.frame);
                    }
                    if !self.decoded_any && self.fallback_next(&av_err_string(xfer_err))? {
                        return Ok(DrainOutcome::FallbackRetry);
                    }
                    bail!(
                        "av_hwframe_transfer_data failed: {}",
                        av_err_string(xfer_err)
                    );
                }
                unsafe {
                    (*self.sw_frame).pts = (*self.frame).pts;
                    (*self.sw_frame).colorspace = (*self.frame).colorspace;
                    (*self.sw_frame).color_range = (*self.frame).color_range;
                }
                let out = unsafe { extract_frame(self.sw_frame, is_hdr_codec, true)? };
                unsafe {
                    ffi::av_frame_unref(self.sw_frame);
                }
                out
            } else {
                unsafe { extract_frame(self.frame, is_hdr_codec, transfer_hw)? }
            };
            if let Some(decoded) = decoded {
                if !self.decoded_any {
                    self.decoded_any = true;
                    self.bootstrap_packets = Vec::new();
                    self.bootstrap_bytes = 0;
                }
                frames.push(decoded);
            }
            unsafe {
                ffi::av_frame_unref(self.frame);
            }
        }
    }

    fn open_internal(codec: MediaCodec, hw: DecoderHardware) -> Result<Self> {
        let codec_id = match codec {
            MediaCodec::H264 => ffi::AVCodecID_AV_CODEC_ID_H264,
            MediaCodec::Hevc | MediaCodec::HevcHdr => ffi::AVCodecID_AV_CODEC_ID_HEVC,
        };
        let av_codec = unsafe { ffi::avcodec_find_decoder(codec_id) };
        if av_codec.is_null() {
            bail!("FFmpeg decoder not found for {codec:?}");
        }

        let ctx = unsafe { ffi::avcodec_alloc_context3(av_codec) };
        if ctx.is_null() {
            bail!("avcodec_alloc_context3 returned null");
        }
        let pkt = unsafe { ffi::av_packet_alloc() };
        let frame = unsafe { ffi::av_frame_alloc() };
        let sw_frame = unsafe { ffi::av_frame_alloc() };
        if pkt.is_null() || frame.is_null() || sw_frame.is_null() {
            unsafe {
                if !pkt.is_null() {
                    ffi::av_packet_free(&mut { pkt });
                }
                if !frame.is_null() {
                    ffi::av_frame_free(&mut { frame });
                }
                if !sw_frame.is_null() {
                    ffi::av_frame_free(&mut { sw_frame });
                }
                ffi::avcodec_free_context(&mut { ctx });
            }
            bail!("failed allocating AVPacket/AVFrame");
        }

        let mut hw_device_ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let mut hw_active = false;
        let mut hw_kind = HwKind::None;

        unsafe {
            (*ctx).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*ctx).flags2 |= ffi::AV_CODEC_FLAG2_FAST as i32;
            // Disallow FF_THREAD_FRAME (1), which adds 1+ frame of decode buffering latency.
            (*ctx).thread_type = 2;
        }

        match hw {
            DecoderHardware::D3D11ZeroCopy(dev) => {
                match unsafe { create_d3d11va_device_ctx(&dev) } {
                    Ok(buf_ref) => {
                        unsafe {
                            (*ctx).hw_device_ctx = ffi::av_buffer_ref(buf_ref);
                            (*ctx).get_format = Some(select_d3d11_format);
                            (*ctx).thread_count = 1;
                        }
                        hw_device_ctx = buf_ref;
                        hw_active = true;
                        hw_kind = HwKind::D3D11ZeroCopy;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "D3D11VA device setup failed, using software decoder: {e:#}"
                        );
                        unsafe {
                            (*ctx).thread_count = 4;
                        }
                    }
                }
            }
            DecoderHardware::D3D11Transfer => {
                match unsafe { create_d3d11va_transfer_device_ctx() } {
                    Ok(buf_ref) => {
                        unsafe {
                            (*ctx).hw_device_ctx = ffi::av_buffer_ref(buf_ref);
                            (*ctx).get_format = Some(select_d3d11_format);
                            (*ctx).thread_count = 1;
                        }
                        hw_device_ctx = buf_ref;
                        hw_active = true;
                        hw_kind = HwKind::D3D11Transfer;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "D3D11VA transfer setup failed, using software decoder: {e:#}"
                        );
                        unsafe {
                            (*ctx).thread_count = 4;
                        }
                    }
                }
            }
            DecoderHardware::D3D12ZeroCopy(dev) => {
                match unsafe { create_d3d12va_device_ctx(&dev) } {
                    Ok(buf_ref) => {
                        unsafe {
                            (*ctx).hw_device_ctx = ffi::av_buffer_ref(buf_ref);
                            (*ctx).get_format = Some(select_d3d12_format);
                            (*ctx).thread_count = 1;
                        }
                        hw_device_ctx = buf_ref;
                        hw_active = true;
                        hw_kind = HwKind::D3D12ZeroCopy;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "D3D12VA device setup failed, using software decoder: {e:#}"
                        );
                        unsafe {
                            (*ctx).thread_count = 4;
                        }
                    }
                }
            }
            DecoderHardware::VulkanZeroCopy(vk_dev) => {
                let supported = match codec {
                    MediaCodec::H264 => vk_dev.supports_h264(),
                    MediaCodec::Hevc | MediaCodec::HevcHdr => vk_dev.supports_h265(),
                };
                if !supported {
                    unsafe {
                        ffi::av_packet_free(&mut { pkt });
                        ffi::av_frame_free(&mut { frame });
                        ffi::av_frame_free(&mut { sw_frame });
                        ffi::avcodec_free_context(&mut { ctx });
                    }
                    bail!("Vulkan Video does not advertise decode support for {codec:?}");
                }
                match unsafe { create_vulkan_device_ctx(&vk_dev) } {
                    Ok(buf_ref) => {
                        unsafe {
                            (*ctx).hw_device_ctx = ffi::av_buffer_ref(buf_ref);
                            (*ctx).get_format = Some(select_vulkan_format);
                            (*ctx).thread_count = 1;
                        }
                        hw_device_ctx = buf_ref;
                        hw_active = true;
                        hw_kind = HwKind::VulkanZeroCopy;
                    }
                    Err(e) => {
                        unsafe {
                            ffi::av_packet_free(&mut { pkt });
                            ffi::av_frame_free(&mut { frame });
                            ffi::av_frame_free(&mut { sw_frame });
                            ffi::avcodec_free_context(&mut { ctx });
                        }
                        return Err(e).context("Vulkan Video device setup failed");
                    }
                }
            }
            DecoderHardware::None => unsafe {
                (*ctx).thread_count = 4;
            },
        }

        let open_err = unsafe { ffi::avcodec_open2(ctx, av_codec, std::ptr::null_mut()) };
        if open_err < 0 {
            unsafe {
                if !hw_device_ctx.is_null() {
                    ffi::av_buffer_unref(&mut hw_device_ctx);
                }
                ffi::av_packet_free(&mut { pkt });
                ffi::av_frame_free(&mut { frame });
                ffi::av_frame_free(&mut { sw_frame });
                ffi::avcodec_free_context(&mut { ctx });
            }
            bail!("avcodec_open2 failed: {}", av_err_string(open_err));
        }

        Ok(Self {
            codec,
            ctx,
            hw_device_ctx,
            pkt,
            frame,
            sw_frame,
            hw_active,
            hw_kind,
            decoded_any: false,
            pending_header: Vec::new(),
            bootstrap_packets: Vec::new(),
            bootstrap_bytes: 0,
        })
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe {
            if !self.sw_frame.is_null() {
                ffi::av_frame_free(&mut self.sw_frame);
            }
            if !self.frame.is_null() {
                ffi::av_frame_free(&mut self.frame);
            }
            if !self.pkt.is_null() {
                ffi::av_packet_free(&mut self.pkt);
            }
            if !self.ctx.is_null() {
                ffi::avcodec_free_context(&mut self.ctx);
            }
            if !self.hw_device_ctx.is_null() {
                ffi::av_buffer_unref(&mut self.hw_device_ctx);
            }
        }
    }
}

unsafe fn create_d3d11va_transfer_device_ctx() -> Result<*mut ffi::AVBufferRef> {
    let mut hw_ref: *mut ffi::AVBufferRef = std::ptr::null_mut();
    let err = unsafe {
        ffi::av_hwdevice_ctx_create(
            &mut hw_ref,
            ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    if err < 0 || hw_ref.is_null() {
        bail!(
            "av_hwdevice_ctx_create(D3D11VA) failed: {}",
            av_err_string(err)
        );
    }
    Ok(hw_ref)
}

unsafe fn create_d3d11va_device_ctx(dev: &ID3D11Device) -> Result<*mut ffi::AVBufferRef> {
    if let Ok(mt) = dev.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }

    let mut hw_ref =
        unsafe { ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D11VA) };
    if hw_ref.is_null() {
        bail!("av_hwdevice_ctx_alloc(D3D11VA) returned null");
    }

    unsafe {
        let hw_dev_ctx = (*hw_ref).data.cast::<ffi::AVHWDeviceContext>();
        let d3d_ctx = (*hw_dev_ctx).hwctx.cast::<ffi::AVD3D11VADeviceContext>();
        (*d3d_ctx).device = dev.clone().into_raw().cast();
        (*d3d_ctx).BindFlags = (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;

        let init_err = ffi::av_hwdevice_ctx_init(hw_ref);
        if init_err < 0 {
            ffi::av_buffer_unref(&mut hw_ref);
            bail!(
                "av_hwdevice_ctx_init(D3D11VA) failed: {}",
                av_err_string(init_err)
            );
        }
    }

    Ok(hw_ref)
}

unsafe fn create_d3d12va_device_ctx(dev: &ID3D12Device) -> Result<*mut ffi::AVBufferRef> {
    let mut hw_ref =
        unsafe { ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_D3D12VA) };
    if hw_ref.is_null() {
        bail!("av_hwdevice_ctx_alloc(D3D12VA) returned null");
    }

    unsafe {
        let hw_dev_ctx = (*hw_ref).data.cast::<ffi::AVHWDeviceContext>();
        let d3d12_ctx = (*hw_dev_ctx).hwctx.cast::<ffi::AVD3D12VADeviceContext>();
        (*d3d12_ctx).device = dev.clone().into_raw().cast();

        let init_err = ffi::av_hwdevice_ctx_init(hw_ref);
        if init_err < 0 {
            ffi::av_buffer_unref(&mut hw_ref);
            bail!(
                "av_hwdevice_ctx_init(D3D12VA) failed: {}",
                av_err_string(init_err)
            );
        }
    }

    Ok(hw_ref)
}

unsafe extern "C" fn vulkan_hwdevice_free_cb(ctx: *mut ffi::AVHWDeviceContext) {
    if ctx.is_null() {
        return;
    }
    let ptr = unsafe {
        (*ctx)
            .user_opaque
            .cast::<Arc<crate::vulkan_renderer::VulkanVideoDevice>>()
    };
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr));
            (*ctx).user_opaque = std::ptr::null_mut();
        }
    }
}

unsafe extern "C" fn vulkan_lock_queue_cb(
    ctx: *mut ffi::AVHWDeviceContext,
    queue_family: u32,
    index: u32,
) {
    if ctx.is_null() {
        return;
    }
    let ptr = unsafe {
        (*ctx)
            .user_opaque
            .cast::<Arc<crate::vulkan_renderer::VulkanVideoDevice>>()
    };
    if !ptr.is_null() {
        let vk_dev = unsafe { &*ptr };
        vk_dev.lock_queue(queue_family, index);
    }
}

unsafe extern "C" fn vulkan_unlock_queue_cb(
    ctx: *mut ffi::AVHWDeviceContext,
    queue_family: u32,
    index: u32,
) {
    if ctx.is_null() {
        return;
    }
    let ptr = unsafe {
        (*ctx)
            .user_opaque
            .cast::<Arc<crate::vulkan_renderer::VulkanVideoDevice>>()
    };
    if !ptr.is_null() {
        let vk_dev = unsafe { &*ptr };
        vk_dev.unlock_queue(queue_family, index);
    }
}

unsafe fn create_vulkan_device_ctx(
    vk_dev: &Arc<crate::vulkan_renderer::VulkanVideoDevice>,
) -> Result<*mut ffi::AVBufferRef> {
    if vk_dev.queue_families.is_empty() || vk_dev.queue_families.len() > 64 {
        bail!(
            "invalid Vulkan queue family count {}",
            vk_dev.queue_families.len()
        );
    }
    let mut hw_ref =
        unsafe { ffi::av_hwdevice_ctx_alloc(ffi::AVHWDeviceType_AV_HWDEVICE_TYPE_VULKAN) };
    if hw_ref.is_null() {
        bail!("av_hwdevice_ctx_alloc(VULKAN) returned null");
    }

    unsafe {
        let hw_dev_ctx = (*hw_ref).data.cast::<ffi::AVHWDeviceContext>();
        let boxed_dev = Box::into_raw(Box::new(Arc::clone(vk_dev)));
        (*hw_dev_ctx).user_opaque = boxed_dev.cast();
        (*hw_dev_ctx).free = Some(vulkan_hwdevice_free_cb);

        let vk_ctx = (*hw_dev_ctx).hwctx.cast::<ffi::AVVulkanDeviceContext>();
        (*vk_ctx).get_proc_addr = (*vk_dev.vk).get_proc_addr;
        (*vk_ctx).inst = (*vk_dev.vk).instance;
        (*vk_ctx).phys_dev = (*vk_dev.vk).phys_device;
        (*vk_ctx).act_dev = (*vk_dev.vk).device;
        if !(*vk_dev.vk).features.is_null() {
            (*vk_ctx).device_features = *(*vk_dev.vk).features;
        }
        (*vk_ctx).enabled_inst_extensions = (*vk_dev.vk_inst).extensions;
        (*vk_ctx).nb_enabled_inst_extensions = (*vk_dev.vk_inst).num_extensions;
        (*vk_ctx).enabled_dev_extensions = (*vk_dev.vk).extensions;
        (*vk_ctx).nb_enabled_dev_extensions = (*vk_dev.vk).num_extensions;
        (*vk_ctx).lock_queue = Some(vulkan_lock_queue_cb);
        (*vk_ctx).unlock_queue = Some(vulkan_unlock_queue_cb);
        for (i, &qf) in vk_dev.queue_families.iter().enumerate() {
            (*vk_ctx).qf[i] = qf;
        }
        (*vk_ctx).nb_qf = vk_dev.queue_families.len() as i32;

        let init_err = ffi::av_hwdevice_ctx_init(hw_ref);
        if init_err < 0 {
            ffi::av_buffer_unref(&mut hw_ref);
            bail!(
                "av_hwdevice_ctx_init(VULKAN) failed: {}",
                av_err_string(init_err)
            );
        }
    }

    Ok(hw_ref)
}

unsafe fn select_hw_pix_fmt(
    pix_fmts: *const ffi::AVPixelFormat,
    preferred: ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut p = pix_fmts;
    while !p.is_null() && unsafe { *p } != ffi::AVPixelFormat_AV_PIX_FMT_NONE {
        if unsafe { *p } == preferred {
            return preferred;
        }
        p = unsafe { p.add(1) };
    }
    if !pix_fmts.is_null() {
        unsafe { *pix_fmts }
    } else {
        ffi::AVPixelFormat_AV_PIX_FMT_NONE
    }
}

unsafe extern "C" fn select_d3d11_format(
    _ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe { select_hw_pix_fmt(pix_fmts, ffi::AVPixelFormat_AV_PIX_FMT_D3D11) }
}

unsafe extern "C" fn select_d3d12_format(
    _ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe { select_hw_pix_fmt(pix_fmts, ffi::AVPixelFormat_AV_PIX_FMT_D3D12) }
}

unsafe extern "C" fn select_vulkan_format(
    _ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    unsafe { select_hw_pix_fmt(pix_fmts, ffi::AVPixelFormat_AV_PIX_FMT_VULKAN) }
}

unsafe fn extract_frame(
    src: *mut ffi::AVFrame,
    is_hdr: bool,
    preserve_semiplanar: bool,
) -> Result<Option<DecodedFrame>> {
    if src.is_null() {
        bail!("FFmpeg AVFrame pointer is null");
    }
    let fmt = unsafe { (*src).format };
    let width = unsafe { (*src).width.max(0) as u32 };
    let height = unsafe { (*src).height.max(0) as u32 };
    let pts = unsafe { (*src).pts };
    if width == 0 || height == 0 || width > MAX_VIDEO_DIMENSION || height > MAX_VIDEO_DIMENSION {
        return Ok(None);
    }
    let color_info =
        unsafe { FrameColorInfo::from_av_values((*src).colorspace, (*src).color_range, is_hdr) };

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_D3D11 {
        let cloned = unsafe { ffi::av_frame_clone(src) };
        if cloned.is_null() {
            bail!("av_frame_clone returned null");
        }
        let guard = Arc::new(AvFrameGuard { ptr: cloned });
        let raw_tex = unsafe { (*cloned).data[0].cast::<c_void>() };
        let slice_index = unsafe { (*cloned).data[1] as usize as u32 };
        let texture = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw_tex) }
            .context("AVFrame D3D11 texture pointer was null")?
            .clone();
        return Ok(Some(DecodedFrame::D3D11(D3D11Frame {
            texture,
            slice_index,
            width,
            height,
            pts,
            is_hdr,
            color_info,
            av_frame: guard,
        })));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_VULKAN {
        let cloned = unsafe { ffi::av_frame_clone(src) };
        if cloned.is_null() {
            bail!("av_frame_clone returned null");
        }
        let guard = Arc::new(AvFrameGuard { ptr: cloned });
        return Ok(Some(DecodedFrame::Vulkan(VulkanFrame {
            width,
            height,
            pts,
            is_hdr,
            color_info,
            av_frame: guard,
        })));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_D3D12 {
        let cloned = unsafe { ffi::av_frame_clone(src) };
        if cloned.is_null() {
            bail!("av_frame_clone returned null");
        }
        let guard = Arc::new(AvFrameGuard { ptr: cloned });
        let frame_ptr = unsafe { (*cloned).data[0].cast::<ffi::AVD3D12VAFrame>() };
        if frame_ptr.is_null() {
            bail!("AVFrame D3D12VA frame pointer was null");
        }
        let raw_tex = unsafe { (*frame_ptr).texture.cast::<c_void>() };
        let texture = unsafe { ID3D12Resource::from_raw_borrowed(&raw_tex) }
            .context("AVD3D12VAFrame texture pointer was null")?
            .clone();
        let raw_fence = unsafe { (*frame_ptr).sync_ctx.fence.cast::<c_void>() };
        let fence = if raw_fence.is_null() {
            None
        } else {
            unsafe { ID3D12Fence::from_raw_borrowed(&raw_fence) }.cloned()
        };
        let fence_value = unsafe { (*frame_ptr).sync_ctx.fence_value };
        let subresource_index =
            u32::try_from(unsafe { (*frame_ptr).subresource_index }.max(0)).unwrap_or(0);
        return Ok(Some(DecodedFrame::D3D12(D3D12Frame {
            texture,
            subresource_index,
            fence,
            fence_value,
            width,
            height,
            pts,
            is_hdr,
            color_info,
            _av_frame: guard,
        })));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_YUV420P {
        let (y_ptr, u_ptr, v_ptr, ys_raw, us_raw, vs_raw) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).data[2],
                (*src).linesize[0],
                (*src).linesize[1],
                (*src).linesize[2],
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let ys = validate_plane_layout(y_ptr, ys_raw, w, h)?;
        let us = validate_plane_layout(u_ptr, us_raw, cw, ch)?;
        let vs = validate_plane_layout(v_ptr, vs_raw, cw, ch)?;
        let y = copy_plane(y_ptr, ys, w, h);
        let u = copy_plane(u_ptr, us, cw, ch);
        let v = copy_plane(v_ptr, vs, cw, ch);
        return Ok(Some(DecodedFrame::SoftwareYuv420 {
            y,
            u,
            v,
            y_stride: w,
            u_stride: cw,
            v_stride: cw,
            width,
            height,
            pts,
            is_hdr,
            color_info,
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_NV12 {
        let (y_ptr, uv_ptr, ys_raw, uvs_raw) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).linesize[0],
                (*src).linesize[1],
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let ys = validate_plane_layout(y_ptr, ys_raw, w, h)?;
        let uvs = validate_plane_layout(uv_ptr, uvs_raw, cw * 2, ch)?;
        let y = copy_plane(y_ptr, ys, w, h);
        if preserve_semiplanar {
            let uv = copy_plane(uv_ptr, uvs, cw * 2, ch);
            return Ok(Some(DecodedFrame::SoftwareNv12 {
                y,
                uv,
                y_stride: w,
                uv_stride: cw * 2,
                width,
                height,
                pts,
                is_hdr,
                color_info,
            }));
        }
        let mut u = Vec::with_capacity(cw * ch);
        let mut v = Vec::with_capacity(cw * ch);
        for row in 0..ch {
            let src_row = unsafe { std::slice::from_raw_parts(uv_ptr.add(row * uvs), cw * 2) };
            for pair in src_row.as_chunks::<2>().0 {
                u.push(pair[0]);
                v.push(pair[1]);
            }
        }
        return Ok(Some(DecodedFrame::SoftwareYuv420 {
            y,
            u,
            v,
            y_stride: w,
            u_stride: cw,
            v_stride: cw,
            width,
            height,
            pts,
            is_hdr,
            color_info,
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_YUV420P10LE {
        let (y_ptr, u_ptr, v_ptr, ys_raw, us_raw, vs_raw) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).data[2],
                (*src).linesize[0],
                (*src).linesize[1],
                (*src).linesize[2],
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let ys = validate_plane_layout(y_ptr, ys_raw, w * 2, h)?;
        let us = validate_plane_layout(u_ptr, us_raw, cw * 2, ch)?;
        let vs = validate_plane_layout(v_ptr, vs_raw, cw * 2, ch)?;
        let y = copy_plane_10le_to_u8(y_ptr, ys, w, h, 2);
        let u = copy_plane_10le_to_u8(u_ptr, us, cw, ch, 2);
        let v = copy_plane_10le_to_u8(v_ptr, vs, cw, ch, 2);
        return Ok(Some(DecodedFrame::SoftwareYuv420 {
            y,
            u,
            v,
            y_stride: w,
            u_stride: cw,
            v_stride: cw,
            width,
            height,
            pts,
            is_hdr: true,
            color_info,
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_P010LE {
        let (y_ptr, uv_ptr, ys_raw, uvs_raw) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).linesize[0],
                (*src).linesize[1],
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let ys = validate_plane_layout(y_ptr, ys_raw, w * 2, h)?;
        let uvs = validate_plane_layout(uv_ptr, uvs_raw, cw * 4, ch)?;
        if preserve_semiplanar {
            let y = copy_plane(y_ptr, ys, w * 2, h);
            let uv = copy_plane(uv_ptr, uvs, cw * 4, ch);
            return Ok(Some(DecodedFrame::SoftwareP010 {
                y,
                uv,
                y_stride: w * 2,
                uv_stride: cw * 4,
                width,
                height,
                pts,
                is_hdr: true,
                color_info,
            }));
        }
        let y = copy_plane_10le_to_u8(y_ptr, ys, w, h, 8);
        let mut u = Vec::with_capacity(cw * ch);
        let mut v = Vec::with_capacity(cw * ch);
        for row in 0..ch {
            let src_row = unsafe { std::slice::from_raw_parts(uv_ptr.add(row * uvs), cw * 4) };
            for quad in src_row.as_chunks::<4>().0 {
                u.push((u16::from_le_bytes([quad[0], quad[1]]) >> 8) as u8);
                v.push((u16::from_le_bytes([quad[2], quad[3]]) >> 8) as u8);
            }
        }
        return Ok(Some(DecodedFrame::SoftwareYuv420 {
            y,
            u,
            v,
            y_stride: w,
            u_stride: cw,
            v_stride: cw,
            width,
            height,
            pts,
            is_hdr: true,
            color_info,
        }));
    }

    tracing::warn!("Unsupported decoded pixel format {fmt}");
    Ok(None)
}

pub(crate) fn validate_plane_layout(
    ptr: *const u8,
    linesize: i32,
    min_row_bytes: usize,
    rows: usize,
) -> Result<usize> {
    if ptr.is_null() {
        bail!("FFmpeg AVFrame plane pointer is null");
    }
    if linesize <= 0 {
        bail!("FFmpeg AVFrame linesize ({linesize}) must be positive");
    }
    let stride = linesize as usize;
    if stride < min_row_bytes {
        bail!("FFmpeg AVFrame linesize ({stride}) is smaller than row width ({min_row_bytes})");
    }
    if rows > 0
        && (rows - 1)
            .checked_mul(stride)
            .and_then(|o| o.checked_add(min_row_bytes))
            .is_none()
    {
        bail!("FFmpeg AVFrame plane byte offset overflow");
    }
    Ok(stride)
}

pub(crate) fn copy_plane(src: *const u8, stride: usize, width: usize, rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * rows);
    if stride == width {
        let all = unsafe { std::slice::from_raw_parts(src, width * rows) };
        out.extend_from_slice(all);
    } else {
        for r in 0..rows {
            let src_row = unsafe { std::slice::from_raw_parts(src.add(r * stride), width) };
            out.extend_from_slice(src_row);
        }
    }
    out
}

/// Converts a 10-bit little-endian luma/chroma plane to 8-bit by right-shifting `shift` bits
/// (`shift = 2` for LSB-aligned `YUV420P10LE`, `shift = 8` for MSB-aligned `P010LE`).
fn copy_plane_10le_to_u8(
    src: *const u8,
    stride_bytes: usize,
    width: usize,
    rows: usize,
    shift: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * rows);
    for r in 0..rows {
        let row_bytes = unsafe { std::slice::from_raw_parts(src.add(r * stride_bytes), width * 2) };
        for pair in row_bytes.as_chunks::<2>().0 {
            let sample = u16::from_le_bytes(*pair);
            out.push((sample >> shift).min(255) as u8);
        }
    }
    out
}

pub(crate) fn av_err_string(err: i32) -> String {
    let mut buf = [0i8; 128];
    unsafe {
        ffi::av_strerror(err, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Valid 64x64 H.264 Baseline Annex-B SPS + PPS + IDR NAL generated via FFmpeg libx264.
    const TEST_H264_IDR: &[u8] = &[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x0A, 0xD9, 0x04, 0x26, 0xC0, 0x44, 0x00, 0x00,
        0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xCA, 0x3C, 0x48, 0x99, 0x20, 0x00, 0x00, 0x00,
        0x01, 0x68, 0xCB, 0x83, 0xCB, 0x20, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x04, 0xBC, 0x46,
        0x28, 0x00, 0x0C, 0xC4, 0xC7, 0x00, 0x01, 0x7D, 0x68, 0xE0, 0x00, 0x22, 0x43, 0x27, 0x27,
        0x27, 0x5D, 0x75, 0xD7, 0x5D, 0x75, 0xD7, 0x5D, 0x75, 0xD7, 0x80,
    ];

    #[test]
    fn initializes_h264_software_decoder_and_decodes_idr_frame() {
        let mut decoder =
            VideoDecoder::new(MediaCodec::H264, None).expect("FFmpeg H264 decoder initializes");
        assert!(!decoder.is_hw_active());

        assert!(decoder.decode(&[]).expect("empty input ok").is_empty());

        let frames = decoder
            .decode(TEST_H264_IDR)
            .expect("valid H.264 IDR decodes");
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            DecodedFrame::SoftwareYuv420 { width, height, .. } => {
                assert_eq!((*width, *height), (64, 64));
            }
            DecodedFrame::D3D11(_)
            | DecodedFrame::D3D12(_)
            | DecodedFrame::Vulkan(_)
            | DecodedFrame::SoftwareNv12 { .. }
            | DecodedFrame::SoftwareP010 { .. } => panic!("expected software YUV420 frame"),
        }
    }

    #[test]
    fn flushes_and_decodes_subsequent_idr_frame() {
        let mut decoder =
            VideoDecoder::new(MediaCodec::H264, None).expect("FFmpeg H264 decoder initializes");
        let first = decoder.decode(TEST_H264_IDR).expect("first IDR decodes");
        assert_eq!(first.len(), 1);

        decoder.flush();

        let second = decoder
            .decode(TEST_H264_IDR)
            .expect("IDR decodes cleanly after flush");
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn preserves_packet_pts_across_decode() {
        let mut decoder =
            VideoDecoder::new(MediaCodec::H264, None).expect("FFmpeg H264 decoder initializes");
        let frames = decoder
            .decode_with_pts(TEST_H264_IDR, 42)
            .expect("H.264 IDR decodes with explicit PTS");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].pts(), 42);
        assert!(!frames[0].is_hdr());
    }

    #[test]
    fn initializes_hevc_hdr_decoder_and_downshifts_10bit_planes() {
        let mut decoder = VideoDecoder::new(MediaCodec::HevcHdr, None)
            .expect("FFmpeg HEVC HDR10 decoder initializes");
        assert!(!decoder.is_hw_active());
        assert!(decoder.decode(&[]).expect("empty input ok").is_empty());

        // YUV420P10LE (LSB 10-bit: 0..=1023) with 2-byte padding in stride_bytes.
        let mut p10_buf = Vec::new();
        for sample in [0_u16, 512, 1020, 0xFFFF] {
            p10_buf.extend_from_slice(&sample.to_le_bytes());
        }
        let converted_lsb = copy_plane_10le_to_u8(p10_buf.as_ptr(), 8, 3, 1, 2);
        assert_eq!(converted_lsb, vec![0, 128, 255]);

        // P010LE (MSB 10-bit in high bits of u16).
        let mut p010_buf = Vec::new();
        for sample in [0_u16, 128_u16 << 8, 255_u16 << 8] {
            p010_buf.extend_from_slice(&sample.to_le_bytes());
        }
        let converted_msb = copy_plane_10le_to_u8(p010_buf.as_ptr(), 6, 3, 1, 8);
        assert_eq!(converted_msb, vec![0, 128, 255]);
    }

    #[test]
    fn rejects_invalid_plane_layouts_and_oversized_packets() {
        let byte = [0u8; 64];
        assert!(validate_plane_layout(std::ptr::null(), 64, 16, 2).is_err());
        assert!(validate_plane_layout(byte.as_ptr(), 0, 16, 2).is_err());
        assert!(validate_plane_layout(byte.as_ptr(), -64, 16, 2).is_err());
        assert!(validate_plane_layout(byte.as_ptr(), 8, 16, 2).is_err());
        assert!(validate_plane_layout(byte.as_ptr(), i32::MAX, 16, usize::MAX).is_err());
        assert_eq!(validate_plane_layout(byte.as_ptr(), 16, 16, 2).unwrap(), 16);

        let mut decoder =
            VideoDecoder::new(MediaCodec::H264, None).expect("FFmpeg H264 decoder initializes");
        let oversized = vec![0u8; MAX_VIDEO_PACKET_BYTES + 1];
        assert!(decoder.decode(&oversized).is_err());
    }

    #[test]
    fn preserves_semiplanar_nv12_and_p010_frames() {
        let mut nv12_y = [16u8, 32, 48, 64, 80, 96, 112, 128];
        let mut nv12_uv = [120u8, 130, 140, 150];
        let mut av_frame = unsafe { ffi::av_frame_alloc() };
        assert!(!av_frame.is_null());
        unsafe {
            (*av_frame).format = ffi::AVPixelFormat_AV_PIX_FMT_NV12;
            (*av_frame).width = 4;
            (*av_frame).height = 2;
            (*av_frame).pts = 42;
            (*av_frame).data[0] = nv12_y.as_mut_ptr();
            (*av_frame).data[1] = nv12_uv.as_mut_ptr();
            (*av_frame).linesize[0] = 4;
            (*av_frame).linesize[1] = 4;
        }
        let frame = unsafe { extract_frame(av_frame, false, true) }
            .expect("NV12 extract ok")
            .expect("NV12 frame produced");
        match frame {
            DecodedFrame::SoftwareNv12 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                pts,
                is_hdr,
                color_info,
            } => {
                assert_eq!(
                    (width, height, y_stride, uv_stride, pts, is_hdr, color_info),
                    (4, 2, 4, 4, 42, false, FrameColorInfo::default())
                );
                assert_eq!(y, nv12_y);
                assert_eq!(uv, nv12_uv);
            }
            _ => panic!("expected SoftwareNv12"),
        }

        let mut p010_y: Vec<u8> = [64_u16 << 6, 256_u16 << 6, 512_u16 << 6, 1020_u16 << 6]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut p010_uv: Vec<u8> = [512_u16 << 6, 600_u16 << 6]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        unsafe {
            (*av_frame).format = ffi::AVPixelFormat_AV_PIX_FMT_P010LE;
            (*av_frame).width = 2;
            (*av_frame).height = 2;
            (*av_frame).pts = 99;
            (*av_frame).colorspace = ffi::AVColorSpace_AVCOL_SPC_BT2020_NCL;
            (*av_frame).color_range = ffi::AVColorRange_AVCOL_RANGE_MPEG;
            (*av_frame).data[0] = p010_y.as_mut_ptr();
            (*av_frame).data[1] = p010_uv.as_mut_ptr();
            (*av_frame).linesize[0] = 4;
            (*av_frame).linesize[1] = 4;
        }
        let p010_frame = unsafe { extract_frame(av_frame, true, true) }
            .expect("P010 extract ok")
            .expect("P010 frame produced");
        assert_eq!(p010_frame.dimensions(), (2, 2));
        assert_eq!(p010_frame.bit_depth(), 10);
        assert_eq!(
            p010_frame.color_info(),
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt2020,
                full_range: false,
            }
        );
        match p010_frame {
            DecodedFrame::SoftwareP010 {
                y,
                uv,
                y_stride,
                uv_stride,
                width,
                height,
                pts,
                is_hdr,
                color_info,
            } => {
                assert_eq!(
                    (
                        width,
                        height,
                        y_stride,
                        uv_stride,
                        pts,
                        is_hdr,
                        color_info.matrix
                    ),
                    (2, 2, 4, 4, 99, true, YuvColorMatrix::Bt2020)
                );
                assert_eq!(y, p010_y);
                assert_eq!(uv, p010_uv);
            }
            _ => panic!("expected SoftwareP010"),
        }
        unsafe {
            ffi::av_frame_free(&mut av_frame);
        }
    }

    #[test]
    fn maps_av_color_space_and_range_with_hdr_fallback() {
        assert_eq!(
            FrameColorInfo::from_av_values(
                ffi::AVColorSpace_AVCOL_SPC_SMPTE170M,
                ffi::AVColorRange_AVCOL_RANGE_JPEG,
                false,
            ),
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt601,
                full_range: true,
            }
        );
        assert_eq!(
            FrameColorInfo::from_av_values(
                ffi::AVColorSpace_AVCOL_SPC_UNSPECIFIED,
                ffi::AVColorRange_AVCOL_RANGE_UNSPECIFIED,
                false,
            ),
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt709,
                full_range: false,
            }
        );
        assert_eq!(
            FrameColorInfo::from_av_values(
                ffi::AVColorSpace_AVCOL_SPC_UNSPECIFIED,
                ffi::AVColorRange_AVCOL_RANGE_UNSPECIFIED,
                true,
            ),
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt2020,
                full_range: false,
            }
        );
        assert_eq!(
            FrameColorInfo {
                matrix: YuvColorMatrix::Bt709,
                full_range: false,
            }
            .label(),
            "BT.709 Lim"
        );
    }

    #[test]
    fn initializes_d3d11_transfer_mode_and_decodes_idr_without_d3d11_frames() {
        let mut decoder =
            VideoDecoder::new_with_mode(MediaCodec::H264, DecoderHardware::D3D11Transfer)
                .expect("D3D11Transfer or software fallback initializes");
        let frames = decoder.decode(TEST_H264_IDR).expect("IDR decode ok");
        assert!(!frames.is_empty(), "expected decoded IDR frame");
        assert!(
            !matches!(frames[0], DecodedFrame::D3D11(_)),
            "transfer mode must never emit D3D11 zero-copy textures"
        );
    }

    #[test]
    fn initializes_d3d12va_zero_copy_and_decodes_idr_or_falls_back() {
        use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
        use windows::Win32::Graphics::Direct3D12::D3D12CreateDevice;

        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_err() {
            eprintln!("skipping D3D12VA test: no D3D12 device available");
            return;
        }
        let dev = device.expect("D3D12CreateDevice populated device");
        let mut decoder =
            VideoDecoder::new_with_mode(MediaCodec::H264, DecoderHardware::D3D12ZeroCopy(dev))
                .expect("D3D12ZeroCopy or fallback initializes");
        let frames = decoder.decode(TEST_H264_IDR).expect("IDR decode ok");
        assert!(!frames.is_empty(), "expected decoded IDR frame");
        match &frames[0] {
            DecodedFrame::D3D12(_) => assert!(decoder.is_hw_active()),
            DecodedFrame::SoftwareNv12 { .. } | DecodedFrame::SoftwareP010 { .. } => {
                assert!(decoder.is_hw_active());
            }
            DecodedFrame::SoftwareYuv420 { .. } => assert!(!decoder.is_hw_active()),
            DecodedFrame::D3D11(_) | DecodedFrame::Vulkan(_) => {
                panic!("unexpected D3D11/Vulkan frame from D3D12ZeroCopy mode")
            }
        }
    }

    #[test]
    fn d3d12_fallback_replays_split_sps_pps_and_idr_packets() {
        use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
        use windows::Win32::Graphics::Direct3D12::D3D12CreateDevice;

        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_err() {
            eprintln!("skipping split-header D3D12 fallback test: no D3D12 device available");
            return;
        }
        let dev = device.expect("D3D12CreateDevice populated device");
        let idr_offset = TEST_H264_IDR
            .windows(4)
            .position(|w| w == [0x00, 0x00, 0x01, 0x65])
            .expect("TEST_H264_IDR contains IDR NAL start code");
        let (header_pkt, idr_pkt) = TEST_H264_IDR.split_at(idr_offset);

        let mut decoder =
            VideoDecoder::new_with_mode(MediaCodec::H264, DecoderHardware::D3D12ZeroCopy(dev))
                .expect("D3D12ZeroCopy or fallback initializes");
        let pre = decoder
            .decode_with_pts(header_pkt, 1)
            .expect("header packet should succeed");
        assert!(pre.is_empty(), "header-only packet emits no frames");

        let frames = decoder
            .decode_with_pts(idr_pkt, 2)
            .expect("split IDR decode should replay header packet across fallback");
        assert!(
            !frames.is_empty(),
            "expected decoded IDR frame after replaying buffered SPS/PPS header"
        );
        assert_eq!(frames[0].pts(), 2);
    }

    const TEST_HEVC_IDR: &[u8] = &[
        // VPS (0x40), SPS (0x42), PPS (0x44)
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x1E, 0xBA, 0x02, 0x40, 0x00, 0x00,
        0x00, 0x01, 0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
        0x00, 0x00, 0x03, 0x00, 0x1E, 0xA0, 0x20, 0x81, 0x05, 0x96, 0xE9, 0x29, 0x30, 0xBC, 0x05,
        0xA0, 0x20, 0x00, 0x00, 0x03, 0x00, 0x20, 0x00, 0x00, 0x03, 0x03, 0x21, 0x00, 0x00, 0x00,
        0x01, 0x44, 0x01, 0xC0, 0x73, 0xC0, 0x89, // IDR_N_LP (0x28)
        0x00, 0x00, 0x01, 0x28, 0x01, 0xAC, 0x76, 0x0D, 0x90, 0x91, 0xD2, 0x38,
    ];

    #[test]
    fn d3d12_fallback_replays_split_hevc_vps_sps_pps_and_idr() {
        use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
        use windows::Win32::Graphics::Direct3D12::D3D12CreateDevice;

        let mut device: Option<ID3D12Device> = None;
        if unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device) }.is_err() {
            eprintln!("skipping split-HEVC D3D12 fallback test: no D3D12 device available");
            return;
        }
        let dev = device.expect("D3D12CreateDevice populated device");
        let idr_offset = TEST_HEVC_IDR
            .windows(4)
            .position(|w| w == [0x00, 0x00, 0x01, 0x28])
            .expect("TEST_HEVC_IDR contains IDR_N_LP NAL start code");
        let (vps_sps_pps, idr_slice) = TEST_HEVC_IDR.split_at(idr_offset);

        let mut decoder =
            VideoDecoder::new_with_mode(MediaCodec::Hevc, DecoderHardware::D3D12ZeroCopy(dev))
                .expect("D3D12ZeroCopy or fallback initializes");
        let pre = decoder
            .decode_with_pts(vps_sps_pps, 1)
            .expect("HEVC VPS/SPS/PPS header packet should succeed");
        assert!(pre.is_empty(), "header-only packet emits no frames");

        let frames = decoder
            .decode_with_pts(idr_slice, 2)
            .expect("split HEVC IDR decode should combine and replay header across fallback");
        assert!(
            !frames.is_empty(),
            "expected decoded HEVC IDR frame after replaying buffered VPS/SPS/PPS header"
        );
        assert_eq!(frames[0].pts(), 2);
        match &frames[0] {
            DecodedFrame::D3D12(_) => assert!(decoder.is_hw_active()),
            DecodedFrame::SoftwareNv12 { .. } | DecodedFrame::SoftwareP010 { .. } => {
                assert!(decoder.is_hw_active());
            }
            DecodedFrame::SoftwareYuv420 { .. } => assert!(!decoder.is_hw_active()),
            DecodedFrame::D3D11(_) | DecodedFrame::Vulkan(_) => {
                panic!("unexpected D3D11/Vulkan frame from D3D12ZeroCopy mode")
            }
        }
    }
}
