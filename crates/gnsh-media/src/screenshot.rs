use crate::ffi;
use crate::video_decoder::{
    AvFrameGuard, DecodedFrame, av_err_string, copy_plane, validate_plane_layout,
};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// CPU-owned planar YUV pixels extracted from an active stream frame for background PNG encoding.
pub struct ScreenshotPixels {
    pub width: u32,
    pub height: u32,
    pub is_hdr: bool,
    pub format: ScreenshotPixelFormat,
}

pub enum ScreenshotPixelFormat {
    Nv12 {
        y: Vec<u8>,
        uv: Vec<u8>,
        y_stride: usize,
        uv_stride: usize,
    },
    P010 {
        y: Vec<u8>,
        uv: Vec<u8>,
        y_stride: usize,
        uv_stride: usize,
    },
    Yuv420 {
        y: Vec<u8>,
        u: Vec<u8>,
        v: Vec<u8>,
        y_stride: usize,
        u_stride: usize,
        v_stride: usize,
    },
}

/// Extracts CPU-accessible YUV planes from any `DecodedFrame` (`D3D11`/`D3D12`/`Vulkan` via `av_hwframe_transfer_data`, or software planes directly).
pub fn extract_screenshot_pixels(frame: &DecodedFrame) -> Result<ScreenshotPixels> {
    match frame {
        DecodedFrame::D3D11(f) => unsafe { hw_avframe_to_pixels(f.av_frame.as_ptr(), f.is_hdr) },
        DecodedFrame::D3D12(f) => unsafe { hw_avframe_to_pixels(f._av_frame.as_ptr(), f.is_hdr) },
        DecodedFrame::Vulkan(f) => unsafe { hw_avframe_to_pixels(f.av_frame.as_ptr(), f.is_hdr) },
        DecodedFrame::SoftwareNv12 {
            y,
            uv,
            y_stride,
            uv_stride,
            width,
            height,
            is_hdr,
            ..
        } => Ok(ScreenshotPixels {
            width: *width,
            height: *height,
            is_hdr: *is_hdr,
            format: ScreenshotPixelFormat::Nv12 {
                y: y.clone(),
                uv: uv.clone(),
                y_stride: *y_stride,
                uv_stride: *uv_stride,
            },
        }),
        DecodedFrame::SoftwareP010 {
            y,
            uv,
            y_stride,
            uv_stride,
            width,
            height,
            is_hdr,
            ..
        } => Ok(ScreenshotPixels {
            width: *width,
            height: *height,
            is_hdr: *is_hdr,
            format: ScreenshotPixelFormat::P010 {
                y: y.clone(),
                uv: uv.clone(),
                y_stride: *y_stride,
                uv_stride: *uv_stride,
            },
        }),
        DecodedFrame::SoftwareYuv420 {
            y,
            u,
            v,
            y_stride,
            u_stride,
            v_stride,
            width,
            height,
            is_hdr,
            ..
        } => Ok(ScreenshotPixels {
            width: *width,
            height: *height,
            is_hdr: *is_hdr,
            format: ScreenshotPixelFormat::Yuv420 {
                y: y.clone(),
                u: u.clone(),
                v: v.clone(),
                y_stride: *y_stride,
                u_stride: *u_stride,
                v_stride: *v_stride,
            },
        }),
    }
}

