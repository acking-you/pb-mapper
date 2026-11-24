//! H.264 sequence parameter sets, read through the VUI and written back,
//! and H.265's, read as far as their conformance window.
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
//!
//! An SPS also says which part of its coded picture a decoder shows: H.264's
//! `frame_cropping`, H.265's `conformance_window`. [`crop_sets`] has a
//! keyframe's SPS show only the top left of a picture coded larger, which a
//! Mac host codes padded to a multiple of 16 (`platform/macos/frame.rs`),
//! every other bit as the encoder wrote it: once per set of parameter sets
//! the encoder writes, never per picture. H.265's is read through the
//! profile, tier and level, its sub-layers', the chroma format and the
//! picture size to the window, and the rest is copied as the bits it was,
//! VUI and extensions included.

use crate::VideoCodec;
use std::fmt;

/// The profiles whose SPS carries the chroma format, bit depths and scaling
/// matrices (H.264 7.3.2.1.1).
const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
/// H.264's SPS NAL unit type.
const SPS_NAL: u8 = 7;
/// H.265's SPS NAL unit type, in the six bits of its two-byte header's first
/// byte below the forbidden bit.
const HEVC_SPS_NAL: u8 = 33;
/// H.265's `sps_max_sub_layers_minus1` at most (H.265 7.4.3.2.1).
const HEVC_MAX_SUB_LAYERS_MINUS1: u32 = 6;
/// What a 4:2:0 picture's cropping counts in: a chroma sample, two luma
/// samples across and two down, for H.264's frames (`CropUnitX` and
/// `CropUnitY`) and H.265's conformance window (`SubWidthC`, `SubHeightC`).
const CHROMA_420_UNIT: u32 = 2;
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

/// `sets`, a keyframe's parameter sets of `codec` in decoding order, each
/// SPS among them showing only the top-left `size` of its coded picture
/// ([`crop_to`]) and every other set as it is. `None` where there is no SPS
/// or one does not crop: the parameter sets would not say what the viewer
/// is to see.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "the Mac host's padded frame alone crops")
)]
pub(crate) fn crop_sets(
    codec: VideoCodec,
    sets: &[&[u8]],
    size: (u32, u32),
) -> Option<Vec<Vec<u8>>> {
    let mut cropped = Vec::with_capacity(sets.len());
    let mut sps = false;
    for set in sets {
        if is_sps(codec, set) {
            cropped.push(crop_to(codec, set, size)?);
            sps = true;
        } else {
            cropped.push(set.to_vec());
        }
    }
    sps.then_some(cropped)
}

/// Whether `nal`, a NAL unit of `codec` with its header, is an SPS.
fn is_sps(codec: VideoCodec, nal: &[u8]) -> bool {
    let Some(&header) = nal.first() else {
        return false;
    };
    match codec {
        VideoCodec::H264 => header & 0x1f == SPS_NAL,
        VideoCodec::H265 => (header >> 1) & 0x3f == HEVC_SPS_NAL,
        VideoCodec::Jpeg => false,
    }
}

/// `nal`, an SPS of `codec` with its NAL header, saying that a decoder
/// shows `size` of its picture: the top left of what it codes, cropped
/// further from the right and the bottom, every other bit as the encoder
/// wrote it. `None` for an SPS this does not read as far as its cropping,
/// one whose picture is not 4:2:0 frames, one that would show less than
/// `size` or an odd number of rows or columns more, and where the rewrite
/// does not read back as written, at `size`.
fn crop_to(codec: VideoCodec, nal: &[u8], size: (u32, u32)) -> Option<Vec<u8>> {
    if size.0 == 0 || size.1 == 0 {
        return None;
    }
    match codec {
        VideoCodec::H264 => {
            let (&header, _) = nal.split_first()?;
            let cropped = Sps::parse(nal)?.cropped_to(size)?;
            let rewritten = cropped.to_nal(header);
            let read = Sps::parse(&rewritten)?;
            (read == cropped && read.shown() == Some(size)).then_some(rewritten)
        }
        VideoCodec::H265 => {
            let (&header, _) = nal.split_first_chunk::<2>()?;
            let cropped = HevcSps::parse(nal)?.cropped_to(size)?;
            let rewritten = cropped.to_nal(header);
            let read = HevcSps::parse(&rewritten)?;
            (read == cropped && read.shown() == Some(size)).then_some(rewritten)
        }
        VideoCodec::Jpeg => None,
    }
}

