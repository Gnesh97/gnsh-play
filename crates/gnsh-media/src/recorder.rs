use crate::annex_b::{contains_keyframe_nal, extract_parameter_sets, is_header_only_annex_b};
use crate::ffi;
use crate::video_decoder::{MediaCodec, av_err_string};
use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::ffi::CString;
use std::path::{Path, PathBuf};

const MAX_REPLAY_BUFFER_BYTES: usize = 512 * 1024 * 1024;
pub const MIN_REPLAY_BUFFER_SECS: u16 = 15;
pub const MAX_REPLAY_BUFFER_SECS: u16 = 60;

/// Zero-reencode Matroska (`.mkv`) muxer copying H.264/HEVC Annex-B video and `pcm_s16le` stereo audio directly to disk.
pub struct MkvMuxer {
    final_path: PathBuf,
    tmp_path: PathBuf,
    fmt_ctx: *mut ffi::AVFormatContext,
    video_stream_idx: i32,
    audio_stream_idx: i32,
    video_tb: ffi::AVRational,
    audio_tb: ffi::AVRational,
    audio_sample_rate: u32,
    audio_channels: u32,
    last_video_pts_us: i64,
    audio_samples_written: i64,
    finished: bool,
}

unsafe impl Send for MkvMuxer {}

impl MkvMuxer {
    /// Opens an `.mkv` output file (written via a sibling `.mkv.tmp` file until `finish()`).
    pub fn create(
        path: &Path,
        codec: MediaCodec,
        width: u32,
        height: u32,
        extradata: &[u8],
        audio_sample_rate: u32,
        audio_channels: u32,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("MkvMuxer requires non-zero video dimensions");
        }
        if audio_sample_rate == 0 || audio_channels == 0 {
            bail!("MkvMuxer requires non-zero audio parameters");
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed creating {}", parent.display()))?;
        }

        let tmp_path = path.with_extension("mkv.tmp");
        let tmp_str = tmp_path
            .to_str()
            .context("recording path contains non-UTF-8 characters")?;
        let c_tmp = CString::new(tmp_str).context("recording path contains interior NUL")?;

        let mut fmt_ctx: *mut ffi::AVFormatContext = std::ptr::null_mut();
        let alloc_err = unsafe {
            ffi::avformat_alloc_output_context2(
                &mut fmt_ctx,
                std::ptr::null(),
                c"matroska".as_ptr(),
                c_tmp.as_ptr(),
            )
        };
        if alloc_err < 0 || fmt_ctx.is_null() {
            bail!(
                "avformat_alloc_output_context2(matroska) failed: {}",
                av_err_string(alloc_err)
            );
        }

        let init_res = unsafe {
            Self::configure_and_open(
                fmt_ctx,
                &c_tmp,
                codec,
                width,
                height,
                extradata,
                audio_sample_rate,
                audio_channels,
            )
        };
        let (video_stream_idx, audio_stream_idx, video_tb, audio_tb) = match init_res {
            Ok(info) => info,
            Err(err) => {
                unsafe {
                    if !(*fmt_ctx).pb.is_null() {
                        let _ = ffi::avio_closep(&raw mut (*fmt_ctx).pb);
                    }
                    ffi::avformat_free_context(fmt_ctx);
                }
                let _ = std::fs::remove_file(&tmp_path);
                return Err(err);
            }
        };