unsafe fn hw_avframe_to_pixels(
    hw_frame: *mut ffi::AVFrame,
    is_hdr: bool,
) -> Result<ScreenshotPixels> {
    if hw_frame.is_null() {
        bail!("hardware AVFrame pointer was null");
    }
    let sw_ptr = unsafe { ffi::av_frame_alloc() };
    if sw_ptr.is_null() {
        bail!("av_frame_alloc(screenshot) returned null");
    }
    let _sw_guard = AvFrameGuard { ptr: sw_ptr };

    let xfer_err = unsafe { ffi::av_hwframe_transfer_data(sw_ptr, hw_frame, 0) };
    if xfer_err < 0 {
        bail!(
            "av_hwframe_transfer_data(screenshot) failed: {}",
            av_err_string(xfer_err)
        );
    }

    let fmt = unsafe { (*sw_ptr).format };
    let width = u32::try_from(unsafe { (*sw_ptr).width }).context("invalid screenshot width")?;
    let height = u32::try_from(unsafe { (*sw_ptr).height }).context("invalid screenshot height")?;
    if width == 0 || height == 0 {
        bail!("zero-sized screenshot frame");
    }
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_NV12 {
        let (yp, uvp, ys, uvs) = unsafe {
            (
                (*sw_ptr).data[0],
                (*sw_ptr).data[1],
                validate_plane_layout((*sw_ptr).data[0], (*sw_ptr).linesize[0], w, h)?,
                validate_plane_layout((*sw_ptr).data[1], (*sw_ptr).linesize[1], cw * 2, ch)?,
            )
        };
        return Ok(ScreenshotPixels {
            width,
            height,
            is_hdr,
            format: ScreenshotPixelFormat::Nv12 {
                y: copy_plane(yp, ys, w, h),
                uv: copy_plane(uvp, uvs, cw * 2, ch),
                y_stride: w,
                uv_stride: cw * 2,
            },
        });
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_P010LE {
        let (yp, uvp, ys, uvs) = unsafe {
            (
                (*sw_ptr).data[0],
                (*sw_ptr).data[1],
                validate_plane_layout((*sw_ptr).data[0], (*sw_ptr).linesize[0], w * 2, h)?,
                validate_plane_layout((*sw_ptr).data[1], (*sw_ptr).linesize[1], cw * 4, ch)?,
            )
        };
        return Ok(ScreenshotPixels {
            width,
            height,
            is_hdr: true,
            format: ScreenshotPixelFormat::P010 {
                y: copy_plane(yp, ys, w * 2, h),
                uv: copy_plane(uvp, uvs, cw * 4, ch),
                y_stride: w * 2,
                uv_stride: cw * 4,
            },
        });
    }

    if fmt == ffi::AVPixelFormat_AV_PIX_FMT_YUV420P {
        let (yp, up, vp, ys, us, vs) = unsafe {
            (
                (*sw_ptr).data[0],
                (*sw_ptr).data[1],
                (*sw_ptr).data[2],
                validate_plane_layout((*sw_ptr).data[0], (*sw_ptr).linesize[0], w, h)?,
                validate_plane_layout((*sw_ptr).data[1], (*sw_ptr).linesize[1], cw, ch)?,
                validate_plane_layout((*sw_ptr).data[2], (*sw_ptr).linesize[2], cw, ch)?,
            )
        };
        return Ok(ScreenshotPixels {
            width,
            height,
            is_hdr,
            format: ScreenshotPixelFormat::Yuv420 {
                y: copy_plane(yp, ys, w, h),
                u: copy_plane(up, us, cw, ch),
                v: copy_plane(vp, vs, cw, ch),
                y_stride: w,
                u_stride: cw,
                v_stride: cw,
            },
        });
    }

    bail!("unsupported downloaded screenshot pixel format {fmt}");
}

fn pq_to_linear(v: f32) -> f32 {
    let p = v.max(0.0).powf(1.0 / 78.84375);
    let num = (p - 0.8359375).max(0.0);
    let den = (18.851_563 - 18.6875 * p).max(1e-6);
    (num / den).powf(1.0 / 0.15930176)
}

fn linear_to_srgb(c: f32) -> f32 {
    let clamped = c.clamp(0.0, 1.0);
    if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    }
}

