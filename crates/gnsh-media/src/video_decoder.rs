use crate::ffi;
use anyhow::{Context, Result, bail};
use std::ffi::CStr;
use std::os::raw::c_void;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, ID3D11Device, ID3D11Multithread,
    ID3D11Texture2D,
};
use windows::core::Interface as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCodec {
    H264,
    Hevc,
    HevcHdr,
}

struct AvFrameGuard {
    ptr: *mut ffi::AVFrame,
}

unsafe impl Send for AvFrameGuard {}

impl Drop for AvFrameGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                ffi::av_frame_free(&mut self.ptr);
            }
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
    // Retains FFmpeg's AVBufferRef until the texture slice is finished rendering.
    _av_frame: AvFrameGuard,
}

pub enum DecodedFrame {
    D3D11(D3D11Frame),
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
    },
}

impl DecodedFrame {
    /// Returns the FFmpeg presentation timestamp carried through decoding.
    pub fn pts(&self) -> i64 {
        match self {
            Self::D3D11(frame) => frame.pts,
            Self::SoftwareYuv420 { pts, .. } => *pts,
        }
    }

    /// Returns whether this decoded frame carries an HDR10 signal.
    pub fn is_hdr(&self) -> bool {
        match self {
            Self::D3D11(frame) => frame.is_hdr,
            Self::SoftwareYuv420 { is_hdr, .. } => *is_hdr,
        }
    }
}

pub struct VideoDecoder {
    codec: MediaCodec,
    ctx: *mut ffi::AVCodecContext,
    hw_device_ctx: *mut ffi::AVBufferRef,
    pkt: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
    hw_active: bool,
    decoded_any: bool,
}

unsafe impl Send for VideoDecoder {}

enum DrainOutcome {
    Done,
    FallbackToSw,
}

impl VideoDecoder {
    pub fn new(codec: MediaCodec, d3d11_device: Option<ID3D11Device>) -> Result<Self> {
        if let Some(dev) = d3d11_device
            && let Ok(hw) = Self::open_internal(codec, Some(dev))
        {
            return Ok(hw);
        }
        Self::open_internal(codec, None)
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

        let mut frames = Vec::new();
        let mut send_err = unsafe {
            (*self.pkt).data = annex_b.as_ptr().cast_mut();
            (*self.pkt).size = annex_b.len() as i32;
            (*self.pkt).pts = pts;
            (*self.pkt).dts = pts;
            ffi::avcodec_send_packet(self.ctx, self.pkt)
        };

        if send_err == ffi::GNSH_AVERROR_EAGAIN {
            if matches!(self.drain_frames(&mut frames)?, DrainOutcome::FallbackToSw) {
                return self.decode_with_pts(annex_b, pts);
            }
            send_err = unsafe {
                (*self.pkt).data = annex_b.as_ptr().cast_mut();
                (*self.pkt).size = annex_b.len() as i32;
                (*self.pkt).pts = pts;
                (*self.pkt).dts = pts;
                ffi::avcodec_send_packet(self.ctx, self.pkt)
            };
        }

        if send_err < 0 && send_err != ffi::GNSH_AVERROR_EAGAIN {
            if self.hw_active && !self.decoded_any {
                tracing::warn!(
                    "D3D11VA failed on initial packet ({}); falling back to software decode",
                    av_err_string(send_err)
                );
                let sw = Self::open_internal(self.codec, None)?;
                *self = sw;
                return self.decode_with_pts(annex_b, pts);
            }
            bail!("avcodec_send_packet failed: {}", av_err_string(send_err));
        }

        if matches!(self.drain_frames(&mut frames)?, DrainOutcome::FallbackToSw) {
            return self.decode_with_pts(annex_b, pts);
        }

        Ok(frames)
    }

    fn drain_frames(&mut self, frames: &mut Vec<DecodedFrame>) -> Result<DrainOutcome> {
        loop {
            let r = unsafe { ffi::avcodec_receive_frame(self.ctx, self.frame) };
            if r == ffi::GNSH_AVERROR_EAGAIN || r == ffi::GNSH_AVERROR_EOF {
                return Ok(DrainOutcome::Done);
            }
            if r < 0 {
                if self.hw_active && !self.decoded_any {
                    tracing::warn!(
                        "D3D11VA receive_frame failed ({}); falling back to software decode",
                        av_err_string(r)
                    );
                    let sw = Self::open_internal(self.codec, None)?;
                    *self = sw;
                    return Ok(DrainOutcome::FallbackToSw);
                }
                bail!("avcodec_receive_frame failed: {}", av_err_string(r));
            }

            let is_hdr_codec = self.codec == MediaCodec::HevcHdr;
            if let Some(decoded) = unsafe { extract_frame(self.frame, is_hdr_codec)? } {
                self.decoded_any = true;
                frames.push(decoded);
            }
            unsafe {
                ffi::av_frame_unref(self.frame);
            }
        }
    }