        Ok(Self {
            final_path: path.to_path_buf(),
            tmp_path,
            fmt_ctx,
            video_stream_idx,
            audio_stream_idx,
            video_tb,
            audio_tb,
            audio_sample_rate,
            audio_channels,
            last_video_pts_us: -1,
            audio_samples_written: 0,
            finished: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn configure_and_open(
        fmt_ctx: *mut ffi::AVFormatContext,
        c_tmp: &CString,
        codec: MediaCodec,
        width: u32,
        height: u32,
        extradata: &[u8],
        audio_sample_rate: u32,
        audio_channels: u32,
    ) -> Result<(i32, i32, ffi::AVRational, ffi::AVRational)> {
        let vst = unsafe { ffi::avformat_new_stream(fmt_ctx, std::ptr::null()) };
        if vst.is_null() {
            bail!("avformat_new_stream(video) returned null");
        }
        unsafe {
            (*vst).time_base = ffi::AVRational {
                num: 1,
                den: 1_000_000,
            };
            let vpar = (*vst).codecpar;
            (*vpar).codec_type = ffi::AVMediaType_AVMEDIA_TYPE_VIDEO;
            (*vpar).codec_id = match codec {
                MediaCodec::H264 => ffi::AVCodecID_AV_CODEC_ID_H264,
                MediaCodec::Hevc | MediaCodec::HevcHdr => ffi::AVCodecID_AV_CODEC_ID_HEVC,
            };
            (*vpar).width = i32::try_from(width).context("video width exceeds i32")?;
            (*vpar).height = i32::try_from(height).context("video height exceeds i32")?;
            (*vpar).format = if codec == MediaCodec::HevcHdr {
                ffi::AVPixelFormat_AV_PIX_FMT_P010LE
            } else {
                ffi::AVPixelFormat_AV_PIX_FMT_NV12
            };
            if codec == MediaCodec::HevcHdr {
                (*vpar).color_primaries = ffi::AVColorPrimaries_AVCOL_PRI_BT2020;
                (*vpar).color_trc = ffi::AVColorTransferCharacteristic_AVCOL_TRC_SMPTE2084;
                (*vpar).color_space = ffi::AVColorSpace_AVCOL_SPC_BT2020_NCL;
            } else {
                (*vpar).color_primaries = ffi::AVColorPrimaries_AVCOL_PRI_BT709;
                (*vpar).color_trc = ffi::AVColorTransferCharacteristic_AVCOL_TRC_BT709;
                (*vpar).color_space = ffi::AVColorSpace_AVCOL_SPC_BT709;
            }
            if !extradata.is_empty() {
                let extra_sz = extradata.len() + ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize;
                let extra_ptr = ffi::av_mallocz(extra_sz).cast::<u8>();
                if extra_ptr.is_null() {
                    bail!("av_mallocz(extradata) returned null");
                }
                std::ptr::copy_nonoverlapping(extradata.as_ptr(), extra_ptr, extradata.len());
                (*vpar).extradata = extra_ptr;
                (*vpar).extradata_size =
                    i32::try_from(extradata.len()).context("extradata exceeds i32")?;
            }
        }

        let ast = unsafe { ffi::avformat_new_stream(fmt_ctx, std::ptr::null()) };
        if ast.is_null() {
            bail!("avformat_new_stream(audio) returned null");
        }
        unsafe {
            (*ast).time_base = ffi::AVRational {
                num: 1,
                den: 1_000_000,
            };
            let apar = (*ast).codecpar;
            (*apar).codec_type = ffi::AVMediaType_AVMEDIA_TYPE_AUDIO;
            (*apar).codec_id = ffi::AVCodecID_AV_CODEC_ID_PCM_S16LE;
            (*apar).sample_rate =
                i32::try_from(audio_sample_rate).context("sample rate exceeds i32")?;
            (*apar).bits_per_coded_sample = 16;
            (*apar).format = ffi::AVSampleFormat_AV_SAMPLE_FMT_S16;
            ffi::av_channel_layout_default(
                &raw mut (*apar).ch_layout,
                i32::try_from(audio_channels).context("channels exceed i32")?,
            );
        }

        let io_err = unsafe {
            ffi::avio_open(
                &raw mut (*fmt_ctx).pb,
                c_tmp.as_ptr(),
                ffi::AVIO_FLAG_WRITE as i32,
            )
        };
        if io_err < 0 {
            bail!("avio_open(matroska) failed: {}", av_err_string(io_err));
        }

        let hdr_err = unsafe { ffi::avformat_write_header(fmt_ctx, std::ptr::null_mut()) };
        if hdr_err < 0 {
            bail!(
                "avformat_write_header(matroska) failed: {}",
                av_err_string(hdr_err)
            );
        }

        unsafe {
            Ok((
                (*vst).index,
                (*ast).index,
                (*vst).time_base,
                (*ast).time_base,
            ))
        }
    }

    /// Writes one compressed video access unit (`pts_us` relative to stream start).
    pub fn write_video_packet(
        &mut self,
        data: &[u8],
        pts_us: i64,
        is_keyframe: bool,
    ) -> Result<()> {
        if self.finished || self.fmt_ctx.is_null() || data.is_empty() {
            return Ok(());
        }
        let monotonic_us = pts_us.max(self.last_video_pts_us.saturating_add(1));
        self.last_video_pts_us = monotonic_us;

        let len = i32::try_from(data.len()).context("video packet exceeds i32")?;
        let mut pkt = unsafe { ffi::av_packet_alloc() };
        if pkt.is_null() {
            bail!("av_packet_alloc returned null");
        }
        let write_err = unsafe {
            if ffi::av_new_packet(pkt, len) < 0 {
                ffi::av_packet_free(&mut pkt);
                bail!("av_new_packet(video) failed");
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), (*pkt).data, data.len());
            let src_tb = ffi::AVRational {
                num: 1,
                den: 1_000_000,
            };
            let ts = ffi::av_rescale_q(monotonic_us, src_tb, self.video_tb);
            (*pkt).pts = ts;
            (*pkt).dts = ts;
            (*pkt).stream_index = self.video_stream_idx;
            (*pkt).flags = if is_keyframe {
                ffi::AV_PKT_FLAG_KEY as i32
            } else {
                0
            };
            let err = ffi::av_interleaved_write_frame(self.fmt_ctx, pkt);
            ffi::av_packet_free(&mut pkt);
            err
        };
        if write_err < 0 {
            bail!(
                "av_interleaved_write_frame(video) failed: {}",
                av_err_string(write_err)
            );
        }
        Ok(())
    }

    /// Writes signed 16-bit interleaved PCM audio samples (`sample_rate` / `channels` matching muxer config).
    pub fn write_audio_pcm(&mut self, samples: &[i16], pts_us: i64) -> Result<()> {
        if self.finished || self.fmt_ctx.is_null() || samples.is_empty() {
            return Ok(());
        }
        let ch = self.audio_channels.max(1) as i64;
        let frames = (samples.len() as i64) / ch;
        if frames <= 0 {
            return Ok(());
        }

        let computed_pts_us = if self.audio_samples_written == 0 {
            pts_us.max(0)
        } else {
            let calc = (self.audio_samples_written * 1_000_000) / i64::from(self.audio_sample_rate);
            calc.max(0)
        };
        self.audio_samples_written = self.audio_samples_written.saturating_add(frames);

        let byte_len = samples.len() * 2;
        let pkt_len = i32::try_from(byte_len).context("audio packet exceeds i32")?;
        let mut pkt = unsafe { ffi::av_packet_alloc() };
        if pkt.is_null() {
            bail!("av_packet_alloc(audio) returned null");
        }
        let write_err = unsafe {
            if ffi::av_new_packet(pkt, pkt_len) < 0 {
                ffi::av_packet_free(&mut pkt);
                bail!("av_new_packet(audio) failed");
            }
            let mut dst = std::slice::from_raw_parts_mut((*pkt).data, byte_len);
            for &s in samples {
                let le = s.to_le_bytes();
                dst[0] = le[0];
                dst[1] = le[1];
                dst = &mut dst[2..];
            }
            let src_tb = ffi::AVRational {
                num: 1,
                den: 1_000_000,
            };
            let ts = ffi::av_rescale_q(computed_pts_us, src_tb, self.audio_tb);
            (*pkt).pts = ts;
            (*pkt).dts = ts;
            (*pkt).stream_index = self.audio_stream_idx;
            let err = ffi::av_interleaved_write_frame(self.fmt_ctx, pkt);
            ffi::av_packet_free(&mut pkt);
            err
        };
        if write_err < 0 {
            bail!(
                "av_interleaved_write_frame(audio) failed: {}",
                av_err_string(write_err)
            );
        }
        Ok(())
    }

    fn close_context(&mut self, write_trailer: bool) -> Result<()> {
        if self.finished || self.fmt_ctx.is_null() {
            return Ok(());
        }
        self.finished = true;
        let trailer_err = if write_trailer {
            unsafe { ffi::av_write_trailer(self.fmt_ctx) }
        } else {
            0
        };
        unsafe {
            if !(*self.fmt_ctx).pb.is_null() {
                let _ = ffi::avio_closep(&raw mut (*self.fmt_ctx).pb);
            }
            ffi::avformat_free_context(self.fmt_ctx);
        }
        self.fmt_ctx = std::ptr::null_mut();
        if trailer_err < 0 {
            bail!(
                "av_write_trailer(matroska) failed: {}",
                av_err_string(trailer_err)
            );
        }
        Ok(())
    }

    /// Finalizes the MKV trailer and atomically renames the temporary file to its destination path.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.close_context(true)?;
        std::fs::rename(&self.tmp_path, &self.final_path).with_context(|| {
            format!(
                "failed finalizing recording {} -> {}",
                self.tmp_path.display(),
                self.final_path.display()
            )
        })?;
        Ok(self.final_path.clone())
    }
}

