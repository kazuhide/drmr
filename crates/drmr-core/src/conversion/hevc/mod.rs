//! Bounded HEVC configuration and complete access-unit header evidence.
//!
//! Syntax follows ITU-T H.265 (07/2024), clauses 7.3, 7.4 and Annex E:
//! <https://www.itu.int/rec/T-REC-H.265-202407-S/en>.
//! Header validation does not decode entropy-coded coding tree units.
use super::ConversionError;
use crate::sha256::Sha256;
use parameters::{Sps, Vps};
use pps::Pps;
mod bits;
mod parameters;
mod pps;
mod rps;
mod sei;
mod slice;
mod vui;

const BAD: ConversionError = ConversionError("invalid-hevc-syntax");
const CONFIG: ConversionError = ConversionError("native-hevc-configuration-not-supported");
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_UNITS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Metadata {
    pub width: u32,
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub progressive: bool,
    pub sample_aspect_ratio: Option<(u16, u16)>,
    pub full_range: Option<bool>,
    pub colour_primaries: Option<u8>,
    pub transfer_characteristics: Option<u8>,
    pub matrix_coefficients: Option<u8>,
    pub chroma_location: Option<(u8, u8)>,
}

#[derive(Clone, Debug)]
struct Array {
    kind: u8,
    complete: bool,
    units: Vec<Vec<u8>>,
}
#[derive(Clone, Debug)]
pub(in crate::conversion) struct Configuration {
    header: [u8; 22],
    arrays: Vec<Array>,
    vps: Vps,
    sps: Sps,
    pps: Pps,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum RandomAccess {
    None,
    Idr,
    Bla,
    Cra,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct AccessUnit {
    pub slice_count: u32,
    pub is_sync: bool,
    pub random_access: RandomAccess,
    pub leading_picture: bool,
}
impl Configuration {
    pub fn from_hvcc(data: &[u8]) -> Result<(Self, u8), ConversionError> {
        if data.len() < 23
            || data.len() > 1024 * 1024
            || data[0] != 1
            || data[13] & 0xf0 != 0xf0
            || data[15] & 0xfc != 0xfc
            || data[16] & 0xfc != 0xfc
            || data[17] & 0xf8 != 0xf8
            || data[18] & 0xf8 != 0xf8
            || data[21] >> 6 == 3
        {
            return Err(CONFIG);
        }
        let width = (data[21] & 3) + 1;
        if width == 3 {
            return Err(CONFIG);
        }
        let mut at = 23;
        let mut arrays = Vec::new();
        let mut total = 0;
        for _ in 0..data[22] {
            let array = *data.get(at).ok_or(BAD)?;
            at += 1;
            let kind = array & 63;
            if array & 64 != 0
                || !matches!(kind, 32..=34 | 39 | 40)
                || arrays.iter().any(|a: &Array| a.kind == kind)
            {
                return Err(CONFIG);
            }
            let count = word(data, &mut at)?;
            if count == 0 || (matches!(kind, 32..=34) && count != 1) {
                return Err(CONFIG);
            }
            total += usize::from(count);
            if total > MAX_UNITS {
                return Err(CONFIG);
            }
            let mut units = Vec::new();
            for _ in 0..count {
                let size = usize::from(word(data, &mut at)?);
                let end = at.checked_add(size).ok_or(BAD)?;
                let nal = data.get(at..end).ok_or(BAD)?;
                at = end;
                if bits::header(nal)?.kind != kind {
                    return Err(BAD);
                }
                units.push(nal.to_vec());
            }
            arrays.push(Array {
                kind,
                complete: array & 128 != 0,
                units,
            });
        }
        if at != data.len() {
            return Err(BAD);
        }
        arrays.sort_unstable_by_key(|array| array.kind);
        let find = |kind| -> Result<&[u8], ConversionError> {
            Ok(arrays
                .iter()
                .find(|array| array.kind == kind)
                .ok_or(CONFIG)?
                .units[0]
                .as_slice())
        };
        let vps = Vps::parse(find(32)?)?;
        let sps = Sps::parse(find(33)?, &vps)?;
        let pps = Pps::parse(find(34)?, &sps)?;
        if data[1..13] != sps.profile.raw
            || data[16] & 3 != sps.metadata.chroma_format_idc
            || (data[17] & 7) + 8 != sps.metadata.bit_depth_luma
            || (data[18] & 7) + 8 != sps.metadata.bit_depth_chroma
        {
            return Err(CONFIG);
        }
        let temporal = (data[21] >> 3) & 7;
        if temporal != 0 && temporal != sps.max_sub + 1
            || (data[21] & 4 != 0) != sps.nested
            || vps.nested != sps.nested
        {
            return Err(CONFIG);
        }
        let spatial = u16::from_be_bytes([data[13] & 15, data[14]]);
        if spatial != sps.min_spatial_segmentation.unwrap_or(0) {
            return Err(CONFIG);
        }
        if (data[15] & 3 == 2 && !pps.tiles) || (data[15] & 3 == 3 && !pps.wpp) {
            return Err(CONFIG);
        }
        for array in &arrays {
            if matches!(array.kind, 39 | 40) {
                for nal in &array.units {
                    sei::parse(nal, &sps, vps.id)?;
                }
            }
        }
        Ok((
            Self {
                header: data[..22].try_into().map_err(|_| BAD)?,
                arrays,
                vps,
                sps,
                pps,
            },
            width,
        ))
    }
    pub fn validate_entry(&self, codec: [u8; 4]) -> Result<(), ConversionError> {
        match &codec {
            b"hvc1"
                if self
                    .arrays
                    .iter()
                    .filter(|a| matches!(a.kind, 32..=34))
                    .all(|a| a.complete) =>
            {
                Ok(())
            }
            b"hev1" => Ok(()),
            _ => Err(CONFIG),
        }
    }
    pub fn metadata(&self) -> &Metadata {
        &self.sps.metadata
    }
    pub fn codec_string(&self, codec: [u8; 4]) -> Result<String, ConversionError> {
        self.validate_entry(codec)?;
        let profile = self.header[1];
        let space = match profile >> 6 {
            0 => "",
            1 => "A",
            2 => "B",
            _ => "C",
        };
        let compat =
            u32::from_be_bytes(self.header[2..6].try_into().map_err(|_| BAD)?).reverse_bits();
        let mut text = format!(
            "{}.{space}{}.{compat:X}.{}{}",
            std::str::from_utf8(&codec).map_err(|_| CONFIG)?,
            profile & 31,
            if profile & 32 != 0 { "H" } else { "L" },
            self.header[12]
        );
        let constraints = &self.header[6..12];
        let end = constraints
            .iter()
            .rposition(|b| *b != 0)
            .map_or(0, |at| at + 1);
        for byte in &constraints[..end] {
            text.push_str(&format!(".{byte:02X}"));
        }
        Ok(text)
    }
    /// Normalize only length-prefix width and proven parameter-set placement flags.
    /// Optional arrays and every other configuration field remain significant.
    pub fn semantic_identity(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        let mut header = self.header;
        header[21] &= !3;
        hash.update(b"hevc-configuration");
        hash.update(header);
        for array in &self.arrays {
            hash.update([array.kind]);
            if !matches!(array.kind, 32..=34) {
                hash.update([u8::from(array.complete)]);
            }
            hash.update((array.units.len() as u64).to_be_bytes());
            for nal in &array.units {
                hash.update((nal.len() as u64).to_be_bytes());
                hash.update(nal);
            }
        }
        hash.finalize()
    }
    pub fn semantic_eq(&self, other: &Self) -> bool {
        self.semantic_identity() == other.semantic_identity()
    }
    pub fn length_prefixed_access_unit(
        &self,
        data: &[u8],
        width: u8,
        codec: [u8; 4],
    ) -> Result<AccessUnit, ConversionError> {
        self.validate_entry(codec)?;
        let units = units(data, width)?;
        let mut picture = None;
        let mut last_address = None;
        let mut slices = 0_u32;
        let mut suffix = false;
        let mut ended = false;
        let mut timing = false;
        let mut recovery = None;
        let mut aud = None;
        let mut observed_types = 0_u32;
        let mut pending_entry = None;
        for (index, nal) in units.iter().enumerate() {
            let head = bits::header(nal)?;
            if head.temporal > self.sps.max_sub {
                return Err(BAD);
            }
            match head.kind {
                32..=34 if picture.is_none() && !ended => {
                    if codec == *b"hvc1" {
                        return Err(ConversionError(
                            "native-hevc-in-band-parameter-set-not-supported",
                        ));
                    }
                    let original = self
                        .arrays
                        .iter()
                        .find(|a| a.kind == head.kind)
                        .ok_or(CONFIG)?;
                    if original.units[0].as_slice() != *nal {
                        return Err(ConversionError(
                            "native-hevc-parameter-set-change-not-supported",
                        ));
                    }
                }
                35 if index == 0 => {
                    let rbsp = bits::rbsp(nal)?;
                    let b = &mut bits::Bits::new(&rbsp);
                    let kind = b.u(3)?;
                    if kind > 2 {
                        return Err(BAD);
                    }
                    b.trailing()?;
                    aud = Some(kind);
                }
                39 if picture.is_none() && !ended => {
                    let observed = sei::parse(nal, &self.sps, self.vps.id)?;
                    if observed.picture_timing {
                        if timing {
                            return Err(BAD);
                        }
                        timing = true;
                    }
                    if let Some(point) = observed.recovery {
                        if recovery.replace(point).is_some() {
                            return Err(BAD);
                        }
                    }
                }
                0..=9 | 16..=21 if !suffix && !ended => {
                    let header = slice::parse(nal, &self.sps, &self.pps)?;
                    let address = self.pps.tile_scan_address(header.address, &self.sps)?;
                    if (slices == 0 && (!header.first || header.dependent || address != 0))
                        || (slices != 0 && header.first)
                        || last_address.is_some_and(|old| address <= old)
                    {
                        return Err(BAD);
                    }
                    if let Some(old) = &picture {
                        let old: &slice::Picture = old;
                        if old.nal != header.nal
                            || old.no_prior != header.no_prior
                            || header.picture.as_ref().is_some_and(|new| new != old)
                        {
                            return Err(BAD);
                        }
                    } else {
                        picture = header.picture.clone();
                    }
                    if let Some((start, count)) = pending_entry {
                        if self.pps.entry_points(start, address)? != count {
                            return Err(BAD);
                        }
                    }
                    pending_entry = Some((address, header.entry_points));
                    if let Some(kind) = header.slice_type {
                        observed_types |= 1 << kind;
                    }
                    slices = slices.checked_add(1).ok_or(BAD)?;
                    last_address = Some(address);
                }
                40 if picture.is_some() && !ended => {
                    sei::parse(nal, &self.sps, self.vps.id)?;
                    suffix = true;
                }
                36 if picture.is_some() && !ended => {
                    let rbsp = bits::rbsp(nal)?;
                    bits::Bits::new(&rbsp).trailing()?;
                    ended = true;
                }
                37 if picture.is_some() && index + 1 == units.len() => {
                    let rbsp = bits::rbsp(nal)?;
                    bits::Bits::new(&rbsp).trailing()?;
                    ended = true;
                }
                38 if !ended => {
                    let rbsp = bits::rbsp(nal)?;
                    let (&last, fill) = rbsp.split_last().ok_or(BAD)?;
                    if last != 0x80 || fill.iter().any(|b| *b != 255) {
                        return Err(BAD);
                    }
                }
                _ => return Err(ConversionError("native-hevc-nal-order-not-supported")),
            }
        }
        let picture = picture.ok_or(BAD)?;
        if slices == 0 {
            return Err(BAD);
        }
        if let Some((start, count)) = pending_entry {
            if self.pps.entry_points(start, self.sps.ctbs()?)? != count {
                return Err(BAD);
            }
        }
        if aud.is_some_and(|kind| match kind {
            0 => observed_types & !4 != 0,
            1 => observed_types & !6 != 0,
            _ => false,
        }) {
            return Err(BAD);
        }
        let random_access = match picture.nal.kind {
            16..=18 => RandomAccess::Bla,
            19 | 20 => RandomAccess::Idr,
            21 => RandomAccess::Cra,
            _ => RandomAccess::None,
        };
        Ok(AccessUnit {
            slice_count: slices,
            is_sync: random_access != RandomAccess::None,
            random_access,
            leading_picture: matches!(picture.nal.kind, 6..=9),
        })
    }
    pub fn slice_header_bytes(&self, nal: &[u8]) -> Result<usize, ConversionError> {
        Ok(slice::parse(nal, &self.sps, &self.pps)?.clear_bytes)
    }
    pub fn units_with_width(
        &self,
        data: &[u8],
        width: u8,
        codec: [u8; 4],
        hash: &mut Sha256,
    ) -> Result<(), ConversionError> {
        self.length_prefixed_access_unit(data, width, codec)?;
        let mut count = 0_u64;
        for nal in units(data, width)? {
            let kind = bits::header(nal)?.kind;
            // Only identical parameter-set repetitions and validated AUD are framing.
            if matches!(kind, 32..=35) {
                continue;
            }
            count += 1;
            hash.update((nal.len() as u64).to_be_bytes());
            hash.update(nal);
        }
        hash.update(count.to_be_bytes());
        Ok(())
    }
}
fn word(data: &[u8], at: &mut usize) -> Result<u16, ConversionError> {
    let end = at.checked_add(2).ok_or(BAD)?;
    let value = u16::from_be_bytes(data.get(*at..end).ok_or(BAD)?.try_into().map_err(|_| BAD)?);
    *at = end;
    Ok(value)
}
fn units(data: &[u8], width: u8) -> Result<Vec<&[u8]>, ConversionError> {
    if !matches!(width, 1 | 2 | 4) {
        return Err(CONFIG);
    }
    if data.len() > MAX_BYTES {
        return Err(ConversionError("native-hevc-access-unit-size-limit"));
    }
    let mut at = 0_usize;
    let mut result = Vec::new();
    while at < data.len() {
        let end = at.checked_add(usize::from(width)).ok_or(BAD)?;
        let length = data
            .get(at..end)
            .ok_or(BAD)?
            .iter()
            .fold(0_usize, |value, byte| value << 8 | usize::from(*byte));
        at = end;
        let end = at.checked_add(length).ok_or(BAD)?;
        let nal = data.get(at..end).filter(|nal| !nal.is_empty()).ok_or(BAD)?;
        at = end;
        result.push(nal);
        if result.len() > MAX_UNITS {
            return Err(BAD);
        }
    }
    if result.is_empty() {
        return Err(BAD);
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(in crate::conversion) fn test_configuration(depth: u8) -> Vec<u8> {
    tests::hvcc(depth)
}
#[cfg(test)]
pub(in crate::conversion) fn test_slice() -> (Vec<u8>, usize) {
    tests::idr_slice(true, 0, true)
}