fn pq_bt2020_to_sdr_srgb_rgb24(
    y_bytes: &[u8],
    uv_bytes: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    if y_bytes.len() < (h - 1) * y_stride + w * 2 || uv_bytes.len() < (ch - 1) * uv_stride + cw * 4
    {
        bail!("P010 buffer smaller than expected dimensions");
    }

    let mut out = vec![0u8; w * h * 3];
    for row in 0..h {
        let y_row = &y_bytes[row * y_stride..row * y_stride + w * 2];
        let uv_row_idx = (row / 2).min(ch - 1);
        let uv_row = &uv_bytes[uv_row_idx * uv_stride..uv_row_idx * uv_stride + cw * 4];
        for col in 0..w {
            let y_u10 = u16::from_le_bytes([y_row[col * 2], y_row[col * 2 + 1]]) >> 6;
            let uv_col = (col / 2).min(cw - 1);
            let cb_u10 = u16::from_le_bytes([uv_row[uv_col * 4], uv_row[uv_col * 4 + 1]]) >> 6;
            let cr_u10 = u16::from_le_bytes([uv_row[uv_col * 4 + 2], uv_row[uv_col * 4 + 3]]) >> 6;

            let y = ((f32::from(y_u10) - 64.0) / 876.0).clamp(0.0, 1.0);
            let cb = (f32::from(cb_u10) - 512.0) / 896.0;
            let cr = (f32::from(cr_u10) - 512.0) / 896.0;

            let r_pq = (y + 1.4746 * cr).clamp(0.0, 1.0);
            let g_pq = (y - 0.16455313 * cb - 0.57135313 * cr).clamp(0.0, 1.0);
            let b_pq = (y + 1.8814 * cb).clamp(0.0, 1.0);

            let r20 = pq_to_linear(r_pq) * 40.0;
            let g20 = pq_to_linear(g_pq) * 40.0;
            let b20 = pq_to_linear(b_pq) * 40.0;

            let r709 = (1.660491 * r20 - 0.587641 * g20 - 0.072850 * b20).max(0.0);
            let g709 = (-0.124550 * r20 + 1.132_9 * g20 - 0.008350 * b20).max(0.0);
            let b709 = (-0.018151 * r20 - 0.100579 * g20 + 1.118_73 * b20).max(0.0);

            let aces = |x: f32| -> u8 {
                let tm = (x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14);
                (linear_to_srgb(tm) * 255.0).round().clamp(0.0, 255.0) as u8
            };

            let idx = (row * w + col) * 3;
            out[idx] = aces(r709);
            out[idx + 1] = aces(g709);
            out[idx + 2] = aces(b709);
        }
    }
    Ok(out)
}