impl Drop for MkvMuxer {
    fn drop(&mut self) {
        if !self.finished && !self.fmt_ctx.is_null() {
            let _ = self.close_context(true);
            let _ = std::fs::rename(&self.tmp_path, &self.final_path);
        }
    }
}

#[derive(Clone)]
pub enum BufferedPacketKind {
    Video {
        data: Vec<u8>,
        is_keyframe: bool,
        param_sets: Option<Vec<u8>>,
    },
    Audio {
        samples: Vec<i16>,
        sample_rate: u32,
        channels: u32,
    },
}

#[derive(Clone)]
pub struct BufferedPacket {
    pub ts_us: i64,
    pub byte_size: usize,
    pub kind: BufferedPacketKind,
}

pub struct ReplayClipSnapshot {
    pub extradata: Vec<u8>,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
    pub packets: Vec<BufferedPacket>,
    pub duration_secs: f32,
}

/// Bounded compressed-video + PCM-audio ring buffer for instant clip saving.
pub struct ReplayRingBuffer {
    codec: MediaCodec,
    max_duration_us: i64,
    latest_param_sets: Vec<u8>,
    packets: VecDeque<BufferedPacket>,
    total_bytes: usize,
}

impl ReplayRingBuffer {
    pub fn new(codec: MediaCodec, duration_secs: u16) -> Self {
        let clamped = duration_secs.clamp(MIN_REPLAY_BUFFER_SECS, MAX_REPLAY_BUFFER_SECS);
        Self {
            codec,
            max_duration_us: i64::from(clamped) * 1_000_000,
            latest_param_sets: Vec::new(),
            packets: VecDeque::new(),
            total_bytes: 0,
        }
    }

