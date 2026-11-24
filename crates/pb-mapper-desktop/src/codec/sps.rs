//! H.264 sequence parameter sets, read through the VUI and written back.
//!
//! Without a `bitstream_restriction` in the SPS's VUI, a stream may reorder
//! pictures as far as its level's decoded picture buffer allows, and a
//! decoder may hold pictures back for that before it shows them. A stream
//! that never reorders, as every encoder here is set up to make, can say so:
//! `max_num_reorder_frames` 0, and `max_dec_frame_buffering` its reference
//! frames. [`restrict_reorder`] rewrites an SPS that does not say so, once
//! per SPS the encoder writes, never per picture. Neutral: an encoder's
//! Annex B passes through `annexb::ParameterSets`, which calls it.
//!
//! The parser reads every field of `seq_parameter_set_data` up to the VUI's
//! `bitstream_restriction_flag` (High profiles' chroma format, bit depths and
//! scaling matrices included), and the writer writes them back; the scaling
//! matrices and the VUI's fields before the restriction are copied as the
//! bits they were. An SPS the parser does not take whole, or whose rewrite
//! does not read back as written, is sent as the encoder wrote it.

use std::fmt;

/// The profiles whose SPS carries the chroma format, bit depths and scaling
/// matrices (H.264 7.3.2.1.1).
const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
/// H.264's SPS NAL unit type.
const SPS_NAL: u8 = 7;
/// The VUI fields before `bitstream_restriction_flag` of a VUI that sets
/// none of them: eight flags, all 0 (H.264 E.1.1).
const EMPTY_VUI_FIELDS: usize = 8;
/// What a decoder takes for an absent restriction's motion-vector bound, and
/// what VideoToolbox writes: no bound in practice, since every level's
/// motion vectors stay far inside it.
const LOG2_MAX_MV_LENGTH: u32 = 15;

/// What an encoder's H.264 SPS says about output reordering, and what is
/// sent of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpsReorder {
    /// Its VUI says no picture is ever reordered: sent as it is.
    None,
    /// It leaves reordering open: no `bitstream_restriction` (`stated` is
    /// `None`), or `max_num_reorder_frames` of `stated`. Sent rewritten to
    /// say no picture is reordered.
    Rewritten { stated: Option<u32> },
    /// The parser could not take it whole, or its rewrite did not read back
    /// as written: sent as it is.
    Unread,
}
impl fmt::Display for SpsReorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("0"),
            Self::Rewritten { stated: None } => f.write_str("unstated_rewritten_to_0"),
            Self::Rewritten { stated: Some(n) } => write!(f, "{n}_rewritten_to_0"),
            Self::Unread => f.write_str("unread"),
        }
    }
}

/// What `nal`, an H.264 SPS NAL unit with its header byte, says about
/// reordering, and the SPS to send instead when it leaves reordering open:
/// the same SPS with its VUI's restriction saying no picture is reordered,
/// and `max_dec_frame_buffering` its reference frames.
pub(crate) fn restrict_reorder(nal: &[u8]) -> (SpsReorder, Option<Vec<u8>>) {
    let Some((&header, _)) = nal.split_first() else {
        return (SpsReorder::Unread, None);
    };
    let Some(sps) = Sps::parse(nal) else {
        return (SpsReorder::Unread, None);
    };
    let stated = sps.reorder();
    if stated == Some(0) {
        return (SpsReorder::None, None);
    }
    let restricted = sps.without_reorder();
    let rewritten = restricted.to_nal(header);
    // The rewrite must read back as written, its every other field intact.
    if Sps::parse(&rewritten).as_ref() != Some(&restricted) {
        return (SpsReorder::Unread, None);
    }
    (SpsReorder::Rewritten { stated }, Some(rewritten))
}

/// Bits copied as they were written: a stretch of an RBSP.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Bits {
    bytes: Vec<u8>,
    len: usize,
}
impl Bits {
    fn zeros(len: usize) -> Self {
        Self {
            bytes: vec![0; len.div_ceil(8)],
            len,
        }
    }
    fn push(&mut self, bit: bool) {
        if self.len.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit && let Some(byte) = self.bytes.last_mut() {
            *byte |= 0x80 >> (self.len % 8);
        }
        self.len += 1;
    }
    fn get(&self, at: usize) -> bool {
        self.bytes
            .get(at / 8)
            .is_some_and(|byte| byte & (0x80 >> (at % 8)) != 0)
    }
}