fn sws_yuv_to_rgb24(pixels: &ScreenshotPixels) -> Result<Vec<u8>> {
    let w = i32::try_from(pixels.width).context("screenshot width exceeds i32")?;
    let h = i32::try_from(pixels.height).context("screenshot height exceeds i32")?;

    let (src_fmt, src_ptrs, src_strides): (ffi::AVPixelFormat, [*const u8; 4], [i32; 4]) =
        match &pixels.format {
            ScreenshotPixelFormat::Nv12 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => (
                ffi::AVPixelFormat_AV_PIX_FMT_NV12,
                [y.as_ptr(), uv.as_ptr(), std::ptr::null(), std::ptr::null()],
                [*y_stride as i32, *uv_stride as i32, 0, 0],
            ),
            ScreenshotPixelFormat::P010 {
                y,
                uv,
                y_stride,
                uv_stride,
            } => (
                ffi::AVPixelFormat_AV_PIX_FMT_P010LE,
                [y.as_ptr(), uv.as_ptr(), std::ptr::null(), std::ptr::null()],
                [*y_stride as i32, *uv_stride as i32, 0, 0],
            ),
            ScreenshotPixelFormat::Yuv420 {
                y,
                u,
                v,
                y_stride,
                u_stride,
                v_stride,
            } => (
                ffi::AVPixelFormat_AV_PIX_FMT_YUV420P,
                [y.as_ptr(), u.as_ptr(), v.as_ptr(), std::ptr::null()],
                [*y_stride as i32, *u_stride as i32, *v_stride as i32, 0],
            ),
        };

    let sws = unsafe {
        ffi::sws_getContext(
            w,
            h,
            src_fmt,
            w,
            h,
            ffi::AVPixelFormat_AV_PIX_FMT_RGB24,
            ffi::SwsFlags_SWS_BICUBIC as i32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if sws.is_null() {
        bail!("sws_getContext(screenshot) returned null");
    }

    unsafe {
        let coeffs = ffi::sws_getCoefficients(ffi::SWS_CS_ITU709 as i32);
        ffi::sws_setColorspaceDetails(sws, coeffs, 0, coeffs, 1, 0, 1 << 16, 1 << 16);
    }

    let mut rgb = vec![0u8; (pixels.width as usize) * (pixels.height as usize) * 3];
    let dst_ptrs = [
        rgb.as_mut_ptr(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    ];
    let dst_strides = [(pixels.width as i32) * 3, 0, 0, 0];

    let scaled = unsafe {
        ffi::sws_scale(
            sws,
            src_ptrs.as_ptr(),
            src_strides.as_ptr(),
            0,
            h,
            dst_ptrs.as_ptr(),
            dst_strides.as_ptr(),
        )
    };
    unsafe {
        ffi::sws_freeContext(sws);
    }
    if scaled <= 0 {
        bail!("sws_scale(screenshot) returned {scaled}");
    }
    Ok(rgb)
}

/// Converts `pixels` (with HDR10-to-SDR ACES tone-mapping when `is_hdr`) and writes a lossless `.png` atomically to `path`.
pub fn save_screenshot_png(pixels: &ScreenshotPixels, path: &Path) -> Result<PathBuf> {
    if pixels.width == 0 || pixels.height == 0 {
        bail!("cannot save empty screenshot");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed creating {}", parent.display()))?;
    }

    let rgb = match &pixels.format {
        ScreenshotPixelFormat::P010 {
            y,
            uv,
            y_stride,
            uv_stride,
        } if pixels.is_hdr => {
            pq_bt2020_to_sdr_srgb_rgb24(y, uv, *y_stride, *uv_stride, pixels.width, pixels.height)?
        }
        _ => sws_yuv_to_rgb24(pixels)?,
    };

    let enc = unsafe { ffi::avcodec_find_encoder(ffi::AVCodecID_AV_CODEC_ID_PNG) };
    if enc.is_null() {
        bail!("FFmpeg PNG encoder (AV_CODEC_ID_PNG) is unavailable");
    }
    let mut ctx = unsafe { ffi::avcodec_alloc_context3(enc) };
    if ctx.is_null() {
        bail!("avcodec_alloc_context3(PNG) returned null");
    }

    let w = i32::try_from(pixels.width).context("screenshot width exceeds i32")?;
    let h = i32::try_from(pixels.height).context("screenshot height exceeds i32")?;
    unsafe {
        (*ctx).width = w;
        (*ctx).height = h;
        (*ctx).pix_fmt = ffi::AVPixelFormat_AV_PIX_FMT_RGB24;
        (*ctx).time_base = ffi::AVRational { num: 1, den: 1 };
    }
    let open_err = unsafe { ffi::avcodec_open2(ctx, enc, std::ptr::null_mut()) };
    if open_err < 0 {
        unsafe {
            ffi::avcodec_free_context(&mut ctx);
        }
        bail!("avcodec_open2(PNG) failed: {}", av_err_string(open_err));
    }

    let mut frame = unsafe { ffi::av_frame_alloc() };
    let mut pkt = unsafe { ffi::av_packet_alloc() };
    if frame.is_null() || pkt.is_null() {
        unsafe {
            if !frame.is_null() {
                ffi::av_frame_free(&mut frame);
            }
            if !pkt.is_null() {
                ffi::av_packet_free(&mut pkt);
            }
            ffi::avcodec_free_context(&mut ctx);
        }
        bail!("failed allocating PNG AVFrame/AVPacket");
    }

    let result = (|| -> Result<Vec<u8>> {
        unsafe {
            (*frame).format = ffi::AVPixelFormat_AV_PIX_FMT_RGB24;
            (*frame).width = w;
            (*frame).height = h;
            (*frame).pts = 0;
            let buf_err = ffi::av_frame_get_buffer(frame, 0);
            if buf_err < 0 {
                bail!(
                    "av_frame_get_buffer(PNG) failed: {}",
                    av_err_string(buf_err)
                );
            }
            let row_bytes = (pixels.width as usize) * 3;
            let dst_stride = (*frame).linesize[0] as usize;
            for r in 0..(pixels.height as usize) {
                std::ptr::copy_nonoverlapping(
                    rgb.as_ptr().add(r * row_bytes),
                    (*frame).data[0].add(r * dst_stride),
                    row_bytes,
                );
            }
            let send_err = ffi::avcodec_send_frame(ctx, frame);
            if send_err < 0 {
                bail!(
                    "avcodec_send_frame(PNG) failed: {}",
                    av_err_string(send_err)
                );
            }
            let _ = ffi::avcodec_send_frame(ctx, std::ptr::null());
            let recv_err = ffi::avcodec_receive_packet(ctx, pkt);
            if recv_err < 0 {
                bail!(
                    "avcodec_receive_packet(PNG) failed: {}",
                    av_err_string(recv_err)
                );
            }
            let encoded = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize).to_vec();
            ffi::av_packet_unref(pkt);
            Ok(encoded)
        }
    })();

    unsafe {
        ffi::av_packet_free(&mut pkt);
        ffi::av_frame_free(&mut frame);
        ffi::avcodec_free_context(&mut ctx);
    }

    let png_bytes = result?;
    let tmp_path = path.with_extension("png.tmp");
    std::fs::write(&tmp_path, &png_bytes)
        .with_context(|| format!("failed writing {}", tmp_path.display()))?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err).with_context(|| format!("failed finalizing {}", path.display()));
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_png_rgb24(path: &Path) -> (u32, u32, Vec<u8>) {
        let bytes = std::fs::read(path).expect("read png bytes");
        assert!(
            bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "valid PNG signature"
        );

        let dec = unsafe { ffi::avcodec_find_decoder(ffi::AVCodecID_AV_CODEC_ID_PNG) };
        assert!(!dec.is_null(), "PNG decoder available");
        let mut ctx = unsafe { ffi::avcodec_alloc_context3(dec) };
        assert!(!ctx.is_null());
        assert!(unsafe { ffi::avcodec_open2(ctx, dec, std::ptr::null_mut()) } >= 0);

        let mut pkt = unsafe { ffi::av_packet_alloc() };
        let mut frame = unsafe { ffi::av_frame_alloc() };
        unsafe {
            (*pkt).data = bytes.as_ptr().cast_mut();
            (*pkt).size = bytes.len() as i32;
            assert!(ffi::avcodec_send_packet(ctx, pkt) >= 0);
            assert!(ffi::avcodec_receive_frame(ctx, frame) >= 0);
        }
        let w = unsafe { (*frame).width as u32 };
        let h = unsafe { (*frame).height as u32 };
        let stride = unsafe { (*frame).linesize[0] as usize };
        let row_bytes = (w as usize) * 3;
        let mut rgb = Vec::with_capacity(row_bytes * h as usize);
        for r in 0..(h as usize) {
            let row =
                unsafe { std::slice::from_raw_parts((*frame).data[0].add(r * stride), row_bytes) };
            rgb.extend_from_slice(row);
        }
        unsafe {
            ffi::av_frame_free(&mut frame);
            ffi::av_packet_free(&mut pkt);
            ffi::avcodec_free_context(&mut ctx);
        }
        (w, h, rgb)
    }

    #[test]
    fn saves_valid_png_from_sdr_nv12_and_hdr10_p010() {
        let dir = std::env::temp_dir().join(format!("gnsh-shot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // SDR NV12 green image (64x64)
        let nv12 = ScreenshotPixels {
            width: 64,
            height: 64,
            is_hdr: false,
            format: ScreenshotPixelFormat::Nv12 {
                y: vec![175u8; 64 * 64],
                uv: vec![42u8; 64 * 32],
                y_stride: 64,
                uv_stride: 64,
            },
        };
        let sdr_path = dir.join("sdr.png");
        save_screenshot_png(&nv12, &sdr_path).expect("save SDR PNG");
        let (w, h, sdr_rgb) = decode_png_rgb24(&sdr_path);
        assert_eq!((w, h), (64, 64));
        let center = (32 * 64 + 32) * 3;
        assert!(
            sdr_rgb[center + 1] > 150 && sdr_rgb[center + 1] > sdr_rgb[center],
            "expected green SDR PNG pixel, got {:?}",
            &sdr_rgb[center..center + 3]
        );

        // HDR10 P010 bright green image (64x64)
        let y_sample = (600_u16 << 6).to_le_bytes();
        let cb_sample = (200_u16 << 6).to_le_bytes();
        let cr_sample = (200_u16 << 6).to_le_bytes();
        let mut p010_y = Vec::with_capacity(64 * 64 * 2);
        for _ in 0..(64 * 64) {
            p010_y.extend_from_slice(&y_sample);
        }
        let mut p010_uv = Vec::with_capacity(64 * 32 * 2);
        for _ in 0..(32 * 32) {
            p010_uv.extend_from_slice(&cb_sample);
            p010_uv.extend_from_slice(&cr_sample);
        }
        let p010 = ScreenshotPixels {
            width: 64,
            height: 64,
            is_hdr: true,
            format: ScreenshotPixelFormat::P010 {
                y: p010_y,
                uv: p010_uv,
                y_stride: 128,
                uv_stride: 128,
            },
        };
        let hdr_path = dir.join("hdr.png");
        save_screenshot_png(&p010, &hdr_path).expect("save HDR PNG");
        let (hw, hh, hdr_rgb) = decode_png_rgb24(&hdr_path);
        assert_eq!((hw, hh), (64, 64));
        assert!(
            hdr_rgb[center + 1] > 150 && hdr_rgb[center + 1] > hdr_rgb[center],
            "expected green tone-mapped HDR PNG pixel, got {:?}",
            &hdr_rgb[center..center + 3]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