/// `crop` (left, right, top and bottom, in chroma samples of a 4:2:0
/// picture), with which a decoder shows `shown`, widened on the right and
/// at the bottom so that it shows `size`: `None` where `size` is larger, or
/// an odd number of columns or rows smaller. Cropping nothing is no
/// cropping.
fn crop_further(
    crop: Option<[u32; 4]>,
    shown: (u32, u32),
    (width, height): (u32, u32),
) -> Option<Option<[u32; 4]>> {
    let units = |shown: u32, wanted: u32| {
        let extra = shown.checked_sub(wanted)?;
        extra
            .is_multiple_of(CHROMA_420_UNIT)
            .then_some(extra / CHROMA_420_UNIT)
    };
    let [left, right, top, bottom] = crop.unwrap_or_default();
    let crop = [
        left,
        right.checked_add(units(shown.0, width)?)?,
        top,
        bottom.checked_add(units(shown.1, height)?)?,
    ];
    Some((crop != [0; 4]).then_some(crop))
}

/// `coded` luma samples less `before` and `after` chroma samples of a 4:2:0
/// picture.
fn shown_side(coded: u32, before: u32, after: u32) -> Option<u32> {
    coded.checked_sub(before.checked_add(after)?.checked_mul(CHROMA_420_UNIT)?)
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
    /// Read past `n` bits.
    fn skip(&mut self, n: usize) -> Option<()> {
        let at = self.at.checked_add(n)?;
        if at > self.data.len().checked_mul(8)? {
            return None;
        }
        self.at = at;
        Some(())
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
    /// restriction set to that, and a VUI with no other field where there
    /// was no VUI. A restriction added where there was none states, besides,
    /// what VideoToolbox writes in its own: motion vectors may cross picture
    /// boundaries, at the length an absent restriction allows
    /// (`LOG2_MAX_MV_LENGTH`), as H.264 E.2.1 infers both; and both
    /// denominators 0, which states no bound on a picture's or a
    /// macroblock's size, where E.2.1 would infer 2 and 1. Nothing else
    /// changes, the constraint flags included.
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

    /// The size a decoder shows of a 4:2:0 picture of frames: its
    /// macroblocks less its cropping. `None` for any other picture, whose
    /// cropping counts in other units.
    fn shown(&self) -> Option<(u32, u32)> {
        // Without the chroma format, a profile's is 4:2:0.
        let four_two_zero = self
            .chroma
            .as_ref()
            .is_none_or(|chroma| chroma.format_idc == 1);
        if !four_two_zero || self.fields.is_some() {
            return None;
        }
        let in_mbs = |minus1: u32| minus1.checked_add(1)?.checked_mul(16);
        let [left, right, top, bottom] = self.crop.unwrap_or_default();
        Some((
            shown_side(in_mbs(self.width_in_mbs_minus1)?, left, right)?,
            shown_side(in_mbs(self.height_in_map_units_minus1)?, top, bottom)?,
        ))
    }

    /// This SPS showing `size` of its picture, cropped further from the
    /// right and the bottom; nothing else changes.
    fn cropped_to(&self, size: (u32, u32)) -> Option<Self> {
        let crop = crop_further(self.crop, self.shown()?, size)?;
        Some(Self {
            crop,
            ..self.clone()
        })
    }
}

/// `general_profile_space` through `general_level_idc` (H.265 7.3.3): the
/// space, tier and profile, 32 compatibility flags, four source flags, 43
/// constraint bits and one more, and the level.
const GENERAL_PROFILE_TIER_LEVEL_BITS: usize = 2 + 1 + 5 + 32 + 4 + 43 + 1 + 8;
/// A sub-layer's profile when present: as the general one, without a level.
const SUB_LAYER_PROFILE_BITS: usize = 2 + 1 + 5 + 32 + 4 + 43 + 1;
/// A sub-layer's level when present.
const SUB_LAYER_LEVEL_BITS: usize = 8;
/// The sub-layers `profile_tier_level` keeps two reserved bits for in all.
const SUB_LAYER_PLACES: usize = 8;

/// An H.265 SPS (`seq_parameter_set_rbsp`, H.265 7.3.2.2.1), read as far as
/// its conformance window: the bits before it as they were written, with
/// the chroma format and picture size read from them, the window, and the
/// bits after it up to its `rbsp_trailing_bits` as they were written.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HevcSps {
    /// From `sps_video_parameter_set_id` through
    /// `pic_height_in_luma_samples`.
    head: Bits,
    /// Read from `head`, which writes it.
    chroma_format_idc: u32,
    /// `pic_width_in_luma_samples` and `pic_height_in_luma_samples`, read
    /// from `head`, which writes them.
    coded: (u32, u32),
    /// `conf_win_left_offset`, right, top and bottom, in chroma samples.
    window: Option<[u32; 4]>,
    /// From `bit_depth_luma_minus8` to the stop bit: the VUI and extensions
    /// among it.
    tail: Bits,
}