/// Reads an RBSP bit by bit; `None` past its end.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }
    fn bit(&mut self) -> Option<bool> {
        let byte = self.data.get(self.at / 8)?;
        let bit = byte & (0x80 >> (self.at % 8)) != 0;
        self.at += 1;
        Some(bit)
    }
    /// `n` bits, at most 32, most significant first: `u(n)`.
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..n.min(32) {
            value = (value << 1) | u32::from(self.bit()?);
        }
        Some(value)
    }
    fn byte(&mut self) -> Option<u8> {
        u8::try_from(self.bits(8)?).ok()
    }
    /// An unsigned Exp-Golomb code: `ue(v)`, up to `u32::MAX - 1`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let suffix = u64::from(self.bits(zeros)?);
        u32::try_from((1u64 << zeros) - 1 + suffix).ok()
    }
    /// A signed Exp-Golomb code: `se(v)`.
    fn se(&mut self) -> Option<i32> {
        let code = i64::from(self.ue()?);
        let value = if code % 2 == 1 {
            (code + 1) / 2
        } else {
            -(code / 2)
        };
        i32::try_from(value).ok()
    }
    /// The bits from `from` up to here, as they were written.
    fn since(&self, from: usize) -> Bits {
        let mut bits = Bits::default();
        let mut source = Reader {
            data: self.data,
            at: from,
        };
        while source.at < self.at {
            match source.bit() {
                Some(bit) => bits.push(bit),
                None => break,
            }
        }
        bits
    }
    /// What is left is `rbsp_trailing_bits`: a 1, then 0s to the end.
    fn trailing(mut self) -> Option<()> {
        if !self.bit()? {
            return None;
        }
        while let Some(bit) = self.bit() {
            if bit {
                return None;
            }
        }
        Some(())
    }
}

/// Writes an RBSP bit by bit.
#[derive(Default)]
struct Writer {
    bits: Bits,
}
impl Writer {
    fn bit(&mut self, bit: bool) {
        self.bits.push(bit);
    }
    fn bits(&mut self, n: u32, value: u32) {
        for shift in (0..n.min(32)).rev() {
            self.bit(value >> shift & 1 == 1);
        }
    }
    fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let len = u64::BITS - code.leading_zeros();
        for _ in 1..len {
            self.bit(false);
        }
        for shift in (0..len).rev() {
            self.bit(code >> shift & 1 == 1);
        }
    }
    fn se(&mut self, value: i32) {
        let value = i64::from(value);
        let code = if value > 0 { 2 * value - 1 } else { -2 * value };
        // A value `Reader::se` read fits: |value| < 2^31.
        self.ue(u32::try_from(code).unwrap_or(u32::MAX - 1));
    }
    fn copy(&mut self, bits: &Bits) {
        for at in 0..bits.len {
            self.bit(bits.get(at));
        }
    }
    /// The RBSP, closed with `rbsp_trailing_bits`.
    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while !self.bits.len.is_multiple_of(8) {
            self.bit(false);
        }
        self.bits.bytes
    }
}

/// A NAL unit's payload without its emulation prevention bytes: each 0x03
/// that follows two 0x00 bytes.
fn unescape(payload: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &byte in payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        rbsp.push(byte);
    }
    rbsp
}

/// An RBSP as a NAL unit's payload: a 0x03 before any byte up to 0x03 that
/// follows two 0x00 bytes, so no start code appears inside it.
fn escape(rbsp: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 1);
    let mut zeros = 0;
    for &byte in rbsp {
        if zeros >= 2 && byte <= 3 {
            payload.push(3);
            zeros = 0;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        payload.push(byte);
    }
    payload
}

/// The chroma format, bit depths and scaling matrices of a High profile.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Chroma {
    format_idc: u32,
    /// Only with `format_idc` 3.
    separate_colour_plane: bool,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
    transform_bypass: bool,
    /// The scaling lists `seq_scaling_matrix_present_flag` brings, as they
    /// were written; `None` without it.
    scaling: Option<Bits>,
}

/// How picture order counts are coded (`pic_order_cnt_type`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum PicOrder {
    /// 0: a counter of `log2_max_lsb_minus4 + 4` bits in each slice.
    Lsb { log2_max_lsb_minus4: u32 },
    /// 1: a cycle of offsets.
    Cycle {
        always_zero: bool,
        offset_for_non_ref_pic: i32,
        offset_for_top_to_bottom_field: i32,
        offsets: Vec<i32>,
    },
    /// 2: output order is decoding order.
    Decoding,
}

/// The VUI's `bitstream_restriction` (H.264 E.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Restriction {
    motion_vectors_over_pic_boundaries: bool,
    max_bytes_per_pic_denom: u32,
    max_bits_per_mb_denom: u32,
    log2_max_mv_length_horizontal: u32,
    log2_max_mv_length_vertical: u32,
    max_num_reorder_frames: u32,
    max_dec_frame_buffering: u32,
}

