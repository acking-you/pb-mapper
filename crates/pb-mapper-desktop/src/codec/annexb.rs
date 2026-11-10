//! Annex B H.264 and H.265 as encoders produce it: NAL unit ranges, their
//! kinds, keyframes, and the parameter sets a keyframe must carry. Neutral:
//! every hardware encoder's output passes through here.

use crate::{Result, VideoCodec, error};

/// The largest access unit an encoder may hand over.
pub(crate) const MAX_ACCESS_UNIT: usize = 4 * 1024 * 1024;
const _: () = assert!(
    crate::oop::ipc::MAX_FRAME >= MAX_ACCESS_UNIT,
    "the helper pipe carries a whole access unit"
);
const _: () = assert!(
    crate::wire::MAX_PAYLOAD >= MAX_ACCESS_UNIT,
    "a whole access unit fits one payload record"
);

pub(crate) fn codec_name(kind: VideoCodec) -> &'static str {
    match kind {
        VideoCodec::H265 => "H.265",
        VideoCodec::H264 => "H.264",
        VideoCodec::Jpeg => "JPEG",
    }
}

/// An Annex B access unit, with each NAL unit's range in it: from its
/// header to the next start code. The ranges are built from these bytes,
/// so every one is in bounds.
pub(crate) struct AccessUnit<'a> {
    bytes: &'a [u8],
    units: Vec<(usize, usize)>,
}
impl<'a> AccessUnit<'a> {
    /// Find the NAL units. The scan is the same for H.264 and H.265.
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Self> {
        if !bytes.starts_with(&[0, 0, 0, 1]) && !bytes.starts_with(&[0, 0, 1]) {
            return Err(error("hardware encoder did not produce Annex B"));
        }
        let mut units = Vec::new();
        let mut position = 0;
        while position + 3 < bytes.len() {
            let prefix = if bytes[position..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if bytes[position..].starts_with(&[0, 0, 1]) {
                3
            } else {
                position += 1;
                continue;
            };
            if let Some((_, end)) = units.last_mut() {
                *end = position;
            }
            let start = position + prefix;
            if start >= bytes.len() {
                return Err(error("truncated NAL unit"));
            }
            units.push((start, bytes.len()));
            if units.len() > 4096 {
                return Err(error("too many NAL units"));
            }
            position = start;
        }
        if units.is_empty() {
            return Err(error("hardware encoder did not produce Annex B"));
        }
        Ok(Self { bytes, units })
    }

    /// Each NAL unit's kind, in order.
    fn kinds(&self, kind: VideoCodec) -> impl Iterator<Item = Nal> + '_ {
        self.units
            .iter()
            .map(move |&(start, _)| nal(kind, self.bytes[start]))
    }

    /// Whether it holds a picture a decoder can start from.
    pub(crate) fn keyframe(&self, kind: VideoCodec) -> bool {
        self.kinds(kind).any(|nal| nal == Nal::Irap)
    }
}

/// What a NAL unit is, as far as sending it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nal {
    /// A picture a decoder can start from: H.264's IDR (5); H.265's IRAP
    /// pictures (16 to 21), which hold its IDRs (19 and 20) and CRA (21).
    Irap,
    /// H.265's video parameter set (32); H.264 has none.
    Vps,
    /// H.264 7, H.265 33.
    Sps,
    /// H.264 8, H.265 34.
    Pps,
    /// The access unit delimiter, first in its unit when there is one:
    /// H.264 9, H.265 35.
    Delimiter,
    Other,
}

/// A NAL unit's kind from its first header byte: H.264's type is the low five
/// bits; H.265's two-byte header holds its type in the six bits below the
/// forbidden bit, `(b0 >> 1) & 0x3f`.
fn nal(kind: VideoCodec, header: u8) -> Nal {
    match kind {
        VideoCodec::H265 => match (header >> 1) & 0x3f {
            16..=21 => Nal::Irap,
            32 => Nal::Vps,
            33 => Nal::Sps,
            34 => Nal::Pps,
            35 => Nal::Delimiter,
            _ => Nal::Other,
        },
        VideoCodec::H264 | VideoCodec::Jpeg => match header & 0x1f {
            5 => Nal::Irap,
            7 => Nal::Sps,
            8 => Nal::Pps,
            9 => Nal::Delimiter,
            _ => Nal::Other,
        },
    }
}

/// A parameter set a decoder needs before a keyframe.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamSet {
    Vps,
    Sps,
    Pps,
}
impl ParamSet {
    /// The set a NAL unit of this kind is, if it is one.
    fn of(nal: Nal) -> Option<Self> {
        match nal {
            Nal::Vps => Some(Self::Vps),
            Nal::Sps => Some(Self::Sps),
            Nal::Pps => Some(Self::Pps),
            Nal::Irap | Nal::Delimiter | Nal::Other => None,
        }
    }
}

