//! Length-prefixed H.264 and H.265 (the AVCC and HVCC sample format that
//! VideoToolbox and MP4 use) into the Annex B this crate sends: each NAL
//! unit behind a four-byte start code, and a keyframe's parameter sets in
//! front of its pictures, after its access unit delimiter when it has one.

use super::annexb::MAX_ACCESS_UNIT;
use crate::{Result, VideoCodec, error};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Whether `header`, a NAL unit's first byte, is an access unit delimiter:
/// H.264 type 9, H.265 type 35.
fn delimiter(kind: VideoCodec, header: u8) -> bool {
    match kind {
        VideoCodec::H265 => (header >> 1) & 0x3f == 35,
        VideoCodec::H264 | VideoCodec::Jpeg => header & 0x1f == 9,
    }
}

/// The NAL units of `sample`, each behind a big-endian length of
/// `length_size` bytes (1, 2 or 4), every one in bounds and not empty.
fn units(sample: &[u8], length_size: usize) -> impl Iterator<Item = Result<&[u8]>> {
    let mut rest = sample;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let Some((length, after)) = rest.split_at_checked(length_size) else {
            rest = &[];
            return Some(Err(error("truncated NAL unit length")));
        };
        let length = length
            .iter()
            .fold(0usize, |length, byte| (length << 8) | usize::from(*byte));
        match after.split_at_checked(length) {
            Some((unit, next)) if length > 0 => {
                rest = next;
                Some(Ok(unit))
            }
            _ => {
                rest = &[];
                Some(Err(error("NAL unit length outside its sample")))
            }
        }
    })
}

/// `sample`, NAL units behind `length_size`-byte lengths, as Annex B into
/// `out`, which is cleared first. `parameter_sets` (a keyframe's, in decoding
/// order: H.265's VPS, then SPS and PPS) go in front of its first unit, or
/// right after it when that is an access unit delimiter.
pub(crate) fn annex_b(
    kind: VideoCodec,
    sample: &[u8],
    length_size: usize,
    parameter_sets: &[&[u8]],
    out: &mut Vec<u8>,
) -> Result<()> {
    if !matches!(length_size, 1 | 2 | 4) {
        return Err(error("unsupported NAL unit length size"));
    }
    out.clear();
    let sets: usize = parameter_sets.iter().map(|set| set.len() + 4).sum();
    out.reserve(sample.len() + sets);
    let mut first = true;
    for unit in units(sample, length_size) {
        let unit = unit?;
        let leading_delimiter =
            first && unit.first().is_some_and(|header| delimiter(kind, *header));
        if first && !leading_delimiter {
            put_sets(parameter_sets, out);
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(unit);
        if leading_delimiter {
            put_sets(parameter_sets, out);
        }
        first = false;
    }
    if first {
        return Err(error("encoder produced an empty sample"));
    }
    if out.len() > MAX_ACCESS_UNIT {
        return Err(error("access unit exceeds video limit"));
    }
    Ok(())
}

fn put_sets(parameter_sets: &[&[u8]], out: &mut Vec<u8>) {
    for set in parameter_sets {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(set);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::annexb::{AccessUnit, ParameterSets};

    /// A length-prefixed sample of `units`, with `size`-byte lengths.
    fn sample(units: &[&[u8]], size: usize) -> Vec<u8> {
        units
            .iter()
            .flat_map(|unit| {
                let length = unit.len().to_be_bytes();
                length[length.len() - size..]
                    .iter()
                    .chain(unit.iter())
                    .copied()
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// An H.264 IDR with an SEI becomes Annex B with its SPS and PPS first,
    /// which the sender's own parser reads as a keyframe that carries them;
    /// a P-frame goes as it is.
    #[test]
    fn an_h264_keyframe_gets_its_parameter_sets_in_front() -> Result<()> {
        let (sps, pps): (&[u8], &[u8]) = (&[0x67, 0x64, 0, 0x28], &[0x68, 0xee, 0x3c]);
        let (sei, idr): (&[u8], &[u8]) = (&[0x06, 5, 1], &[0x65, 0x88, 0x84]);
        let mut out = Vec::new();
        annex_b(
            VideoCodec::H264,
            &sample(&[sei, idr], 4),
            4,
            &[sps, pps],
            &mut out,
        )?;
        assert_eq!(
            out,
            [
                &START_CODE[..],
                sps,
                &START_CODE,
                pps,
                &START_CODE,
                sei,
                &START_CODE,
                idr
            ]
            .concat()
        );
        let mut ranges = Vec::new();
        let unit = AccessUnit::parse(&out, &mut ranges)?;
        let mut sets = ParameterSets::new(VideoCodec::H264);
        sets.observe(&unit)?;
        assert!(unit.keyframe(VideoCodec::H264) && sets.carried(&unit));

        let p: &[u8] = &[0x41, 0x9a, 0x02];
        annex_b(VideoCodec::H264, &sample(&[p], 2), 2, &[], &mut out)?;
        assert_eq!(out, [&START_CODE[..], p].concat());
        Ok(())
    }

    /// An H.265 access unit delimiter stays first, before the VPS, SPS and
    /// PPS.
    #[test]
    fn an_h265_delimiter_stays_before_the_parameter_sets() -> Result<()> {
        let aud: &[u8] = &[35 << 1, 1, 0x50];
        let (vps, sps, pps): (&[u8], &[u8], &[u8]) =
            (&[32 << 1, 1, 0xa], &[33 << 1, 1, 0xb], &[34 << 1, 1, 0xc]);
        let idr: &[u8] = &[19 << 1, 1, 0xd];
        let mut out = Vec::new();
        annex_b(
            VideoCodec::H265,
            &sample(&[aud, idr], 4),
            4,
            &[vps, sps, pps],
            &mut out,
        )?;
        let expected = [aud, vps, sps, pps, idr]
            .iter()
            .flat_map(|unit| START_CODE.iter().chain(unit.iter()).copied())
            .collect::<Vec<_>>();
        assert_eq!(out, expected);
        let mut ranges = Vec::new();
        let unit = AccessUnit::parse(&out, &mut ranges)?;
        let mut sets = ParameterSets::new(VideoCodec::H265);
        sets.observe(&unit)?;
        assert!(unit.keyframe(VideoCodec::H265) && sets.carried(&unit));
        Ok(())
    }

    /// A length that runs past its sample, an empty unit, an empty sample
    /// and an unknown length size are refused, never read out of bounds.
    #[test]
    fn a_malformed_sample_is_refused() {
        let mut out = Vec::new();
        let mut convert =
            |sample: &[u8], size| annex_b(VideoCodec::H264, sample, size, &[], &mut out).is_err();
        assert!(convert(&[0, 0, 0, 9, 0x65, 1], 4));
        assert!(convert(&[0, 0, 0, 0], 4));
        assert!(convert(&[0, 0], 4));
        assert!(convert(&[], 4));
        assert!(convert(&[1, 0x65], 3));
    }
}