impl HevcSps {
    /// Parse an SPS NAL unit, its two header bytes included. A zero byte
    /// after its trailing bits, which a byte stream may put before the next
    /// start code, is not part of it.
    fn parse(nal: &[u8]) -> Option<Self> {
        let (&[first, _], payload) = nal.split_first_chunk::<2>()?;
        if first & 0x80 != 0 || (first >> 1) & 0x3f != HEVC_SPS_NAL {
            return None;
        }
        let end = payload.iter().rposition(|&byte| byte != 0)? + 1;
        let rbsp = unescape(payload.get(..end)?);
        let mut bits = Reader::new(&rbsp);
        // sps_video_parameter_set_id.
        bits.bits(4)?;
        let sub_layers_minus1 = bits.bits(3)?;
        if sub_layers_minus1 > HEVC_MAX_SUB_LAYERS_MINUS1 {
            return None;
        }
        // sps_temporal_id_nesting_flag.
        bits.bit()?;
        skip_profile_tier_level(&mut bits, sub_layers_minus1)?;
        // sps_seq_parameter_set_id.
        if bits.ue()? > 15 {
            return None;
        }
        let chroma_format_idc = bits.ue()?;
        if chroma_format_idc > 3 {
            return None;
        }
        if chroma_format_idc == 3 {
            // separate_colour_plane_flag.
            bits.bit()?;
        }
        let coded = (bits.ue()?, bits.ue()?);
        if coded.0 == 0 || coded.1 == 0 {
            return None;
        }
        let head = bits.since(0);
        let window = if bits.bit()? {
            Some([bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?])
        } else {
            None
        };
        // The stop bit is the RBSP's last 1, after everything read.
        let last = rbsp.iter().rposition(|&byte| byte != 0)?;
        let stop = last * 8 + 7 - rbsp.get(last)?.trailing_zeros() as usize;
        if stop < bits.at {
            return None;
        }
        let tail = Reader {
            data: &rbsp,
            at: stop,
        }
        .since(bits.at);
        Some(Self {
            head,
            chroma_format_idc,
            coded,
            window,
            tail,
        })
    }

    /// The RBSP of this SPS.
    fn write(&self) -> Vec<u8> {
        let Self {
            head,
            chroma_format_idc: _,
            coded: _,
            window,
            tail,
        } = self;
        let mut out = Writer::default();
        out.copy(head);
        out.bit(window.is_some());
        for offset in window.iter().flatten() {
            out.ue(*offset);
        }
        out.copy(tail);
        out.finish()
    }

    /// This SPS as a NAL unit with `header` in front, emulation prevention
    /// included.
    fn to_nal(&self, header: [u8; 2]) -> Vec<u8> {
        let payload = escape(&self.write());
        let mut nal = Vec::with_capacity(payload.len() + header.len());
        nal.extend_from_slice(&header);
        nal.extend_from_slice(&payload);
        nal
    }