/// The parameter sets an encoder last produced, in-band or in its media type.
pub(crate) struct ParameterSets {
    kind: VideoCodec,
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
}
impl ParameterSets {
    pub(crate) fn new(kind: VideoCodec) -> Self {
        Self {
            kind,
            vps: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
        }
    }
    /// The sets this codec's decoder needs, in decoding order: H.265's VPS
    /// first.
    fn needed(&self) -> impl Iterator<Item = ParamSet> + use<> {
        let vps = (self.kind == VideoCodec::H265).then_some(ParamSet::Vps);
        vps.into_iter().chain([ParamSet::Sps, ParamSet::Pps])
    }
    fn set(&self, set: ParamSet) -> &[u8] {
        match set {
            ParamSet::Vps => &self.vps,
            ParamSet::Sps => &self.sps,
            ParamSet::Pps => &self.pps,
        }
    }
    fn set_mut(&mut self, set: ParamSet) -> &mut Vec<u8> {
        match set {
            ParamSet::Vps => &mut self.vps,
            ParamSet::Sps => &mut self.sps,
            ParamSet::Pps => &mut self.pps,
        }
    }
    pub(crate) fn complete(&self) -> bool {
        self.needed().all(|set| !self.set(set).is_empty())
    }
    /// Whether the unit carries every set this codec needs.
    pub(crate) fn carried(&self, unit: &AccessUnit) -> bool {
        self.needed().all(|needed| {
            unit.kinds(self.kind)
                .any(|nal| ParamSet::of(nal) == Some(needed))
        })
    }
    /// Keep the sets the unit carries.
    pub(crate) fn observe(&mut self, unit: &AccessUnit) -> Result<()> {
        for &(start, end) in &unit.units {
            let Some(set) = ParamSet::of(nal(self.kind, unit.bytes[start])) else {
                continue;
            };
            if end - start > 64 * 1024 {
                return Err(error("parameter set exceeds limit"));
            }
            let target = self.set_mut(set);
            target.clear();
            target.extend_from_slice(&unit.bytes[start..end]);
        }
        Ok(())
    }
    /// The bytes of `keyframe` with every kept set in front of its
    /// pictures: after its delimiter, which stays first.
    pub(crate) fn prepend(&self, keyframe: &AccessUnit) -> Result<Vec<u8>> {
        if !self.complete() {
            return Err(error(format!(
                "{} keyframe has no decoder configuration",
                codec_name(self.kind)
            )));
        }
        let AccessUnit {
            bytes: keyframe,
            units,
        } = keyframe;
        let at = match units.first() {
            Some(&(start, end)) if nal(self.kind, keyframe[start]) == Nal::Delimiter => end,
            _ => 0,
        };
        let mut unit = Vec::with_capacity(
            self.vps.len() + self.sps.len() + self.pps.len() + keyframe.len() + 12,
        );
        unit.extend_from_slice(&keyframe[..at]);
        for set in self.needed() {
            unit.extend_from_slice(&[0, 0, 0, 1]);
            unit.extend_from_slice(self.set(set));
        }
        unit.extend_from_slice(&keyframe[at..]);
        if unit.len() > MAX_ACCESS_UNIT {
            return Err(error("access unit exceeds video limit"));
        }
        Ok(unit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parameter_sets_survive_partial_updates_and_emulation_prevention() -> Result<()> {
        let mut sets = ParameterSets::new(VideoCodec::H264);
        let initial = [0, 0, 0, 1, 0x67, 1, 0, 0, 3, 1, 0, 0, 1, 0x68, 2];
        sets.observe(&AccessUnit::parse(&initial)?)?;
        assert!(sets.complete());
        assert_eq!(sets.sps, &[0x67, 1, 0, 0, 3, 1]);
        let partial = [0, 0, 1, 0x68, 3];
        sets.observe(&AccessUnit::parse(&partial)?)?;
        assert!(sets.complete());
        assert_eq!(sets.pps, &[0x68, 3]);
        assert!(AccessUnit::parse(&[0, 0, 0, 1]).is_err());
        assert!(AccessUnit::parse(&[0, 0, 0, 8, 0x65]).is_err());
        Ok(())
    }
    #[test]
    fn h265_keyframes_and_parameter_sets_follow_its_two_byte_header() -> Result<()> {
        let h265 = |kind: u8| nal(VideoCodec::H265, kind << 1);
        assert_eq!([16, 19, 20, 21].map(h265), [Nal::Irap; 4]);
        assert_eq!(
            [32, 33, 34, 35, 1, 39].map(h265),
            [
                Nal::Vps,
                Nal::Sps,
                Nal::Pps,
                Nal::Delimiter,
                Nal::Other,
                Nal::Other
            ]
        );
        // Read as H.264, an IDR_N_LP header is a PPS.
        assert_eq!(nal(VideoCodec::H264, 20 << 1), Nal::Pps);

        let sets_only = [
            0, 0, 0, 1, 0x40, 1, 0xa, 0, 0, 0, 1, 0x42, 1, 0xb, 0, 0, 1, 0x44, 1, 0xc,
        ];
        let mut sets = ParameterSets::new(VideoCodec::H265);
        sets.observe(&AccessUnit::parse(&sets_only)?)?;
        assert!(sets.complete());
        // An IDR without them gets the VPS, SPS and PPS in front, in order,
        // after its delimiter ...
        let idr = [0, 0, 0, 1, 0x46, 1, 0x50, 0, 0, 0, 1, 0x28, 1, 0xd];
        let idr_unit = AccessUnit::parse(&idr)?;
        assert!(idr_unit.keyframe(VideoCodec::H265));
        assert!(!AccessUnit::parse(&sets_only)?.keyframe(VideoCodec::H265));
        assert!(!sets.carried(&idr_unit));
        assert_eq!(
            sets.prepend(&idr_unit)?,
            [
                0, 0, 0, 1, 0x46, 1, 0x50, 0, 0, 0, 1, 0x40, 1, 0xa, 0, 0, 0, 1, 0x42, 1, 0xb, 0,
                0, 0, 1, 0x44, 1, 0xc, 0, 0, 0, 1, 0x28, 1, 0xd
            ]
        );
        // ... one that carries them goes as it is ...
        let whole = [&sets_only[..], &idr].concat();
        assert!(sets.carried(&AccessUnit::parse(&whole)?));
        // ... and without a VPS there is no decoder configuration.
        let mut partial = ParameterSets::new(VideoCodec::H265);
        partial.observe(&AccessUnit::parse(&sets_only[7..])?)?;
        assert!(!partial.complete() && partial.prepend(&idr_unit).is_err());
        Ok(())
    }
}