    pub fn set_duration_secs(&mut self, duration_secs: u16) {
        let clamped = duration_secs.clamp(MIN_REPLAY_BUFFER_SECS, MAX_REPLAY_BUFFER_SECS);
        self.max_duration_us = i64::from(clamped) * 1_000_000;
        self.evict(None);
    }

    pub fn clear(&mut self) {
        self.packets.clear();
        self.total_bytes = 0;
    }

    pub fn push_video(&mut self, data: Vec<u8>, ts_us: i64) {
        if let Some(ps) = extract_parameter_sets(&data, self.codec) {
            self.latest_param_sets = ps;
        }
        if is_header_only_annex_b(&data, self.codec) {
            return;
        }
        let is_keyframe = contains_keyframe_nal(&data, self.codec);
        let param_sets = if is_keyframe && !self.latest_param_sets.is_empty() {
            Some(self.latest_param_sets.clone())
        } else {
            None
        };
        let byte_size = data.len() + param_sets.as_ref().map_or(0, Vec::len);
        self.total_bytes += byte_size;
        self.packets.push_back(BufferedPacket {
            ts_us,
            byte_size,
            kind: BufferedPacketKind::Video {
                data,
                is_keyframe,
                param_sets,
            },
        });
        self.evict(Some(ts_us));
    }

    pub fn push_audio(&mut self, samples: Vec<i16>, sample_rate: u32, channels: u32, ts_us: i64) {
        if samples.is_empty() {
            return;
        }
        let byte_size = samples.len() * 2;
        self.total_bytes += byte_size;
        self.packets.push_back(BufferedPacket {
            ts_us,
            byte_size,
            kind: BufferedPacketKind::Audio {
                samples,
                sample_rate,
                channels,
            },
        });
        self.evict(Some(ts_us));
    }

