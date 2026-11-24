//! Annex B H.264 and H.265 as encoders produce it: NAL unit ranges, their
//! kinds, keyframes, and the parameter sets a keyframe must carry. Neutral:
//! every hardware encoder's output passes through here.

use super::sps::{SpsReorder, restrict_reorder};
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
    units: &'a [(usize, usize)],
}
impl<'a> AccessUnit<'a> {
    /// Find the NAL units, into `units`: an encoder that keeps it parses
    /// every unit without allocating. The scan is the same for H.264 and
    /// H.265.
    pub(crate) fn parse(bytes: &'a [u8], units: &'a mut Vec<(usize, usize)>) -> Result<Self> {
        units.clear();
        if !bytes.starts_with(&[0, 0, 0, 1]) && !bytes.starts_with(&[0, 0, 1]) {
            return Err(error("hardware encoder did not produce Annex B"));
        }
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
        Ok(Self {
            bytes,
            units: units.as_slice(),
        })
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
/// An H.264 SPS that leaves output reordering open is kept, and sent,
/// rewritten to say no picture is reordered (`sps::restrict_reorder`): in
/// front of a keyframe that comes without it, and in place of the
/// encoder's own in a unit that carries it. It is read once each time the
/// encoder writes a different SPS, never for a unit that repeats it.
pub(crate) struct ParameterSets {
    kind: VideoCodec,
    vps: Vec<u8>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    /// The SPS as the encoder wrote it, while `sps` holds it rewritten;
    /// empty while `sps` is the encoder's own.
    sps_source: Vec<u8>,
    /// What the encoder's last H.264 SPS said about reordering.
    reorder: Option<SpsReorder>,
    /// The SPS read so far.
    #[cfg(test)]
    sps_read: usize,
}
impl ParameterSets {
    pub(crate) fn new(kind: VideoCodec) -> Self {
        Self {
            kind,
            vps: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
            sps_source: Vec::new(),
            reorder: None,
            #[cfg(test)]
            sps_read: 0,
        }
    }
    /// What the encoder's last H.264 SPS said about reordering, and what is
    /// sent of it; `None` before one, and for H.265.
    pub(crate) fn sps_reorder(&self) -> Option<SpsReorder> {
        self.reorder
    }
    /// The `profile_idc` of the kept H.264 SPS, which the rewrite never
    /// changes.
    pub(crate) fn sps_profile(&self) -> Option<u8> {
        if self.kind == VideoCodec::H264 {
            self.sps.get(1).copied()
        } else {
            None
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
        for &(start, end) in unit.units {
            let Some(set) = ParamSet::of(nal(self.kind, unit.bytes[start])) else {
                continue;
            };
            if end - start > 64 * 1024 {
                return Err(error("parameter set exceeds limit"));
            }
            let bytes = &unit.bytes[start..end];
            if set == ParamSet::Sps && self.kind == VideoCodec::H264 {
                self.observe_sps(bytes);
                continue;
            }
            let target = self.set_mut(set);
            target.clear();
            target.extend_from_slice(bytes);
        }
        Ok(())
    }
    /// The kept H.264 SPS as the encoder wrote it, before any rewrite;
    /// empty before one, and for H.265.
    pub(crate) fn sps_written(&self) -> &[u8] {
        match self.kind {
            VideoCodec::H264 if self.sps_source.is_empty() => &self.sps,
            VideoCodec::H264 => &self.sps_source,
            VideoCodec::H265 | VideoCodec::Jpeg => &[],
        }
    }
    /// Keep an H.264 SPS, rewritten where it leaves reordering open: read
    /// only when it differs from the last one the encoder wrote.
    fn observe_sps(&mut self, sps: &[u8]) {
        if self.sps_written() == sps {
            return;
        }
        #[cfg(test)]
        {
            self.sps_read += 1;
        }
        let (reorder, rewritten) = restrict_reorder(sps);
        self.reorder = Some(reorder);
        self.sps.clear();
        self.sps_source.clear();
        match rewritten {
            Some(rewritten) => {
                self.sps = rewritten;
                self.sps_source.extend_from_slice(sps);
            }
            None => self.sps.extend_from_slice(sps),
        }
    }
    /// Whether `nal` is the SPS the encoder wrote and this sends rewritten.
    fn rewritten(&self, nal: &[u8]) -> bool {
        !self.sps_source.is_empty() && nal == self.sps_source.as_slice()
    }
    /// The bytes of `unit` as sent, when they differ from the encoder's: an
    /// SPS this sends rewritten in place of the encoder's own. `None`, and
    /// no copy, for every unit that carries no such SPS.
    pub(crate) fn restate(&self, unit: &AccessUnit) -> Result<Option<Vec<u8>>> {
        if self.sps_source.is_empty()
            || !unit
                .units
                .iter()
                .any(|&(start, end)| self.rewritten(&unit.bytes[start..end]))
        {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(unit.bytes.len() + self.sps.len());
        self.extend_restated(&mut bytes, unit, 0);
        if bytes.len() > MAX_ACCESS_UNIT {
            return Err(error("access unit exceeds video limit"));
        }
        Ok(Some(bytes))
    }
    /// Append `unit`'s bytes from `from` on, an SPS this sends rewritten in
    /// place of the encoder's own.
    fn extend_restated(&self, out: &mut Vec<u8>, unit: &AccessUnit, from: usize) {
        let mut copied = from;
        for &(start, end) in unit.units {
            if start < from || !self.rewritten(&unit.bytes[start..end]) {
                continue;
            }
            out.extend_from_slice(&unit.bytes[copied..start]);
            out.extend_from_slice(&self.sps);
            copied = end;
        }
        out.extend_from_slice(&unit.bytes[copied..]);
    }
    /// The bytes of `keyframe` with every kept set in front of its
    /// pictures: after its delimiter, which stays first. An SPS of its own
    /// that this sends rewritten is rewritten there too.
    pub(crate) fn prepend(&self, keyframe: &AccessUnit) -> Result<Vec<u8>> {
        if !self.complete() {
            return Err(error(format!(
                "{} keyframe has no decoder configuration",
                codec_name(self.kind)
            )));
        }
        let bytes = keyframe.bytes;
        let at = match keyframe.units.first() {
            Some(&(start, end)) if nal(self.kind, bytes[start]) == Nal::Delimiter => end,
            _ => 0,
        };
        let mut unit =
            Vec::with_capacity(self.vps.len() + self.sps.len() + self.pps.len() + bytes.len() + 12);
        unit.extend_from_slice(&bytes[..at]);
        for set in self.needed() {
            unit.extend_from_slice(&[0, 0, 0, 1]);
            unit.extend_from_slice(self.set(set));
        }
        self.extend_restated(&mut unit, keyframe, at);
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
        let mut units = Vec::new();
        let mut sets = ParameterSets::new(VideoCodec::H264);
        let initial = [0, 0, 0, 1, 0x67, 1, 0, 0, 3, 1, 0, 0, 1, 0x68, 2];
        sets.observe(&AccessUnit::parse(&initial, &mut units)?)?;
        assert!(sets.complete());
        assert_eq!(sets.sps, &[0x67, 1, 0, 0, 3, 1]);
        let partial = [0, 0, 1, 0x68, 3];
        sets.observe(&AccessUnit::parse(&partial, &mut units)?)?;
        assert!(sets.complete());
        assert_eq!(sets.pps, &[0x68, 3]);
        // A kept buffer finds what a fresh one does.
        let fresh = AccessUnit::parse(&initial, &mut Vec::new())?.units.to_vec();
        assert_eq!(AccessUnit::parse(&initial, &mut units)?.units, fresh);
        assert!(AccessUnit::parse(&[0, 0, 0, 1], &mut units).is_err());
        assert!(AccessUnit::parse(&[0, 0, 0, 8, 0x65], &mut units).is_err());
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
        let (mut units, mut idr_units) = (Vec::new(), Vec::new());
        let mut sets = ParameterSets::new(VideoCodec::H265);
        sets.observe(&AccessUnit::parse(&sets_only, &mut units)?)?;
        assert!(sets.complete());
        // An IDR without them gets the VPS, SPS and PPS in front, in order,
        // after its delimiter ...
        let idr = [0, 0, 0, 1, 0x46, 1, 0x50, 0, 0, 0, 1, 0x28, 1, 0xd];
        let idr_unit = AccessUnit::parse(&idr, &mut idr_units)?;
        assert!(idr_unit.keyframe(VideoCodec::H265));
        assert!(!AccessUnit::parse(&sets_only, &mut units)?.keyframe(VideoCodec::H265));
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
        assert!(sets.carried(&AccessUnit::parse(&whole, &mut units)?));
        // ... and without a VPS there is no decoder configuration.
        let mut partial = ParameterSets::new(VideoCodec::H265);
        partial.observe(&AccessUnit::parse(&sets_only[7..], &mut units)?)?;
        assert!(!partial.complete() && partial.prepend(&idr_unit).is_err());
        Ok(())
    }

    /// An H.264 SPS that leaves reordering open goes out rewritten: in
    /// place of the encoder's own in the unit that carries it, the rest of
    /// the unit byte for byte, and in front of a keyframe that comes
    /// without it. It is read once per SPS the encoder writes, not per
    /// unit; a P-frame, and an SPS already saying no reordering, go as they
    /// are, without a copy.
    #[test]
    fn an_sps_that_leaves_reordering_open_goes_out_rewritten() -> Result<()> {
        use crate::codec::sps::tests::{VIDEO_TOOLBOX, hex};
        let open = &VIDEO_TOOLBOX[1];
        let (sps, rewritten) = (hex(open.sps), hex(open.rewritten.unwrap()));
        let pps = hex(open.pps);
        let (delimiter, idr, p): (&[u8], &[u8], &[u8]) =
            (&[0x09, 0x10], &[0x65, 0x88, 0x84], &[0x41, 0x9a, 0x02]);
        let annex_b = |units: &[&[u8]]| {
            units
                .iter()
                .enumerate()
                .flat_map(|(at, unit)| {
                    // A 3-byte start code after the first, as some encoders
                    // write: kept as it is.
                    let code: &[u8] = if at == 0 { &[0, 0, 0, 1] } else { &[0, 0, 1] };
                    code.iter().chain(unit.iter()).copied().collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        };
        let mut units = Vec::new();
        let mut sets = ParameterSets::new(VideoCodec::H264);

        let keyframe = annex_b(&[delimiter, &sps, &pps, idr]);
        let unit = AccessUnit::parse(&keyframe, &mut units)?;
        sets.observe(&unit)?;
        assert!(sets.carried(&unit));
        assert_eq!(
            sets.restate(&unit)?,
            Some(annex_b(&[delimiter, &rewritten, &pps, idr]))
        );
        assert_eq!(
            sets.sps_reorder(),
            Some(SpsReorder::Rewritten { stated: None })
        );
        assert_eq!((sets.sps_profile(), sets.sps_read), (Some(100), 1));

        let frame = annex_b(&[p]);
        let unit = AccessUnit::parse(&frame, &mut units)?;
        sets.observe(&unit)?;
        assert_eq!(sets.restate(&unit)?, None);
        // A keyframe without its sets gets the rewrite in front; one with
        // the encoder's SPS but no PPS gets every set in front and its own
        // SPS rewritten.
        let bare = annex_b(&[delimiter, idr]);
        let unit = AccessUnit::parse(&bare, &mut units)?;
        sets.observe(&unit)?;
        assert_eq!(
            sets.prepend(&unit)?,
            [
                &[0, 0, 0, 1][..],
                delimiter,
                &[0, 0, 0, 1],
                &rewritten,
                &[0, 0, 0, 1],
                &pps,
                &[0, 0, 1],
                idr
            ]
            .concat()
        );
        let partial = annex_b(&[&sps, idr]);
        let unit = AccessUnit::parse(&partial, &mut units)?;
        sets.observe(&unit)?;
        assert_eq!(
            sets.prepend(&unit)?,
            [
                &[0, 0, 0, 1][..],
                &rewritten,
                &[0, 0, 0, 1],
                &pps,
                &[0, 0, 0, 1],
                &rewritten,
                &[0, 0, 1],
                idr
            ]
            .concat()
        );
        // The same SPS again, in band, is not read again.
        let again = annex_b(&[&sps, &pps, idr]);
        sets.observe(&AccessUnit::parse(&again, &mut units)?)?;
        assert_eq!(sets.sps_read, 1);

        // An SPS that says no reordering replaces it and goes as it is.
        let none = &VIDEO_TOOLBOX[0];
        let keyframe = annex_b(&[&hex(none.sps), &hex(none.pps), idr]);
        let unit = AccessUnit::parse(&keyframe, &mut units)?;
        sets.observe(&unit)?;
        assert_eq!(sets.restate(&unit)?, None);
        assert_eq!(sets.sps, hex(none.sps));
        assert_eq!(
            (sets.sps_reorder(), sets.sps_read),
            (Some(SpsReorder::None), 2)
        );
        // H.265's SPS is kept as it is.
        let h265 = ParameterSets::new(VideoCodec::H265);
        assert_eq!((h265.sps_reorder(), h265.sps_profile()), (None, None));
        Ok(())
    }
}