    fn open_internal(codec: MediaCodec, d3d11_device: Option<ID3D11Device>) -> Result<Self> {
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
        if pkt.is_null() || frame.is_null() {
            unsafe {
                if !pkt.is_null() {
                    ffi::av_packet_free(&mut { pkt });
                }
                if !frame.is_null() {
                    ffi::av_frame_free(&mut { frame });
                }
                ffi::avcodec_free_context(&mut { ctx });
            }
            bail!("failed allocating AVPacket/AVFrame");
        }

        let mut hw_device_ctx: *mut ffi::AVBufferRef = std::ptr::null_mut();
        let mut hw_active = false;

        unsafe {
            (*ctx).flags |= ffi::AV_CODEC_FLAG_LOW_DELAY as i32;
            (*ctx).flags2 |= ffi::AV_CODEC_FLAG2_FAST as i32;
            // Disallow FF_THREAD_FRAME (1), which adds 1+ frame of decode buffering latency.
            (*ctx).thread_type = 2;
        }

        if let Some(dev) = d3d11_device {
            match unsafe { create_d3d11va_device_ctx(&dev) } {
                Ok(buf_ref) => {
                    unsafe {
                        (*ctx).hw_device_ctx = ffi::av_buffer_ref(buf_ref);
                        (*ctx).get_format = Some(select_d3d11_format);
                        (*ctx).thread_count = 1;
                    }
                    hw_device_ctx = buf_ref;
                    hw_active = true;
                }
                Err(e) => {
                    tracing::warn!("D3D11VA device setup failed, using software decoder: {e:#}");
                }
            }
        } else {
            unsafe {
                (*ctx).thread_count = 4;
            }
        }

        let open_err = unsafe { ffi::avcodec_open2(ctx, av_codec, std::ptr::null_mut()) };
        if open_err < 0 {
            unsafe {
                if !hw_device_ctx.is_null() {
                    ffi::av_buffer_unref(&mut hw_device_ctx);
                }
                ffi::av_packet_free(&mut { pkt });
                ffi::av_frame_free(&mut { frame });
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
            hw_active,
            decoded_any: false,
        })
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe {
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

unsafe extern "C" fn select_d3d11_format(
    _ctx: *mut ffi::AVCodecContext,
    pix_fmts: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut p = pix_fmts;
    while !p.is_null() && unsafe { *p } != ffi::AVPixelFormat_AV_PIX_FMT_NONE {
        if unsafe { *p } == ffi::AVPixelFormat_AV_PIX_FMT_D3D11 {
            return ffi::AVPixelFormat_AV_PIX_FMT_D3D11;
        }
        p = unsafe { p.add(1) };
    }
    if !pix_fmts.is_null() {
        unsafe { *pix_fmts }
    } else {
        ffi::AVPixelFormat_AV_PIX_FMT_NONE
    }
}

unsafe fn extract_frame(src: *mut ffi::AVFrame, is_hdr: bool) -> Result<Option<DecodedFrame>> {
    let fmt = unsafe { (*src).format };
    let width = unsafe { (*src).width.max(0) as u32 };
    let height = unsafe { (*src).height.max(0) as u32 };
    let pts = unsafe { (*src).pts };
    if width == 0 || height == 0 {
        return Ok(None);
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_D3D11 {
        let cloned = unsafe { ffi::av_frame_clone(src) };
        if cloned.is_null() {
            bail!("av_frame_clone returned null");
        }
        let guard = AvFrameGuard { ptr: cloned };
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
            _av_frame: guard,
        })));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_YUV420P {
        let (y_ptr, u_ptr, v_ptr, ys, us, vs) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).data[2],
                (*src).linesize[0] as usize,
                (*src).linesize[1] as usize,
                (*src).linesize[2] as usize,
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
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
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_NV12 {
        let (y_ptr, uv_ptr, ys, uvs) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).linesize[0] as usize,
                (*src).linesize[1] as usize,
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let y = copy_plane(y_ptr, ys, w, h);
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
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_YUV420P10LE {
        let (y_ptr, u_ptr, v_ptr, ys, us, vs) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).data[2],
                (*src).linesize[0] as usize,
                (*src).linesize[1] as usize,
                (*src).linesize[2] as usize,
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
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
        }));
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_P010LE {
        let (y_ptr, uv_ptr, ys, uvs) = unsafe {
            (
                (*src).data[0],
                (*src).data[1],
                (*src).linesize[0] as usize,
                (*src).linesize[1] as usize,
            )
        };
        let w = width as usize;
        let h = height as usize;
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
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
        }));
    }

    tracing::warn!("Unsupported decoded pixel format {fmt}");
    Ok(None)
}

fn copy_plane(src: *const u8, stride: usize, width: usize, rows: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * rows];
    for r in 0..rows {
        let src_row = unsafe { std::slice::from_raw_parts(src.add(r * stride), width) };
        out[r * width..(r + 1) * width].copy_from_slice(src_row);
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

fn av_err_string(err: i32) -> String {
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
            DecodedFrame::D3D11(_) => panic!("expected software YUV420 frame"),
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
}
