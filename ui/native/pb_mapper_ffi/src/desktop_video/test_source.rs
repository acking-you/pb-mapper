//! The clip player behind `pb_mapper_desktop_video_test_source_v1`.
use bytes::Bytes;
use pb_mapper::desktop::{VideoCodec, VideoSink, VideoUnit};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const MAX_CLIP: u64 = 256 * 1024 * 1024;
/// The wire's largest payload.
const MAX_UNIT: usize = 4 * 1024 * 1024;

pub(super) struct Clip {
    codec: VideoCodec,
    units: Vec<Unit>,
}

struct Unit {
    bytes: Bytes,
    keyframe: bool,
    width: u32,
    height: u32,
}

pub(super) struct Player {
    /// Dropping it ends the loop at once.
    stop: mpsc::Sender<()>,
    paused: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}
impl Player {
    pub(super) fn start(sink: Arc<VideoSink>, clip: Clip, fps: u32) -> Option<Self> {
        let (stop, stopped) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let playing = paused.clone();
        let thread = std::thread::Builder::new()
            .name("pb-desktop-test-source".into())
            .spawn(move || play(&sink, &clip, fps, &playing, &stopped))
            .ok()?;
        Some(Self {
            stop,
            paused,
            thread,
        })
    }

    pub(super) fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }

    pub(super) fn stop(self) {
        drop(self.stop);
        let _ = self.thread.join();
    }
}

/// Plays like a host: nothing while the viewer is paused, then the clip
/// again from its first IDR.
fn play(
    sink: &VideoSink,
    clip: &Clip,
    fps: u32,
    paused: &AtomicBool,
    stopped: &mpsc::Receiver<()>,
) {
    let interval = Duration::from_secs(1) / fps;
    let started = Instant::now();
    let mut deadline = started;
    let mut frame_id = 0;
    let mut position = 0;
    loop {
        if paused.load(Ordering::Acquire) {
            position = 0;
        } else if let Some(unit) = clip.units.get(position) {
            frame_id += 1;
            sink.push_for_test(VideoUnit {
                seq: frame_id,
                generation: 1,
                frame_id,
                codec: clip.codec,
                width: unit.width,
                height: unit.height,
                keyframe: unit.keyframe,
                duplicate: false,
                // The sink marks a discontinuity, as in a session: after
                // a drop, a generation change or a resume.
                discontinuity: false,
                pts_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                bytes: unit.bytes.clone(),
                received_at: Instant::now(),
            });
            position = (position + 1) % clip.units.len();
        }
        deadline += interval;
        let now = Instant::now();
        // Behind schedule (a suspended device), carry on without a burst.
        deadline = deadline.max(now);
        if !matches!(
            stopped.recv_timeout(deadline - now),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            return;
        }
    }
}

pub(super) fn load(path: &Path) -> Option<Clip> {
    if std::fs::metadata(path).ok()?.len() > MAX_CLIP {
        return None;
    }
    let data = Bytes::from(std::fs::read(path).ok()?);
    let mut codec = None;
    let mut units = Vec::new();
    let mut size = None;
    let mut offset = 0;
    while offset < data.len() {
        let len = data.get(offset..offset + 4)?;
        let len = u32::from_be_bytes(len.try_into().ok()?) as usize;
        let start = offset + 4;
        let end = start.checked_add(len)?;
        if len == 0 || len > MAX_UNIT || end > data.len() {
            return None;
        }
        let bytes = data.slice(start..end);
        let nals = nal_units(&bytes);
        if nals.is_empty() {
            return None;
        }
        // An H.265 VPS header, 0x40 0x01, is a NAL type no H.264
        // encoder writes.
        let codec = *codec.get_or_insert(if nals.iter().any(|nal| nal.starts_with(&[0x40, 1])) {
            VideoCodec::H265
        } else {
            VideoCodec::H264
        });
        let mut keyframe = false;
        for nal in nals {
            match (codec, nal_type(codec, nal)) {
                (VideoCodec::H264, 5) | (VideoCodec::H265, 16..=21) => keyframe = true,
                (VideoCodec::H264, 7) | (VideoCodec::H265, 33) => {
                    size = Some(sps_size(codec, nal)?);
                }
                _ => {}
            }
        }
        if units.is_empty() && !keyframe {
            return None;
        }
        let (width, height) = size?;
        units.push(Unit {
            bytes,
            keyframe,
            width,
            height,
        });
        offset = end;
    }
    Some(Clip {
        codec: codec?,
        units,
    })
}