/// The VUI: its fields before `bitstream_restriction_flag`, as they were
/// written, and the restriction.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Vui {
    fields: Bits,
    restriction: Option<Restriction>,
}

/// An H.264 SPS (`seq_parameter_set_data`, H.264 7.3.2.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sps {
    profile_idc: u8,
    /// `constraint_set0_flag` to `constraint_set5_flag` and the two reserved
    /// bits after them.
    constraints: u8,
    level_idc: u8,
    id: u32,
    /// High profiles only.
    chroma: Option<Chroma>,
    log2_max_frame_num_minus4: u32,
    order: PicOrder,
    max_num_ref_frames: u32,
    gaps_in_frame_num_allowed: bool,
    width_in_mbs_minus1: u32,
    height_in_map_units_minus1: u32,
    /// `None`: frames only. Else `mb_adaptive_frame_field_flag`.
    fields: Option<bool>,
    direct_8x8_inference: bool,
    /// Left, right, top and bottom.
    crop: Option<[u32; 4]>,
    vui: Option<Vui>,
}

impl Sps {
    /// Parse an SPS NAL unit, its header byte included. A zero byte after
    /// its trailing bits, which a byte stream may put before the next start
    /// code, is not part of it.
    pub(crate) fn parse(nal: &[u8]) -> Option<Self> {
        let (&header, payload) = nal.split_first()?;
        if header & 0x80 != 0 || header & 0x1f != SPS_NAL {
            return None;
        }
        let end = payload.iter().rposition(|&byte| byte != 0)? + 1;
        let rbsp = unescape(payload.get(..end)?);
        let mut bits = Reader::new(&rbsp);
        let sps = Self::read(&mut bits)?;
        bits.trailing()?;
        Some(sps)
    }

    fn read(bits: &mut Reader<'_>) -> Option<Self> {
        let profile_idc = bits.byte()?;
        let constraints = bits.byte()?;
        let level_idc = bits.byte()?;
        let id = bits.ue()?;
        if id > 31 {
            return None;
        }
        let chroma = if HIGH_PROFILES.contains(&profile_idc) {
            Some(Chroma::read(bits)?)
        } else {
            None
        };
        let log2_max_frame_num_minus4 = bits.ue()?;
        if log2_max_frame_num_minus4 > 12 {
            return None;
        }
        let order = match bits.ue()? {
            0 => {
                let log2_max_lsb_minus4 = bits.ue()?;
                if log2_max_lsb_minus4 > 12 {
                    return None;
                }
                PicOrder::Lsb {
                    log2_max_lsb_minus4,
                }
            }
            1 => {
                let always_zero = bits.bit()?;
                let offset_for_non_ref_pic = bits.se()?;
                let offset_for_top_to_bottom_field = bits.se()?;
                let cycle = bits.ue()?;
                if cycle > 255 {
                    return None;
                }
                let offsets = (0..cycle).map(|_| bits.se()).collect::<Option<_>>()?;
                PicOrder::Cycle {
                    always_zero,
                    offset_for_non_ref_pic,
                    offset_for_top_to_bottom_field,
                    offsets,
                }
            }
            2 => PicOrder::Decoding,
            _ => return None,
        };
        let max_num_ref_frames = bits.ue()?;
        if max_num_ref_frames > 16 {
            return None;
        }
        let gaps_in_frame_num_allowed = bits.bit()?;
        let width_in_mbs_minus1 = bits.ue()?;
        let height_in_map_units_minus1 = bits.ue()?;
        let fields = if bits.bit()? { None } else { Some(bits.bit()?) };
        let direct_8x8_inference = bits.bit()?;
        let crop = if bits.bit()? {
            Some([bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?])
        } else {
            None
        };
        let vui = if bits.bit()? {
            Some(Vui::read(bits)?)
        } else {
            None
        };
        Some(Self {
            profile_idc,
            constraints,
            level_idc,
            id,
            chroma,
            log2_max_frame_num_minus4,
            order,
            max_num_ref_frames,
            gaps_in_frame_num_allowed,
            width_in_mbs_minus1,
            height_in_map_units_minus1,
            fields,
            direct_8x8_inference,
            crop,
            vui,
        })
    }

