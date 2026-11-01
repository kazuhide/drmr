//! Independently check the declared cipher ranges against parsed VP9 frame boundaries.
use super::{BAD, ConversionError, SampleProtection, Scheme, crypto};
use crate::conversion::vp9::Sample;

pub(in crate::conversion) const LAYOUT: ConversionError =
    ConversionError("native-vp9-cenc-layout-not-supported");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum Vp9Protection {
    Clear,
    CanonicalCenc,
    ShakaTailCenc,
}

fn expected(sample: &Sample, size: u32, prefix: bool) -> Result<[u8; 32], ConversionError> {
    let mut position = 0_usize;
    for frame in &sample.frames {
        if frame.offset != position || frame.size == 0 || frame.header_bytes > frame.size {
            return Err(BAD);
        }
        position = position.checked_add(frame.size).ok_or(BAD)?;
    }
    if position.checked_add(sample.index_bytes) != usize::try_from(size).ok() {
        return Err(BAD);
    }
    crypto::protected_ranges_digest(
        size,
        sample.frames.iter().map(|frame| {
            let body = frame.size.checked_sub(frame.header_bytes).ok_or(BAD)?;
            let residue = body % 16;
            let start = frame
                .offset
                .checked_add(frame.header_bytes)
                .and_then(|value| value.checked_add(if prefix { residue } else { 0 }))
                .ok_or(BAD)?;
            let end = start.checked_add(body - residue).ok_or(BAD)?;
            Ok(u32::try_from(start).map_err(|_| BAD)?..u32::try_from(end).map_err(|_| BAD)?)
        }),
    )
}

pub(in crate::conversion) fn classify(
    sample: &Sample,
    size: u32,
    protection: &SampleProtection,
) -> Result<Vp9Protection, ConversionError> {
    match protection {
        SampleProtection::Clear => Ok(Vp9Protection::Clear),
        SampleProtection::Encrypted {
            scheme: Scheme::Cenc,
            vp9_ranges: Some(actual),
            ..
        } => {
            if *actual == expected(sample, size, true)? {
                Ok(Vp9Protection::CanonicalCenc)
            } else if *actual == expected(sample, size, false)? {
                Ok(Vp9Protection::ShakaTailCenc)
            } else {
                Err(LAYOUT)
            }
        }
        SampleProtection::Encrypted { .. } => Err(LAYOUT),
    }
}