    /// The size a decoder shows of a 4:2:0 picture: its luma samples less
    /// its conformance window. `None` for any other chroma format, whose
    /// window counts in other units.
    fn shown(&self) -> Option<(u32, u32)> {
        if self.chroma_format_idc != 1 {
            return None;
        }
        let [left, right, top, bottom] = self.window.unwrap_or_default();
        Some((
            shown_side(self.coded.0, left, right)?,
            shown_side(self.coded.1, top, bottom)?,
        ))
    }

    /// This SPS showing `size` of its picture, its window widened on the
    /// right and at the bottom; nothing else changes.
    fn cropped_to(&self, size: (u32, u32)) -> Option<Self> {
        let window = crop_further(self.window, self.shown()?, size)?;
        Some(Self {
            window,
            ..self.clone()
        })
    }
}

/// Read past `profile_tier_level(1, sps_max_sub_layers_minus1)` (H.265
/// 7.3.3): the general profile, tier and level, which flags say each
/// sub-layer's profile and level are present, the reserved bits after
/// them, and those present.
fn skip_profile_tier_level(bits: &mut Reader<'_>, sub_layers_minus1: u32) -> Option<()> {
    bits.skip(GENERAL_PROFILE_TIER_LEVEL_BITS)?;
    let layers = usize::try_from(sub_layers_minus1).ok()?;
    let mut present = [(false, false); HEVC_MAX_SUB_LAYERS_MINUS1 as usize];
    let present = present.get_mut(..layers)?;
    for layer in present.iter_mut() {
        *layer = (bits.bit()?, bits.bit()?);
    }
    if layers > 0 {
        // reserved_zero_2bits.
        bits.skip(2 * (SUB_LAYER_PLACES - layers))?;
    }
    for &(profile, level) in present.iter() {
        if profile {
            bits.skip(SUB_LAYER_PROFILE_BITS)?;
        }
        if level {
            bits.skip(SUB_LAYER_LEVEL_BITS)?;
        }
    }
    Some(())
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
    pub(crate) const BUILT: [Sample; 3] = [
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

    /// Every sample: VideoToolbox's, then the built ones.
    pub(crate) fn samples() -> impl Iterator<Item = &'static Sample> {
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

    /// Parameter sets VideoToolbox wrote in front of a keyframe at a size a
    /// Mac host pads its content to, and that content.
    pub(crate) struct Padded {
        pub(crate) name: &'static str,
        pub(crate) codec: VideoCodec,
        /// In decoding order: H.265's VPS, SPS and PPS.
        pub(crate) sets: &'static [&'static str],
        pub(crate) coded: (u32, u32),
        pub(crate) content: (u32, u32),
    }

    /// What `video_toolbox_parameter_sets` (`platform/macos/tests.rs`)
    /// printed on the M6 Mac of
    /// docs/benchmarks/2026-11-24-mac-vt-padded-capture: H.264 with
    /// low-latency rate control (Constrained High, which this macOS writes
    /// with `constraint_set4` and `constraint_set5`), H.264 without it
    /// (High), and H.265 with it (Main), each coding a picture padded from
    /// 1920x1080, 3024x1964 and 3340x2160.
    pub(crate) const PADDED: [Padded; 9] = [
        Padded {
            name: "constrained high 1920x1088, low latency",
            codec: VideoCodec::H264,
            sets: &["27640c2aac1314501e0089a6a0202020f0804258", "28ee3cb0"],
            coded: (1920, 1088),
            content: (1920, 1080),
        },
        Padded {
            name: "constrained high 3024x1968, low latency",
            codec: VideoCodec::H264,
            sets: &["27640c34ac13142002f40f7a6a0202020f08042220", "28ee3cb0"],
            coded: (3024, 1968),
            content: (3024, 1964),
        },
        Padded {
            name: "constrained high 3344x2160, low latency",
            codec: VideoCodec::H264,
            sets: &["27640c34ac1314700d1010fa6a0202020f08042780", "28ee3cb0"],
            coded: (3344, 2160),
            content: (3340, 2160),
        },
        Padded {
            name: "high 1920x1088",
            codec: VideoCodec::H264,
            sets: &["2764002aac56807802269a80808081", "28ee3cb0"],
            coded: (1920, 1088),
            content: (1920, 1080),
        },
        Padded {
            name: "high 3024x1968",
            codec: VideoCodec::H264,
            sets: &["27640034ac56802f40f7a6a020202040", "28ee3cb0"],
            coded: (3024, 1968),
            content: (3024, 1964),
        },
        Padded {
            name: "high 3344x2160",
            codec: VideoCodec::H264,
            sets: &["27640034ac5680344043e9a808080810", "28ee3cb0"],
            coded: (3344, 2160),
            content: (3340, 2160),
        },
        Padded {
            name: "main 1920x1088, low latency",
            codec: VideoCodec::H265,
            sets: &[
                "40010c03ffff016000000300b0000003000003007b00001b0240",
                "420103016000000300b0000003000003007b0000a003c0801105881b9245232e7e13d0bea1bd529a80808081fc201040",
                "4401c0764c16c9",
            ],
            coded: (1920, 1088),
            content: (1920, 1080),
        },
        Padded {
            name: "main 3024x1968, low latency",
            codec: VideoCodec::H265,
            sets: &[
                "40010c03ffff016000000300b00000030000030099000008c090",
                "420103016000000300b000000300000300990000a0017a2007b1620239245210b9f84f42fa86f543faa823d5529a80808081fc201040",
                "4401c0764c16c9",
            ],
            coded: (3024, 1968),
            content: (3024, 1964),
        },
        Padded {
            name: "main 3344x2160, low latency",
            codec: VideoCodec::H265,
            sets: &[
                "40010c03ffff016000000300b0000003000003009900001b0240",
                "420103016000000300b000000300000300990000a001a220021c5881b9245232e7e13d0bea1bd529a80808081fc20104",
                "4401c0764c16c9",
            ],
            coded: (3344, 2160),
            content: (3340, 2160),
        },
    ];

    /// A padded sample's sets, as bytes.
    pub(crate) fn padded_sets(sample: &Padded) -> Vec<Vec<u8>> {
        sample.sets.iter().map(|set| hex(set)).collect()
    }

    /// The size an SPS of `codec` shows.
    fn shown(codec: VideoCodec, nal: &[u8]) -> Option<(u32, u32)> {
        match codec {
            VideoCodec::H264 => Sps::parse(nal)?.shown(),
            VideoCodec::H265 => HevcSps::parse(nal)?.shown(),
            VideoCodec::Jpeg => None,
        }
    }

    /// Every H.265 sample's SPS, and its crop, reads as far as its window
    /// and writes back byte for byte: the bits before and after it are
    /// where the encoder wrote them.
    #[test]
    fn every_hevc_sps_writes_back_byte_identically() {
        for sample in PADDED
            .iter()
            .filter(|sample| sample.codec == VideoCodec::H265)
        {
            let sps = hex(sample.sets[1]);
            let cropped = crop_to(VideoCodec::H265, &sps, sample.content).unwrap();
            for nal in [sps, cropped] {
                let read = HevcSps::parse(&nal).unwrap_or_else(|| panic!("{}", sample.name));
                assert_eq!(read.to_nal([nal[0], nal[1]]), nal, "{}", sample.name);
                assert_eq!(read.chroma_format_idc, 1);
            }
        }
    }

    /// Each sample's SPS shows the size VideoToolbox coded and, cropped,
    /// the content alone, cropped from the right and the bottom in chroma
    /// samples; every other field, and every other set, is as the encoder
    /// wrote it. Cropping again to the content changes nothing, and
    /// cropping to the coded size gives the encoder's own SPS back.
    #[test]
    fn every_padded_sample_crops_to_its_content() {
        for sample in &PADDED {
            let codec = sample.codec;
            let sets = padded_sets(sample);
            let borrowed: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
            let cropped = crop_sets(codec, &borrowed, sample.content)
                .unwrap_or_else(|| panic!("{}", sample.name));
            assert_eq!(cropped.len(), sets.len());
            let at = usize::from(codec == VideoCodec::H265);
            for (index, (before, after)) in sets.iter().zip(&cropped).enumerate() {
                if index != at {
                    assert_eq!(before, after, "{}", sample.name);
                }
            }
            let (before, after) = (&sets[at], &cropped[at]);
            assert_eq!(shown(codec, before), Some(sample.coded), "{}", sample.name);
            assert_eq!(shown(codec, after), Some(sample.content), "{}", sample.name);
            let extra = (
                (sample.coded.0 - sample.content.0) / 2,
                (sample.coded.1 - sample.content.1) / 2,
            );
            match codec {
                VideoCodec::H264 => {
                    let read = Sps::parse(before).unwrap();
                    let cropped = Sps::parse(after).unwrap();
                    assert_eq!(read.crop, None, "{}", sample.name);
                    assert_eq!(
                        cropped.crop,
                        Some([0, extra.0, 0, extra.1]),
                        "{}",
                        sample.name
                    );
                    assert_eq!(
                        Sps {
                            crop: None,
                            ..cropped
                        },
                        read,
                        "{}",
                        sample.name
                    );
                }
                VideoCodec::H265 | VideoCodec::Jpeg => {
                    let read = HevcSps::parse(before).unwrap();
                    let cropped = HevcSps::parse(after).unwrap();
                    let [left, right, top, bottom] = read.window.unwrap_or_default();
                    assert_eq!(
                        cropped.window,
                        Some([left, right + extra.0, top, bottom + extra.1]),
                        "{}",
                        sample.name
                    );
                    assert_eq!(
                        HevcSps {
                            window: read.window,
                            ..cropped
                        },
                        read,
                        "{}",
                        sample.name
                    );
                }
            }
            assert_eq!(crop_to(codec, after, sample.content).as_ref(), Some(after));
            assert_eq!(crop_to(codec, before, sample.coded).as_ref(), Some(before));
        }
    }

    /// An H.265 SPS of `sub_layers` (whether each one's profile and level
    /// are present), `chroma` (`chroma_format_idc`), `coded` luma samples
    /// and `window`, a few of the usual fields after the window: built bit
    /// by bit, every bit of the profiles and levels a 1, so that a length
    /// misread lands on another size.
    fn hevc_sps(
        sub_layers: &[(bool, bool)],
        chroma: u32,
        coded: (u32, u32),
        window: Option<[u32; 4]>,
    ) -> Vec<u8> {
        let mut out = Writer::default();
        // sps_video_parameter_set_id, sps_max_sub_layers_minus1 and
        // sps_temporal_id_nesting_flag.
        out.bits(4, 0);
        out.bits(3, sub_layers.len() as u32);
        out.bit(true);
        let ones = |out: &mut Writer, n: usize| (0..n).for_each(|_| out.bit(true));
        ones(&mut out, GENERAL_PROFILE_TIER_LEVEL_BITS);
        for &(profile, level) in sub_layers {
            out.bit(profile);
            out.bit(level);
        }
        if !sub_layers.is_empty() {
            out.bits(2 * (SUB_LAYER_PLACES - sub_layers.len()) as u32, 0);
        }
        for &(profile, level) in sub_layers {
            if profile {
                ones(&mut out, SUB_LAYER_PROFILE_BITS);
            }
            if level {
                ones(&mut out, SUB_LAYER_LEVEL_BITS);
            }
        }
        out.ue(0);
        out.ue(chroma);
        if chroma == 3 {
            out.bit(false);
        }
        out.ue(coded.0);
        out.ue(coded.1);
        out.bit(window.is_some());
        for offset in window.iter().flatten() {
            out.ue(*offset);
        }
        // 8-bit depths, a 16-bit picture order count, one sub-layer's
        // ordering, then flags.
        for value in [0, 0, 12, 1, 4, 0] {
            out.ue(value);
        }
        for flag in [true, false, true, true, false] {
            out.bit(flag);
        }
        [&[0x42, 0x01][..], &escape(&out.finish())].concat()
    }

    /// The profile, tier and level of every sub-layer present is read past,
    /// whichever are present: the picture size and window are found where
    /// they are, and the window is widened to crop to the content asked
    /// for.
    #[test]
    fn an_hevc_sps_with_sub_layers_is_read_to_its_window() {
        for sub_layers in [
            &[][..],
            &[(true, true), (false, true)],
            &[(false, false); 6],
            &[
                (true, false),
                (true, true),
                (false, true),
                (true, true),
                (false, false),
                (true, true),
            ],
        ] {
            let sps = hevc_sps(sub_layers, 1, (1928, 1088), Some([0, 4, 0, 4]));
            let read = HevcSps::parse(&sps).unwrap_or_else(|| panic!("{sub_layers:?}"));
            assert_eq!(
                (read.coded, read.window),
                ((1928, 1088), Some([0, 4, 0, 4]))
            );
            assert_eq!(read.shown(), Some((1920, 1080)));
            let cropped = crop_to(VideoCodec::H265, &sps, (1910, 1076)).unwrap();
            let after = HevcSps::parse(&cropped).unwrap();
            assert_eq!(after.window, Some([0, 9, 0, 6]), "{sub_layers:?}");
            assert_eq!((after.head, after.tail), (read.head, read.tail));
        }
        // No window becomes one; a window of nothing is none.
        let sps = hevc_sps(&[], 1, (1920, 1088), None);
        let cropped = crop_to(VideoCodec::H265, &sps, (1920, 1080)).unwrap();
        assert_eq!(HevcSps::parse(&cropped).unwrap().window, Some([0, 0, 0, 4]));
        assert_eq!(crop_to(VideoCodec::H265, &sps, (1920, 1088)), Some(sps));
        // Seven sub-layers is more than H.265 has.
        let seven = hevc_sps(&[(false, false); 7], 1, (64, 64), None);
        assert!(HevcSps::parse(&seven).is_none());
    }

    /// What would not show the content exactly is refused, and the caller
    /// sends nothing cropped: a larger size, an odd number of rows or
    /// columns to crop, an empty size, a picture other than 4:2:0 frames
    /// (H.264's 4:4:4 and field samples, an H.265 4:4:4 one), an SPS cut
    /// short or of the other codec, sets without an SPS, and JPEG.
    #[test]
    fn what_would_not_crop_exactly_is_refused() {
        let h264 = hex(PADDED[0].sets[0]);
        let h265 = hex(PADDED[6].sets[1]);
        for (codec, sps) in [(VideoCodec::H264, &h264), (VideoCodec::H265, &h265)] {
            assert!(crop_to(codec, sps, (1920, 1080)).is_some());
            assert!(crop_to(codec, sps, (1922, 1080)).is_none());
            assert!(crop_to(codec, sps, (1920, 1090)).is_none());
            assert!(crop_to(codec, sps, (1919, 1080)).is_none());
            assert!(crop_to(codec, sps, (1920, 1081)).is_none());
            assert!(crop_to(codec, sps, (0, 1080)).is_none());
            // Cut before the window. H.265's bits after it are copied, not
            // read, so a cut there goes out cut, as the encoder wrote it.
            assert!(crop_to(codec, &sps[..16], (1920, 1080)).is_none());
            assert!(crop_to(VideoCodec::Jpeg, sps, (1920, 1080)).is_none());
        }
        assert!(crop_to(VideoCodec::H264, &h264[..h264.len() - 3], (1920, 1080)).is_none());
        assert!(crop_to(VideoCodec::H264, &hex(BUILT[2].sps), (8, 8)).is_none());
        assert!(crop_to(VideoCodec::H264, &hex(BUILT[1].sps), (8, 8)).is_none());
        let four_four_four = hevc_sps(&[], 3, (1920, 1088), None);
        assert!(HevcSps::parse(&four_four_four).is_some());
        assert!(crop_to(VideoCodec::H265, &four_four_four, (1920, 1080)).is_none());
        assert!(crop_to(VideoCodec::H265, &h264, (1920, 1080)).is_none());
        assert!(crop_to(VideoCodec::H264, &h265, (1920, 1080)).is_none());
        let vps_pps = [hex(PADDED[6].sets[0]), hex(PADDED[6].sets[2])];
        let vps_pps: Vec<&[u8]> = vps_pps.iter().map(Vec::as_slice).collect();
        assert!(crop_sets(VideoCodec::H265, &vps_pps, (1920, 1080)).is_none());
    }
}