    /// The RBSP of this SPS.
    fn write(&self) -> Vec<u8> {
        let Self {
            profile_idc,
            constraints,
            level_idc,
            id,
            chroma,
            log2_max_frame_num_minus4,
            order,
            max_num_ref_frames,
            gaps_in_frame_num_allowed,
            width_in_mbs_minus1,
            height_in_map_units_minus1,
            fields,
            direct_8x8_inference,
            crop,
            vui,
        } = self;
        let mut out = Writer::default();
        out.bits(8, u32::from(*profile_idc));
        out.bits(8, u32::from(*constraints));
        out.bits(8, u32::from(*level_idc));
        out.ue(*id);
        if let Some(chroma) = chroma {
            chroma.write(&mut out);
        }
        out.ue(*log2_max_frame_num_minus4);
        match order {
            PicOrder::Lsb {
                log2_max_lsb_minus4,
            } => {
                out.ue(0);
                out.ue(*log2_max_lsb_minus4);
            }
            PicOrder::Cycle {
                always_zero,
                offset_for_non_ref_pic,
                offset_for_top_to_bottom_field,
                offsets,
            } => {
                out.ue(1);
                out.bit(*always_zero);
                out.se(*offset_for_non_ref_pic);
                out.se(*offset_for_top_to_bottom_field);
                // At most 255, as read.
                out.ue(u32::try_from(offsets.len()).unwrap_or(u32::MAX - 1));
                for offset in offsets {
                    out.se(*offset);
                }
            }
            PicOrder::Decoding => out.ue(2),
        }
        out.ue(*max_num_ref_frames);
        out.bit(*gaps_in_frame_num_allowed);
        out.ue(*width_in_mbs_minus1);
        out.ue(*height_in_map_units_minus1);
        out.bit(fields.is_none());
        if let Some(adaptive) = fields {
            out.bit(*adaptive);
        }
        out.bit(*direct_8x8_inference);
        out.bit(crop.is_some());
        for offset in crop.iter().flatten() {
            out.ue(*offset);
        }
        out.bit(vui.is_some());
        if let Some(vui) = vui {
            vui.write(&mut out);
        }
        out.finish()
    }

    /// This SPS as a NAL unit with `header` in front, emulation prevention
    /// included.
    pub(crate) fn to_nal(&self, header: u8) -> Vec<u8> {
        let payload = escape(&self.write());
        let mut nal = Vec::with_capacity(payload.len() + 1);
        nal.push(header);
        nal.extend_from_slice(&payload);
        nal
    }

    /// `max_num_reorder_frames`, when the VUI states it.
    fn reorder(&self) -> Option<u32> {
        let restriction = self.vui.as_ref()?.restriction.as_ref()?;
        Some(restriction.max_num_reorder_frames)
    }

    /// This SPS saying that no picture is reordered and that a decoder
    /// needs no more pictures than the reference frames: the VUI's
    /// restriction set to that, added with the inferred values of its other
    /// fields where there was none, and a VUI with no other field where
    /// there was no VUI. Nothing else changes, the constraint flags
    /// included.
    fn without_reorder(&self) -> Self {
        let buffering = self.max_num_ref_frames;
        let mut sps = self.clone();
        let vui = sps.vui.get_or_insert_with(|| Vui {
            fields: Bits::zeros(EMPTY_VUI_FIELDS),
            restriction: None,
        });
        let restriction = vui.restriction.get_or_insert(Restriction {
            motion_vectors_over_pic_boundaries: true,
            // 0: no bound stated.
            max_bytes_per_pic_denom: 0,
            max_bits_per_mb_denom: 0,
            log2_max_mv_length_horizontal: LOG2_MAX_MV_LENGTH,
            log2_max_mv_length_vertical: LOG2_MAX_MV_LENGTH,
            max_num_reorder_frames: 0,
            max_dec_frame_buffering: buffering,
        });
        restriction.max_num_reorder_frames = 0;
        restriction.max_dec_frame_buffering = buffering;
        sps
    }
}

impl Chroma {
    fn read(bits: &mut Reader<'_>) -> Option<Self> {
        let format_idc = bits.ue()?;
        if format_idc > 3 {
            return None;
        }
        let separate_colour_plane = format_idc == 3 && bits.bit()?;
        let bit_depth_luma_minus8 = bits.ue()?;
        let bit_depth_chroma_minus8 = bits.ue()?;
        if bit_depth_luma_minus8 > 6 || bit_depth_chroma_minus8 > 6 {
            return None;
        }
        let transform_bypass = bits.bit()?;
        let scaling = if bits.bit()? {
            let from = bits.at;
            let lists = if format_idc == 3 { 12 } else { 8 };
            for list in 0..lists {
                if bits.bit()? {
                    skip_scaling_list(bits, if list < 6 { 16 } else { 64 })?;
                }
            }
            Some(bits.since(from))
        } else {
            None
        };
        Some(Self {
            format_idc,
            separate_colour_plane,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            transform_bypass,
            scaling,
        })
    }

