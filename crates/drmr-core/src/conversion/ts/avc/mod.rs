//! AVC syntax evidence, independent of entropy decoding.
//!
//! The parser follows ITU-T H.264 (08/2024), clauses 7.3, 7.4 and Annex E:
//! <https://www.itu.int/rec/T-REC-H.264-202408-I/en>.
//! Header boundaries were also cross-checked against Shaka Packager v3.4.2:
//! <https://github.com/shaka-project/shaka-packager/blob/c819deaa2376399a89d41f3804bc72f4a20d9d6d/packager/media/codecs/h264_parser.cc>.
//! Valid headers do not establish that every entropy-coded macroblock decodes.

mod bits;
mod parameters;
mod sei;
mod slice;

use super::super::ConversionError;
use parameters::{Pps, Sps};

pub(in crate::conversion) const BAD: ConversionError = ConversionError("invalid-avc-syntax");
const CONFIG: ConversionError = ConversionError("ts-codec-configuration-not-supported");
const PROGRESSIVE: ConversionError = ConversionError("ts-progressive-frame-evidence-required");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct VideoClock {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub fixed_frame_rate: bool,
    pub pic_struct_present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Metadata {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub chroma_format_idc: u8,
    pub separate_colour_plane: bool,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub sample_aspect_ratio: Option<(u16, u16)>,
    pub video_format: Option<u8>,
    pub full_range: Option<bool>,
    pub colour_primaries: Option<u8>,
    pub transfer_characteristics: Option<u8>,
    pub matrix_coefficients: Option<u8>,
    pub chroma_location: Option<(u8, u8)>,
    pub frame_mbs_only: bool,
    pub mb_adaptive_frame_field: bool,
    pub clock: Option<VideoClock>,
    pub max_num_reorder_frames: Option<u32>,
}

#[derive(Clone)]
pub(in crate::conversion) struct Configuration {
    sps: Sps,
    pps: Pps,
    sps_bytes: Vec<u8>,
    pps_bytes: Vec<u8>,
}

impl Configuration {
    pub fn parse(sps: &[u8], pps: &[u8]) -> Result<Self, ConversionError> {
        let parsed_sps = Sps::parse(sps)?;
        let parsed_pps = Pps::parse(pps, &parsed_sps)?;
        Ok(Self {
            sps: parsed_sps,
            pps: parsed_pps,
            sps_bytes: sps.to_vec(),
            pps_bytes: pps.to_vec(),
        })
    }
    pub fn metadata(&self) -> &Metadata {
        &self.sps.metadata
    }
    pub fn from_avcc(data: &[u8]) -> Result<(Self, u8), ConversionError> {
        if data.len() < 7 || data[0] != 1 || data[4] & 0xfc != 0xfc || data[5] != 0xe1 {
            return Err(CONFIG);
        }
        let width = (data[4] & 3) + 1;
        if width == 3 {
            return Err(CONFIG);
        }
        let mut at = 6;
        let sps = configuration_unit(data, &mut at)?;
        if data.get(at) != Some(&1) {
            return Err(CONFIG);
        }
        at += 1;
        let pps = configuration_unit(data, &mut at)?;
        let config = Self::parse(sps, pps)?;
        if data.get(1..4) != sps.get(1..4) {
            return Err(CONFIG);
        }
        if at != data.len() {
            if matches!(data[1], 66 | 77 | 88) || data.len() - at != 4 {
                return Err(CONFIG);
            }
            let m = config.metadata();
            if data[at] & 0xfc != 0xfc
                || data[at + 1] & 0xf8 != 0xf8
                || data[at + 2] & 0xf8 != 0xf8
                || data[at + 3] != 0
                || data[at] & 3 != m.chroma_format_idc
                || (data[at + 1] & 7) + 8 != m.bit_depth_luma
                || (data[at + 2] & 7) + 8 != m.bit_depth_chroma
            {
                return Err(CONFIG);
            }
        }
        Ok((config, width))
    }
    pub fn parameter_sets(&self) -> (&[u8], &[u8]) {
        (&self.sps_bytes, &self.pps_bytes)
    }
    pub fn length_prefixed_access_unit(
        &self,
        data: &[u8],
        width: u8,
    ) -> Result<AccessUnit, ConversionError> {
        if !matches!(width, 1 | 2 | 4) {
            return Err(CONFIG);
        }
        if data.len() > 8 * 1024 * 1024 {
            return Err(ConversionError("ts-access-unit-size-limit"));
        }
        let mut at = 0_usize;
        let mut units = Vec::new();
        while at < data.len() {
            let end = at.checked_add(usize::from(width)).ok_or(BAD)?;
            let length = data
                .get(at..end)
                .ok_or(BAD)?
                .iter()
                .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
            at = end;
            let end = at.checked_add(length).ok_or(BAD)?;
            units.push(data.get(at..end).filter(|n| !n.is_empty()).ok_or(BAD)?);
            at = end;
            if units.len() > 4096 {
                return Err(BAD);
            }
        }
        self.access_unit(&units)
    }
    pub fn access_unit(&self, units: &[&[u8]]) -> Result<AccessUnit, ConversionError> {
        if units.is_empty() || units.len() > 4096 {
            return Err(BAD);
        }
        if units
            .iter()
            .try_fold(0_usize, |sum, n| sum.checked_add(n.len()))
            .is_none_or(|n| n > 8 * 1024 * 1024)
        {
            return Err(ConversionError("ts-access-unit-size-limit"));
        }
        let mut picture: Option<slice::Picture> = None;
        let mut last_mb = None;
        let mut slice_count = 0_u32;
        let mut picture_structure = None;
        let mut recovery_point = None;
        let mut ended = false;
        let mut permitted_slice_types = 31_u32;
        let mut observed_slice_types = 0_u32;
        for (at, unit) in units.iter().enumerate() {
            let kind = nal_kind(unit)?;
            match kind {
                7 if picture.is_none() && *unit == self.sps_bytes => {}
                8 if picture.is_none() && *unit == self.pps_bytes => {}
                7 | 8 => return Err(CONFIG),
                9 if at == 0 => {
                    let rbsp = bits::rbsp(unit, 9)?;
                    let b = &mut bits::Bits::new(&rbsp);
                    let primary = b.bits(3)?;
                    permitted_slice_types = [4, 5, 7, 16, 24, 20, 29, 31][primary as usize];
                    b.trailing()?;
                }
                6 if picture.is_none() => {
                    let observed = sei::parse(unit, &self.sps)?;
                    if let Some(observed) = observed.picture_structure
                        && picture_structure.replace(observed).is_some()
                    {
                        return Err(BAD);
                    }
                    if let Some(observed) = observed.recovery_point
                        && recovery_point.replace(observed).is_some()
                    {
                        return Err(BAD);
                    }
                }
                1 | 5 if !ended => {
                    let parsed = slice::parse(unit, &self.sps, &self.pps)?;
                    if picture.as_ref().is_some_and(|old| old != &parsed.picture) {
                        return Err(BAD);
                    }
                    if last_mb.is_some_and(|old| parsed.first_mb <= old)
                        || (last_mb.is_none() && parsed.first_mb != 0)
                    {
                        return Err(ConversionError("ts-avc-slice-order-not-supported"));
                    }
                    observed_slice_types |= 1 << parsed.slice_type;
                    if parsed.all_slices_same_type {
                        permitted_slice_types &= 1 << parsed.slice_type;
                    }
                    if observed_slice_types & !permitted_slice_types != 0 {
                        return Err(BAD);
                    }
                    picture = Some(parsed.picture);
                    last_mb = Some(parsed.first_mb);
                    slice_count = slice_count.checked_add(1).ok_or(BAD)?;
                }
                10 | 11 if picture.is_some() && unit.len() == 1 => {
                    ended = true;
                }
                12 => {
                    let rbsp = bits::rbsp(unit, 12)?;
                    let (&last, filler) = rbsp.split_last().ok_or(BAD)?;
                    if last != 0x80 || filler.iter().any(|n| *n != 0xff) {
                        return Err(BAD);
                    }
                }
                _ => return Err(ConversionError("ts-nal-type-not-supported")),
            }
        }
        if slice_count == 0 {
            return Err(BAD);
        }
        match picture_structure {
            Some(1..=6) => return Err(PROGRESSIVE),
            Some(7 | 8) => return Err(ConversionError("ts-avc-picture-duration-not-supported")),
            Some(9..) => return Err(BAD),
            _ => {}
        }
        if self.sps.metadata.mb_adaptive_frame_field {
            return Err(PROGRESSIVE);
        }
        Ok(AccessUnit {
            slice_count,
            picture_structure,
            is_idr: picture.as_ref().ok_or(BAD)?.is_idr(),
            recovery_point,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct AccessUnit {
    pub slice_count: u32,
    pub picture_structure: Option<u8>,
    pub is_idr: bool,
    pub recovery_point: Option<RecoveryPoint>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct RecoveryPoint {
    pub frame_count: u32,
    pub exact_match: bool,
    pub broken_link: bool,
    pub changing_slice_group_idc: u8,
}

fn configuration_unit<'a>(data: &'a [u8], at: &mut usize) -> Result<&'a [u8], ConversionError> {
    let end = at.checked_add(2).ok_or(CONFIG)?;
    let word = data.get(*at..end).ok_or(CONFIG)?;
    let size = usize::from(u16::from_be_bytes([word[0], word[1]]));
    *at = end;
    let end = at.checked_add(size).ok_or(CONFIG)?;
    let unit = data.get(*at..end).filter(|n| !n.is_empty()).ok_or(CONFIG)?;
    *at = end;
    Ok(unit)
}

fn nal_kind(unit: &[u8]) -> Result<u8, ConversionError> {
    let head = *unit.first().ok_or(BAD)?;
    let kind = head & 31;
    if head & 0x80 != 0
        || (matches!(kind, 5 | 7 | 8) && head & 0x60 == 0)
        || (matches!(kind, 6 | 9 | 10 | 11 | 12) && head & 0x60 != 0)
    {
        return Err(BAD);
    }
    Ok(kind)
}

#[cfg(test)]
mod tests;