/// H.264's NAL type is the low five bits of its one-byte header,
/// H.265's the six above the forbidden bit of its two-byte one.
fn nal_type(codec: VideoCodec, nal: &[u8]) -> u8 {
    if codec == VideoCodec::H265 {
        (nal[0] >> 1) & 0x3f
    } else {
        nal[0] & 0x1f
    }
}

/// The NAL units of an Annex B access unit, without their start codes.
fn nal_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut position = 0;
    while position + 3 <= bytes.len() {
        if bytes[position..position + 3] == [0, 0, 1] {
            position += 3;
            starts.push(position);
        } else {
            position += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| {
            let end = starts.get(index + 1).map_or(bytes.len(), |&next| next - 3);
            &bytes[start..end.max(start)]
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

/// The picture size an SPS declares, after its cropping.
pub(super) fn sps_size(codec: VideoCodec, nal: &[u8]) -> Option<(u32, u32)> {
    if codec == VideoCodec::H265 {
        h265_sps_size(nal)
    } else {
        h264_sps_size(nal)
    }
}

/// H.265: the luma size after the conformance window, which crops the
/// coding-block padding (1088 rows to 1080) in chroma units.
fn h265_sps_size(nal: &[u8]) -> Option<(u32, u32)> {
    let rbsp = unescape(nal.get(2..)?);
    let mut bits = Bits {
        data: &rbsp,
        position: 0,
    };
    bits.read(4)?; // sps_video_parameter_set_id
    let sub_layers = bits.read(3)? as usize; // sps_max_sub_layers_minus1
    bits.bit()?; // sps_temporal_id_nesting_flag
    // profile_tier_level: the general profile (88 bits) and level (8),
    // then what each sub-layer adds.
    bits.skip(88 + 8)?;
    let mut present = [(false, false); 7];
    for layer in &mut present[..sub_layers] {
        *layer = (bits.bit()? == 1, bits.bit()? == 1);
    }
    if sub_layers > 0 {
        bits.skip(2 * (8 - sub_layers))?; // reserved_zero_2bits
    }
    for (profile, level) in &present[..sub_layers] {
        bits.skip(if *profile { 88 } else { 0 } + if *level { 8 } else { 0 })?;
    }
    bits.ue()?; // sps_seq_parameter_set_id
    let chroma = bits.ue()?;
    if chroma == 3 {
        bits.bit()?; // separate_colour_plane_flag
    }
    let width = bits.ue()?;
    let height = bits.ue()?;
    let mut window = [0; 4];
    if bits.bit()? == 1 {
        for edge in &mut window {
            *edge = bits.ue()?;
        }
    }
    let [left, right, top, bottom] = window;
    let (unit_x, unit_y) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let width = width.checked_sub(left.checked_add(right)?.checked_mul(unit_x)?)?;
    let height = height.checked_sub(top.checked_add(bottom)?.checked_mul(unit_y)?)?;
    (width > 0 && height > 0).then_some((width, height))
}

/// H.264: the macroblock size after the frame cropping.
fn h264_sps_size(nal: &[u8]) -> Option<(u32, u32)> {
    let rbsp = unescape(nal.get(1..)?);
    let mut bits = Bits {
        data: &rbsp,
        position: 0,
    };
    let profile = bits.read(8)?;
    bits.read(16)?; // constraint flags and level
    bits.ue()?; // seq_parameter_set_id
    let mut chroma = 1;
    let mut separate_planes = false;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = bits.ue()?;
        if chroma == 3 {
            separate_planes = bits.bit()? == 1;
        }
        bits.ue()?; // bit_depth_luma_minus8
        bits.ue()?; // bit_depth_chroma_minus8
        bits.bit()?; // qpprime_y_zero_transform_bypass_flag
        if bits.bit()? == 1 {
            for list in 0..if chroma == 3 { 12 } else { 8 } {
                if bits.bit()? == 1 {
                    bits.skip_scaling_list(if list < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    bits.ue()?; // log2_max_frame_num_minus4
    match bits.ue()? {
        0 => {
            bits.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            bits.bit()?;
            bits.se()?;
            bits.se()?;
            for _ in 0..bits.ue()?.min(255) {
                bits.se()?;
            }
        }
        _ => {}
    }
    bits.ue()?; // max_num_ref_frames
    bits.bit()?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = bits.ue()?.checked_add(1)?;
    let height_units = bits.ue()?.checked_add(1)?;
    let frame_mbs_only = bits.bit()?;
    if frame_mbs_only == 0 {
        bits.bit()?; // mb_adaptive_frame_field_flag
    }
    bits.bit()?; // direct_8x8_inference_flag
    let mut crop = [0; 4];
    if bits.bit()? == 1 {
        for edge in &mut crop {
            *edge = bits.ue()?;
        }
    }
    let [left, right, top, bottom] = crop;
    let fields = 2 - frame_mbs_only;
    let (unit_x, unit_y) = if chroma == 0 || separate_planes {
        (1, fields)
    } else {
        (
            if chroma == 3 { 1 } else { 2 },
            if chroma == 1 { 2 } else { 1 } * fields,
        )
    };
    let width = width_mbs
        .checked_mul(16)?
        .checked_sub(left.checked_add(right)?.checked_mul(unit_x)?)?;
    let height = height_units
        .checked_mul(16 * fields)?
        .checked_sub(top.checked_add(bottom)?.checked_mul(unit_y)?)?;
    (width > 0 && height > 0).then_some((width, height))
}

/// Removes the emulation prevention byte from each `00 00 03`.
fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(bytes.len());
    let mut zeros = 0;
    for &byte in bytes {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        rbsp.push(byte);
    }
    rbsp
}

struct Bits<'a> {
    data: &'a [u8],
    position: usize,
}
impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let bit = byte >> (7 - self.position % 8) & 1;
        self.position += 1;
        Some(bit.into())
    }

    fn read(&mut self, count: u32) -> Option<u32> {
        (0..count).try_fold(0, |value, _| Some(value << 1 | self.bit()?))
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.position += count;
        (self.position <= self.data.len() * 8).then_some(())
    }

    /// Exp-Golomb, unsigned.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        ((1u32 << zeros) - 1).checked_add(self.read(zeros)?)
    }

    /// Exp-Golomb, signed.
    fn se(&mut self) -> Option<i64> {
        let code = i64::from(self.ue()?);
        Some(if code % 2 == 1 {
            (code + 1) / 2
        } else {
            -(code / 2)
        })
    }

    fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
        let (mut last, mut next) = (8, 8);
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()?).rem_euclid(256);
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use crate::desktop_video::*;
    use crate::test_support::TempRoot;

    #[test]
    fn test_source_loops_a_dumped_clip() {
        // Baseline 1920x1088 macroblocks, cropped by 8 rows to 1080.
        const SPS: [u8; 10] = [0x67, 0x42, 0xc0, 0x28, 0xda, 0x01, 0xe0, 0x08, 0x9f, 0x95];
        let record = |nals: &[&[u8]]| {
            let unit: Vec<u8> = nals
                .iter()
                .flat_map(|nal| [&[0, 0, 0, 1][..], nal].concat())
                .collect();
            [&(unit.len() as u32).to_be_bytes()[..], &unit].concat()
        };
        let keyframe = record(&[&SPS, &[0x68, 0xce, 0x3c, 0x80], &[0x65, 0x88, 0x84]]);
        let predicted = record(&[&[0x41, 0x9a, 0x02]]);
        let temp = TempRoot::new("video-clip");
        let directory = temp.path();
        let start = |name: &str, clip: &[u8], fps: u32| {
            let path = directory.join(name);
            std::fs::write(&path, clip).unwrap();
            let path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
            unsafe { pb_mapper_desktop_video_test_source_v1(path.as_ptr(), fps) }
        };
        assert_eq!(start("bad-fps.h264", &keyframe, 0), 0);
        assert_eq!(start("no-keyframe.h264", &predicted, 30), 0);
        assert_eq!(
            start("truncated.h264", &keyframe[..keyframe.len() - 1], 30),
            0
        );

        // An IDR supersedes whatever is still queued, so the pace leaves the
        // consumer a wide margin to pull each unit before the next arrives.
        let source = start("clip.h264", &[keyframe, predicted].concat(), 10);
        assert!(source > 0);
        let consumer = pb_mapper_desktop_video_attach_v1(source) as u64;
        let pull = |timeout_ms| pull_unit(consumer, timeout_ms);
        let pulled: Vec<_> = (0..3).map(|_| pull(1000)).collect();
        for (index, (code, meta)) in pulled.iter().enumerate() {
            assert_eq!(*code, UNIT);
            assert_eq!((meta.generation, meta.frame_id), (1, index as u64 + 1));
            assert_eq!((meta.width, meta.height), (1920, 1080));
        }
        // The loop restarts at the first IDR; nothing was dropped meanwhile.
        let keyframes: Vec<_> = pulled.iter().map(|(_, meta)| meta.flags).collect();
        let both = FLAG_KEYFRAME | FLAG_DISCONTINUITY;
        assert_eq!(keyframes, [both, 0, FLAG_KEYFRAME]);

        assert_eq!(pb_mapper_desktop_video_test_source_pause_v1(source, 1), 0);
        assert_eq!(pull(20).0, PAUSED);
        assert_eq!(pb_mapper_desktop_video_test_source_pause_v1(source, 0), 0);
        // A resume restarts at the IDR, as a discontinuity.
        let (code, resumed) = pull(1000);
        assert_eq!((code, resumed.flags), (UNIT, both));
        pb_mapper_desktop_video_test_source_stop_v1(source);
        pb_mapper_desktop_video_test_source_stop_v1(source);
        assert_eq!(pull(0).0, CLOSED);
        assert_eq!(
            pb_mapper_desktop_video_test_source_pause_v1(source, 1),
            CLOSED
        );
        pb_mapper_desktop_video_detach_v1(consumer);

        // H.265, as NVENC writes it: the units say so, and the size is the
        // SPS's after its conformance window, 1088 rows cropped to 1080.
        let keyframe = record(&[H265_VPS, H265_SPS_1080, H265_PPS, &[0x26, 0x01, 0xaf, 0x10]]);
        let predicted = record(&[&[0x02, 0x01, 0xd0, 0x10]]);
        let source = start("clip.h265", &[keyframe, predicted].concat(), 10);
        assert!(source > 0);
        let consumer = pb_mapper_desktop_video_attach_v1(source) as u64;
        let pulled = [pull_unit(consumer, 1000), pull_unit(consumer, 1000)];
        assert_eq!(
            pulled.map(|(code, meta)| (code, meta.flags, meta.width, meta.height)),
            [
                (UNIT, both | FLAG_H265, 1920, 1080),
                (UNIT, FLAG_H265, 1920, 1080)
            ]
        );
        pb_mapper_desktop_video_test_source_stop_v1(source);
        pb_mapper_desktop_video_detach_v1(consumer);
        // x265 at 1366x766 pads to 1368x768 and crops one chroma column and row.
        assert_eq!(
            test_source::sps_size(VideoCodec::H265, H265_SPS_766),
            Some((1366, 766))
        );
    }

    fn pull_unit(consumer: u64, timeout_ms: u32) -> (i32, PbDesktopVideoMetaV1) {
        let mut meta = PbDesktopVideoMetaV1::default();
        let mut buffer = [0u8; 256];
        let code = unsafe {
            pb_mapper_desktop_video_next_v1(
                consumer,
                timeout_ms,
                buffer.as_mut_ptr(),
                256,
                &mut meta,
            )
        };
        (code, meta)
    }

    // Parameter sets from real encoders, Main 4:2:0: hevc_nvenc at 1920x1080
    // and libx265 at 1366x766.
    const H265_VPS: &[u8] = &[
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x78, 0x99, 0xc0, 0x90,
    ];
    const H265_SPS_1080: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x11, 0x07, 0xcb, 0x96, 0x67, 0x4a, 0x42, 0x11,
        0x91, 0xbf, 0x50, 0xc0, 0x40, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x07, 0x98,
        0x01, 0x77, 0x94, 0x40, 0x00, 0x2d, 0xc6, 0xc0, 0x00, 0x0f, 0x42, 0x40, 0x40,
    ];
    const H265_PPS: &[u8] = &[0x44, 0x01, 0xc1, 0xa5, 0x7c, 0x0c, 0xc9];
    const H265_SPS_766: &[u8] = &[
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x78, 0xa0, 0x02, 0xac, 0x80, 0x30, 0x1d, 0x56, 0x59, 0x59, 0xa4, 0x93, 0x2b,
        0xc0, 0x5a, 0x02, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x3c, 0x10,
    ];
}