    fn evict(&mut self, latest_ts_us: Option<i64>) {
        let newest = latest_ts_us.or_else(|| self.packets.back().map(|p| p.ts_us));
        if let Some(newest_ts) = newest {
            let cutoff = newest_ts.saturating_sub(self.max_duration_us);
            while let Some(front) = self.packets.front() {
                if front.ts_us >= cutoff {
                    break;
                }
                // Keep the last IDR anchor before cutoff unless a newer IDR already supersedes it.
                if matches!(
                    front.kind,
                    BufferedPacketKind::Video {
                        is_keyframe: true,
                        ..
                    }
                ) && !self.has_keyframe_after(0, cutoff)
                {
                    break;
                }
                if let Some(popped) = self.packets.pop_front() {
                    self.total_bytes = self.total_bytes.saturating_sub(popped.byte_size);
                }
            }
        }
        while self.total_bytes > MAX_REPLAY_BUFFER_BYTES {
            if let Some(popped) = self.packets.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(popped.byte_size);
            } else {
                break;
            }
        }
    }

    fn has_keyframe_after(&self, skip_first: usize, max_ts_exclusive: i64) -> bool {
        self.packets.iter().skip(skip_first + 1).any(|p| {
            p.ts_us <= max_ts_exclusive
                && matches!(
                    p.kind,
                    BufferedPacketKind::Video {
                        is_keyframe: true,
                        ..
                    }
                )
        })
    }

    /// Produces a self-contained snapshot starting at the earliest buffered IDR keyframe.
    pub fn snapshot_for_clip(&self) -> Option<ReplayClipSnapshot> {
        let start_idx = self.packets.iter().position(|p| {
            matches!(
                p.kind,
                BufferedPacketKind::Video {
                    is_keyframe: true,
                    ..
                }
            )
        })?;
        let base_ts = self.packets[start_idx].ts_us;
        let extradata = match &self.packets[start_idx].kind {
            BufferedPacketKind::Video {
                param_sets: Some(ps),
                ..
            } => ps.clone(),
            _ => self.latest_param_sets.clone(),
        };

        let mut packets = Vec::with_capacity(self.packets.len().saturating_sub(start_idx));
        let mut audio_sample_rate = 48_000;
        let mut audio_channels = 2;
        let mut max_rel_ts = 0i64;

        for pkt in self.packets.iter().skip(start_idx) {
            let rel_ts = pkt.ts_us.saturating_sub(base_ts);
            max_rel_ts = max_rel_ts.max(rel_ts);
            if let BufferedPacketKind::Audio {
                sample_rate,
                channels,
                ..
            } = pkt.kind
            {
                audio_sample_rate = sample_rate;
                audio_channels = channels;
            }
            let mut cloned = pkt.clone();
            cloned.ts_us = rel_ts;
            packets.push(cloned);
        }

        Some(ReplayClipSnapshot {
            extradata,
            audio_sample_rate,
            audio_channels,
            packets,
            duration_secs: (max_rel_ts as f32) / 1_000_000.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_detects_param_sets_and_keyframes_for_h264_and_hevc() {
        let h264_hdr = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x00, 0x00, 0x01, 0x68, 0xCE,
        ];
        assert!(is_header_only_annex_b(&h264_hdr, MediaCodec::H264));
        assert!(!contains_keyframe_nal(&h264_hdr, MediaCodec::H264));
        let ps = extract_parameter_sets(&h264_hdr, MediaCodec::H264).expect("SPS+PPS");
        assert!(ps.starts_with(&[0, 0, 0, 1, 0x67]));

        let h264_idr = [0x00, 0x00, 0x01, 0x65, 0x88, 0x84];
        assert!(!is_header_only_annex_b(&h264_idr, MediaCodec::H264));
        assert!(contains_keyframe_nal(&h264_idr, MediaCodec::H264));

        let hevc_hdr = [
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x44, 0x01, 0xC0,
        ];
        assert!(is_header_only_annex_b(&hevc_hdr, MediaCodec::Hevc));
        assert!(!contains_keyframe_nal(&hevc_hdr, MediaCodec::Hevc));
        assert!(extract_parameter_sets(&hevc_hdr, MediaCodec::Hevc).is_some());

        let hevc_idr = [0x00, 0x00, 0x01, 0x28, 0x01, 0xAC];
        assert!(!is_header_only_annex_b(&hevc_idr, MediaCodec::Hevc));
        assert!(contains_keyframe_nal(&hevc_idr, MediaCodec::Hevc));
    }

    #[test]
    fn replay_ring_buffer_trims_by_duration_and_starts_clips_on_keyframe() {
        let mut rb = ReplayRingBuffer::new(MediaCodec::H264, 15);
        assert!(rb.snapshot_for_clip().is_none());

        rb.push_video(vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE], 0);
        // Orphan P-frame before first IDR must be skipped by snapshot_for_clip
        rb.push_video(vec![0, 0, 1, 0x41, 1, 2], 1_000_000);
        assert!(rb.snapshot_for_clip().is_none());

        // First IDR at 2s, P-frame at 10s, second IDR at 20s, P-frame at 25s
        rb.push_video(vec![0, 0, 1, 0x65, 0xAA], 2_000_000);
        rb.push_audio(vec![100i16; 960], 48_000, 2, 2_010_000);
        rb.push_video(vec![0, 0, 1, 0x41, 0xBB], 10_000_000);
        rb.push_video(vec![0, 0, 1, 0x65, 0xCC], 20_000_000);
        rb.push_video(vec![0, 0, 1, 0x41, 0xDD], 38_000_000);

        let snap = rb.snapshot_for_clip().expect("clip snapshot ready");
        assert!(!snap.extradata.is_empty());
        assert!(matches!(
            snap.packets[0].kind,
            BufferedPacketKind::Video {
                is_keyframe: true,
                ..
            }
        ));
        assert!(
            snap.duration_secs >= 17.0,
            "expected >= 17s buffered, got {}",
            snap.duration_secs
        );
    }

    #[test]
    fn mkv_muxer_writes_valid_playable_file_with_video_and_audio() {
        let dir = std::env::temp_dir().join(format!("gnsh-mkv-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mkv_path = dir.join("test_capture.mkv");

        // Valid 64x64 H.264 Baseline SPS+PPS + IDR slice
        let h264_sps_pps = [
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x0A, 0xD9, 0x04, 0x26, 0xC0, 0x44, 0x00,
            0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xCA, 0x3C, 0x48, 0x99, 0x20, 0x00,
            0x00, 0x00, 0x01, 0x68, 0xCB, 0x83, 0xCB, 0x20,
        ];
        let h264_idr = [
            0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x04, 0xBC, 0x46, 0x28, 0x00, 0x0C, 0xC4,
            0xC7, 0x00, 0x01, 0x7D, 0x68, 0xE0, 0x00, 0x22, 0x43, 0x27, 0x27, 0x27, 0x5D, 0x75,
            0xD7, 0x5D, 0x75, 0xD7, 0x5D, 0x75, 0xD7, 0x80,
        ];

        let mut muxer = MkvMuxer::create(
            &mkv_path,
            MediaCodec::H264,
            64,
            64,
            &h264_sps_pps,
            48_000,
            2,
        )
        .expect("create MkvMuxer");
        muxer
            .write_video_packet(&h264_idr, 0, true)
            .expect("write video IDR 1");
        muxer
            .write_audio_pcm(&[500i16; 960], 0)
            .expect("write audio 1");
        muxer
            .write_video_packet(&h264_idr, 16_667, true)
            .expect("write video IDR 2");
        muxer
            .write_audio_pcm(&[-500i16; 960], 10_000)
            .expect("write audio 2");
        let saved = muxer.finish().expect("finish mkv");
        assert!(saved.exists());

        let c_in = CString::new(saved.to_str().unwrap()).unwrap();
        let mut in_ctx: *mut ffi::AVFormatContext = std::ptr::null_mut();
        unsafe {
            assert!(
                ffi::avformat_open_input(
                    &mut in_ctx,
                    c_in.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                ) >= 0
            );
            assert!(ffi::avformat_find_stream_info(in_ctx, std::ptr::null_mut()) >= 0);
            assert_eq!((*in_ctx).nb_streams, 2);
            let s0 = *(*in_ctx).streams.add(0);
            let s1 = *(*in_ctx).streams.add(1);
            assert_eq!((*(*s0).codecpar).codec_id, ffi::AVCodecID_AV_CODEC_ID_H264);
            assert_eq!(
                (*(*s1).codecpar).codec_id,
                ffi::AVCodecID_AV_CODEC_ID_PCM_S16LE
            );
            ffi::avformat_close_input(&mut in_ctx);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