    fn write(&self, out: &mut Writer) {
        let Self {
            format_idc,
            separate_colour_plane,
            bit_depth_luma_minus8,
            bit_depth_chroma_minus8,
            transform_bypass,
            scaling,
        } = self;
        out.ue(*format_idc);
        if *format_idc == 3 {
            out.bit(*separate_colour_plane);
        }
        out.ue(*bit_depth_luma_minus8);
        out.ue(*bit_depth_chroma_minus8);
        out.bit(*transform_bypass);
        out.bit(scaling.is_some());
        if let Some(scaling) = scaling {
            out.copy(scaling);
        }
    }
}

/// Read past one `scaling_list` of `size` entries (H.264 7.3.2.1.1.1).
fn skip_scaling_list(bits: &mut Reader<'_>, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = bits.se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next = (last + delta + 256) % 256;
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// Read past `hrd_parameters` (H.264 E.1.2).
fn skip_hrd(bits: &mut Reader<'_>) -> Option<()> {
    let cpb_cnt_minus1 = bits.ue()?;
    if cpb_cnt_minus1 > 31 {
        return None;
    }
    // bit_rate_scale, cpb_size_scale.
    bits.bits(8)?;
    for _ in 0..=cpb_cnt_minus1 {
        bits.ue()?;
        bits.ue()?;
        bits.bit()?;
    }
    // The four delay and offset lengths.
    bits.bits(20)?;
    Some(())
}

impl Vui {
    fn read(bits: &mut Reader<'_>) -> Option<Self> {
        let from = bits.at;
        // aspect_ratio_info_present_flag: aspect_ratio_idc, and with
        // Extended_SAR (255) the sample aspect ratio.
        if bits.bit()? && bits.byte()? == 255 {
            bits.bits(32)?;
        }
        // overscan_info_present_flag: overscan_appropriate_flag.
        if bits.bit()? {
            bits.bit()?;
        }
        // video_signal_type_present_flag: video_format, the full-range flag,
        // and with colour_description_present_flag the colour description.
        if bits.bit()? {
            bits.bits(4)?;
            if bits.bit()? {
                bits.bits(24)?;
            }
        }
        // chroma_loc_info_present_flag: the two sample locations.
        if bits.bit()? {
            bits.ue()?;
            bits.ue()?;
        }
        // timing_info_present_flag: num_units_in_tick, time_scale and
        // fixed_frame_rate_flag.
        if bits.bit()? {
            bits.bits(32)?;
            bits.bits(32)?;
            bits.bit()?;
        }
        let nal_hrd = bits.bit()?;
        if nal_hrd {
            skip_hrd(bits)?;
        }
        let vcl_hrd = bits.bit()?;
        if vcl_hrd {
            skip_hrd(bits)?;
        }
        if nal_hrd || vcl_hrd {
            // low_delay_hrd_flag.
            bits.bit()?;
        }
        // pic_struct_present_flag.
        bits.bit()?;
        let fields = bits.since(from);
        let restriction = if bits.bit()? {
            Some(Restriction {
                motion_vectors_over_pic_boundaries: bits.bit()?,
                max_bytes_per_pic_denom: bits.ue()?,
                max_bits_per_mb_denom: bits.ue()?,
                log2_max_mv_length_horizontal: bits.ue()?,
                log2_max_mv_length_vertical: bits.ue()?,
                max_num_reorder_frames: bits.ue()?,
                max_dec_frame_buffering: bits.ue()?,
            })
        } else {
            None
        };
        Some(Self {
            fields,
            restriction,
        })
    }

    fn write(&self, out: &mut Writer) {
        let Self {
            fields,
            restriction,
        } = self;
        out.copy(fields);
        out.bit(restriction.is_some());
        if let Some(restriction) = restriction {
            let Restriction {
                motion_vectors_over_pic_boundaries,
                max_bytes_per_pic_denom,
                max_bits_per_mb_denom,
                log2_max_mv_length_horizontal,
                log2_max_mv_length_vertical,
                max_num_reorder_frames,
                max_dec_frame_buffering,
            } = restriction;
            out.bit(*motion_vectors_over_pic_boundaries);
            out.ue(*max_bytes_per_pic_denom);
            out.ue(*max_bits_per_mb_denom);
            out.ue(*log2_max_mv_length_horizontal);
            out.ue(*log2_max_mv_length_vertical);
            out.ue(*max_num_reorder_frames);
            out.ue(*max_dec_frame_buffering);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    /// An SPS sample, its PPS, and the SPS `restrict_reorder` sends instead
    /// (`None`: the sample as it is).
    pub(crate) struct Sample {
        pub(crate) name: &'static str,
        pub(crate) sps: &'static str,
        pub(crate) pps: &'static str,
        pub(crate) rewritten: Option<&'static str>,
    }

    /// SPS and PPS that VideoToolbox's H.264 encoder wrote on the M6 Mac
    /// of docs/benchmarks/2026-11-24-windows-encoder-settings, which
    /// records how. Low-latency rate control states
    /// `max_num_reorder_frames` 0; without it, the VUI says nothing of
    /// reordering, or there is no VUI. Each rewrite was built bit by bit by
    /// that record's own Python reader and writer, not this module's.
    pub(crate) const VIDEO_TOOLBOX: [Sample; 8] = [
        Sample {
            name: "constrained high 1080p, low latency",
            sps: "2764002aac1314501e0089f966a0404040f0804258",
            pps: "28ee3cb0",
            rewritten: None,
        },
        Sample {
            name: "high 720p",
            sps: "27640020ac56805005b9",
            pps: "28ee3cb0",
            rewritten: Some("27640020ac56805005ba01e1008540"),
        },
        Sample {
            name: "high 720p, reordering allowed",
            sps: "27640020ac565014016e40",
            pps: "28ee3cb0",
            rewritten: Some("27640020ac565014016e807840212c"),
        },
        Sample {
            name: "main 640x360, reordering allowed",
            sps: "274d001fab281405ff2a",
            pps: "28ee3c80",
            rewritten: Some("274d001fab281405ff2c03c2010960"),
        },
        Sample {
            name: "baseline 640x360",
            sps: "2742001fab405017fca8",
            pps: "28ce3c80",
            rewritten: Some("2742001fab405017fcb00f08042a"),
        },
        Sample {
            name: "constrained high 1080p, BT.709, low latency",
            sps: "2764002aac1314501e0089f966a0202020f0804258",
            pps: "28ee3cb0",
            rewritten: None,
        },
        Sample {
            name: "constrained high 2160p, low latency",
            sps: "27640034ac1314600f0010fa6a0404040f08042680",
            pps: "28ee3cb0",
            rewritten: None,
        },
        Sample {
            name: "high 1080p, BT.709, reordering allowed",
            sps: "2764002aac56501e0089f966a020202040",
            pps: "28ee3cb0",
            rewritten: Some("2764002aac56501e0089f966a0202020f0804258"),
        },
    ];

    /// SPS built bit by bit by the same record's Python writer, in shapes
    /// VideoToolbox does not write and a Media Foundation encoder may:
    /// timing (whose `num_units_in_tick` of 1 needs emulation prevention)
    /// and an HRD in the VUI with a restriction that allows 2 reordered
    /// frames; scaling matrices, picture order type 1 and field coding;
    /// 4:4:4 with a separate colour plane and a VUI without a restriction.
    const BUILT: [Sample; 3] = [
        Sample {
            name: "timing, NAL HRD, 2 reordered frames",
            sps: "6764002aacb403c0113f2e02d404040500000300010000030078606000f424000f425bdef81e1108b240",
            pps: "68ee3cb0",
            rewritten: Some(
                "6764002aacb403c0113f2e02d404040500000300010000030078606000f424000f425bdef81e1108d4",
            ),
        },
        Sample {
            name: "scaling matrices, picture order cycle, fields",
            sps: "67640c2822da49249249249422121084210842101efffffffffffffcd1a210a218168499",
            // VideoToolbox's High PPS, naming SPS 3.
            pps: "6892e3cb",
            rewritten: Some(
                "67640c2822da49249249249422121084210842101efffffffffffffcd1a210a21816849a01e10085c0",
            ),
        },
        Sample {
            name: "4:4:4, separate colour plane",
            sps: "27f4003392dd94b02802d96a4280",
            pps: "28ee3cb0",
            rewritten: Some("27f4003392dd94b02802d96a43e10084b0"),
        },
    ];

    fn samples() -> impl Iterator<Item = &'static Sample> {
        VIDEO_TOOLBOX.iter().chain(BUILT.iter())
    }

    /// Every sample, and every rewrite, reads whole and writes back byte
    /// for byte: each field is read where the encoder wrote it.
    #[test]
    fn every_sample_writes_back_byte_identically() {
        for sample in samples() {
            for nal in [Some(sample.sps), sample.rewritten].into_iter().flatten() {
                let nal = hex(nal);
                let sps = Sps::parse(&nal).unwrap_or_else(|| panic!("{}", sample.name));
                assert_eq!(sps.to_nal(nal[0]), nal, "{}", sample.name);
            }
        }
    }

    /// What the parser reads matches what the record's own reader found:
    /// sizes, cropping, reference frames and the stated reordering, and
    /// the fields only the built samples have.
    #[test]
    fn the_parser_reads_what_the_encoder_wrote() {
        let read = |text: &str| Sps::parse(&hex(text)).unwrap();
        let sps = read(VIDEO_TOOLBOX[0].sps);
        assert_eq!(
            (sps.profile_idc, sps.constraints, sps.level_idc),
            (100, 0, 42)
        );
        assert_eq!(
            (sps.width_in_mbs_minus1, sps.height_in_map_units_minus1),
            (119, 67)
        );
        assert_eq!((sps.crop, sps.max_num_ref_frames), (Some([0, 0, 0, 4]), 4));
        assert_eq!(
            sps.order,
            PicOrder::Lsb {
                log2_max_lsb_minus4: 9
            }
        );
        let restriction = sps.vui.unwrap().restriction.unwrap();
        assert_eq!(
            (
                restriction.max_num_reorder_frames,
                restriction.max_dec_frame_buffering,
                restriction.log2_max_mv_length_vertical
            ),
            (0, 4, 15)
        );
        let sps = read(VIDEO_TOOLBOX[1].sps);
        assert_eq!((sps.vui, sps.crop, sps.max_num_ref_frames), (None, None, 1));
        let sps = read(VIDEO_TOOLBOX[4].sps);
        assert_eq!((sps.profile_idc, sps.chroma), (66, None));
        let sps = read(VIDEO_TOOLBOX[7].sps);
        assert_eq!(sps.vui.unwrap().restriction, None);

        let sps = read(BUILT[0].sps);
        assert_eq!(sps.order, PicOrder::Decoding);
        assert_eq!(sps.reorder(), Some(2));
        let sps = read(BUILT[1].sps);
        assert_eq!(
            (sps.constraints, sps.id, sps.max_num_ref_frames),
            (0x0c, 3, 2)
        );
        assert!(sps.chroma.unwrap().scaling.is_some());
        assert_eq!(
            sps.order,
            PicOrder::Cycle {
                always_zero: false,
                offset_for_non_ref_pic: -1,
                offset_for_top_to_bottom_field: 1,
                offsets: vec![2, -2, 4],
            }
        );
        assert_eq!(sps.fields, Some(true));
        let sps = read(BUILT[2].sps);
        let chroma = sps.chroma.unwrap();
        assert_eq!(
            (
                chroma.format_idc,
                chroma.separate_colour_plane,
                chroma.bit_depth_luma_minus8
            ),
            (3, true, 2)
        );
        assert!(sps.gaps_in_frame_num_allowed && !sps.direct_8x8_inference);
    }

    /// An SPS that says nothing of reordering, or allows some, is sent
    /// saying none, with the decoder needing only the reference frames, and
    /// is otherwise unchanged; one that already says none is sent as it
    /// is.
    #[test]
    fn an_sps_that_leaves_reordering_open_is_rewritten_to_none() {
        for sample in samples() {
            let original = hex(sample.sps);
            let (reorder, rewritten) = restrict_reorder(&original);
            assert_eq!(
                rewritten.as_deref(),
                sample.rewritten.map(hex).as_deref(),
                "{}",
                sample.name
            );
            let before = Sps::parse(&original).unwrap();
            match &rewritten {
                None => assert_eq!(reorder, SpsReorder::None, "{}", sample.name),
                Some(rewritten) => {
                    assert_eq!(
                        reorder,
                        SpsReorder::Rewritten {
                            stated: before.reorder()
                        },
                        "{}",
                        sample.name
                    );
                    let after = Sps::parse(rewritten).unwrap();
                    let restriction = after.vui.as_ref().unwrap().restriction.clone().unwrap();
                    assert_eq!(restriction.max_num_reorder_frames, 0);
                    assert_eq!(
                        restriction.max_dec_frame_buffering,
                        before.max_num_ref_frames
                    );
                    // Already none: nothing more to rewrite.
                    assert_eq!(restrict_reorder(rewritten), (SpsReorder::None, None));
                    // Everything before the VUI is as it was.
                    assert_eq!(
                        Sps {
                            vui: None,
                            ..after.clone()
                        },
                        Sps {
                            vui: None,
                            ..before.clone()
                        }
                    );
                    // So are the VUI's own fields, where it had a VUI.
                    if let Some(vui) = &before.vui {
                        assert_eq!(after.vui.unwrap().fields, vui.fields);
                    }
                }
            }
        }
        assert_eq!(
            restrict_reorder(&hex(BUILT[0].sps)).0,
            SpsReorder::Rewritten { stated: Some(2) }
        );
        assert_eq!(
            restrict_reorder(&hex(VIDEO_TOOLBOX[1].sps)).0,
            SpsReorder::Rewritten { stated: None }
        );
        assert_eq!(
            SpsReorder::Rewritten { stated: Some(2) }.to_string(),
            "2_rewritten_to_0"
        );
    }

    /// Emulation prevention comes off on reading and goes back on exactly
    /// where a start code would otherwise appear.
    #[test]
    fn emulation_prevention_comes_off_and_goes_back() {
        let rbsp = [0, 0, 0, 0, 0, 1, 0, 0, 2, 0, 0, 3, 0, 0, 4, 0, 0];
        let payload = escape(&rbsp);
        assert_eq!(
            payload,
            [
                0, 0, 3, 0, 0, 3, 0, 1, 0, 0, 3, 2, 0, 0, 3, 3, 0, 0, 4, 0, 0
            ]
        );
        assert_eq!(unescape(&payload), rbsp);
        // The built timing sample's 32-bit 1 and 120 carry two.
        let timing = hex(BUILT[0].sps);
        assert_eq!(timing.windows(3).filter(|run| *run == [0, 0, 3]).count(), 2);
    }

    /// What the parser cannot take whole is sent as the encoder wrote it:
    /// a truncated SPS, another NAL type, bits after the trailing bits, an
    /// unknown picture order type. Zero bytes after the trailing bits are
    /// the byte stream's, not the SPS's.
    #[test]
    fn what_the_parser_cannot_take_whole_is_sent_as_it_is() {
        let sps = hex(VIDEO_TOOLBOX[1].sps);
        let unread = |nal: &[u8]| restrict_reorder(nal) == (SpsReorder::Unread, None);
        assert!(unread(&[]));
        assert!(unread(&sps[..1]));
        assert!(unread(&sps[..sps.len() - 2]));
        let mut pps = sps.clone();
        pps[0] = 0x28;
        assert!(unread(&pps));
        let mut forbidden = sps.clone();
        forbidden[0] |= 0x80;
        assert!(unread(&forbidden));
        // 0xb9's last bit is the stop bit: a 1 after it is not trailing.
        let mut extra = sps.clone();
        extra.push(0x80);
        assert!(unread(&extra));
        // pic_order_cnt_type 3.
        let mut order = Writer::default();
        for byte in [100, 0, 32] {
            order.bits(8, byte);
        }
        order.ue(0);
        order.ue(1);
        order.ue(0);
        order.ue(0);
        order.bit(false);
        order.bit(false);
        order.ue(0);
        order.ue(3);
        let order = [&[0x67][..], &escape(&order.finish())].concat();
        assert!(unread(&order));

        let mut padded = sps.clone();
        padded.extend_from_slice(&[0, 0]);
        assert_eq!(
            restrict_reorder(&padded),
            restrict_reorder(&sps),
            "a padded SPS is rewritten as the bare one"
        );
    }

    /// Core Media takes every rewrite with its PPS as a decoder's format,
    /// at the same dimensions as the encoder's own SPS: what the Mac viewer
    /// builds its decoder from.
    #[cfg(target_os = "macos")]
    #[test]
    fn core_media_takes_every_rewrite() {
        use objc2_core_foundation::CFRetained;
        use objc2_core_media::{
            CMFormatDescription, CMVideoFormatDescriptionCreateFromH264ParameterSets,
            CMVideoFormatDescriptionGetDimensions,
        };
        use std::ptr::NonNull;

        let format = |sps: &[u8], pps: &[u8]| {
            let pointers = [NonNull::from(&sps[0]), NonNull::from(&pps[0])];
            let sizes = [sps.len(), pps.len()];
            let mut out: *const CMFormatDescription = std::ptr::null();
            // SAFETY: two parameter sets the arrays point to and size, which
            // outlive the call, and an out pointer to a local.
            let status = unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    2,
                    NonNull::from(&pointers[0]),
                    NonNull::from(&sizes[0]),
                    4,
                    NonNull::from(&mut out),
                )
            };
            assert_eq!(status, 0);
            // SAFETY: a Create-rule result.
            let description =
                unsafe { CFRetained::from_raw(NonNull::new(out.cast_mut()).unwrap()) };
            // SAFETY: a video format description.
            let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(&description) };
            (dimensions.width, dimensions.height)
        };
        let mut rewrites = 0;
        for sample in samples().filter(|sample| sample.name != BUILT[2].name) {
            let (sps, pps) = (hex(sample.sps), hex(sample.pps));
            let (_, rewritten) = restrict_reorder(&sps);
            let Some(rewritten) = rewritten else {
                continue;
            };
            assert_eq!(
                format(&rewritten, &pps),
                format(&sps, &pps),
                "{}",
                sample.name
            );
            rewrites += 1;
        }
        assert_eq!(rewrites, 7);
    }
}
