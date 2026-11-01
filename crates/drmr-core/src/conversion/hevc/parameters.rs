//! Main/Main10 parameter-set relationships and fields needed by every slice.
use super::{
    BAD, CONFIG, ConversionError, Metadata,
    bits::{self, Bits},
    rps::{self, Rps},
    vui::{self, Timing},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Profile {
    pub raw: [u8; 12],
}
impl Profile {
    pub fn parse(b: &mut Bits<'_>) -> Result<Self, ConversionError> {
        let mut raw = [0; 12];
        for byte in &mut raw {
            *byte = b.u8(8)?;
        }
        let profile = raw[0] & 31;
        let compatible = u32::from_be_bytes(raw[1..5].try_into().map_err(|_| BAD)?);
        let constraints = raw[5..11]
            .iter()
            .fold(0_u64, |value, byte| value << 8 | u64::from(*byte));
        if raw[0] >> 6 != 0 || !matches!(profile, 1 | 2) || compatible & 0x0ff0_0000 != 0 {
            return Err(CONFIG);
        }
        let main10 = profile == 2 || compatible & (1 << 29) != 0;
        let allowed = 0xf000_0000_0000_u64 | 1 | if main10 { 1 << 36 } else { 0 };
        if constraints & !allowed != 0 || raw[5] & 0x40 != 0 || constraints & (1 << 36) != 0 {
            return Err(CONFIG);
        }
        if !matches!(
            raw[11],
            30 | 60 | 63 | 90 | 93 | 120 | 123 | 150 | 153 | 156 | 180 | 183 | 186
        ) || (raw[0] & 32 != 0 && raw[11] < 120)
        {
            return Err(CONFIG);
        }
        Ok(Self { raw })
    }
    pub fn depth(&self) -> u8 {
        if self.raw[0] & 31 == 1 { 8 } else { 10 }
    }
}
fn profile_tier_level(b: &mut Bits<'_>, max_sub: u8) -> Result<Profile, ConversionError> {
    let profile = Profile::parse(b)?;
    let mut present = Vec::new();
    for _ in 0..max_sub {
        present.push((b.flag()?, b.flag()?));
    }
    if max_sub != 0 {
        for _ in max_sub..8 {
            if b.u(2)? != 0 {
                return Err(BAD);
            }
        }
    }
    for (has_profile, has_level) in present {
        if has_profile {
            let mut raw = [0_u8; 12];
            for byte in &mut raw[..11] {
                *byte = b.u8(8)?;
            }
            raw[11] = profile.raw[11];
            let mut reader = Bits::new(&raw);
            Profile::parse(&mut reader)?;
        }
        if has_level {
            let level = b.u(8)?;
            if level > u32::from(profile.raw[11]) {
                return Err(BAD);
            }
        }
    }
    Ok(profile)
}
fn ordering(b: &mut Bits<'_>, max_sub: u8) -> Result<u32, ConversionError> {
    let all = b.flag()?;
    let mut prior = None;
    let mut buffer = 0;
    for _ in if all { 0 } else { max_sub }..=max_sub {
        buffer = b.ue(15)?;
        let reorder = b.ue(buffer)?;
        b.ue(u32::MAX - 1)?;
        if prior
            .is_some_and(|(old_buffer, old_reorder)| buffer < old_buffer || reorder < old_reorder)
        {
            return Err(BAD);
        }
        prior = Some((buffer, reorder));
    }
    Ok(buffer)
}

#[derive(Clone, Debug)]
pub(super) struct Vps {
    pub id: u8,
    pub max_sub: u8,
    pub nested: bool,
    pub profile: Profile,
    pub max_buffer: u32,
}
impl Vps {
    pub fn parse(nal: &[u8]) -> Result<Self, ConversionError> {
        if bits::header(nal)?.kind != 32 {
            return Err(BAD);
        }
        let rbsp = bits::rbsp(nal)?;
        let b = &mut Bits::new(&rbsp);
        let id = b.u8(4)?;
        if b.u(2)? != 3 || b.u(6)? != 0 {
            return Err(ConversionError("native-hevc-multilayer-not-supported"));
        }
        let max_sub = b.u8(3)?;
        if max_sub > 6 {
            return Err(BAD);
        }
        let nested = b.flag()?;
        if (max_sub == 0 && !nested) || b.u(16)? != 65535 {
            return Err(BAD);
        }
        let profile = profile_tier_level(b, max_sub)?;
        let max_buffer = ordering(b, max_sub)?;
        if b.u(6)? != 0 || b.ue(1023)? != 0 {
            return Err(ConversionError("native-hevc-multilayer-not-supported"));
        }
        if b.flag()? {
            if b.u(32)? == 0 || b.u(32)? == 0 {
                return Err(BAD);
            }
            if b.flag()? {
                b.ue(u32::MAX - 1)?;
            }
            let count = b.ue(1024)?;
            let mut common = None;
            for index in 0..count {
                if b.ue(1023)? != 0 {
                    return Err(CONFIG);
                }
                let present = index == 0 || b.flag()?;
                common = Some(vui::hrd(b, max_sub, if present { None } else { common })?);
            }
        }
        if b.flag()? {
            return Err(ConversionError("native-hevc-vps-extension-not-supported"));
        }
        b.trailing()?;
        Ok(Self {
            id,
            max_sub,
            nested,
            profile,
            max_buffer,
        })
    }
}

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "The SPS fields are independent HEVC syntax flags, not exclusive states."
)]
pub(super) struct Sps {
    pub id: u32,
    pub profile: Profile,
    pub max_sub: u8,
    pub nested: bool,
    pub metadata: Metadata,
    pub poc_bits: u32,
    pub max_buffer: u32,
    pub ctb_width: u32,
    pub ctb_height: u32,
    pub log2_ctb: u32,
    pub log2_min_cb: u32,
    pub rps: Vec<Rps>,
    pub long_term: Vec<(u32, bool)>,
    pub long_term_present: bool,
    pub temporal_mvp: bool,
    pub sao: bool,
    pub timing: Timing,
    pub min_spatial_segmentation: Option<u16>,
}
impl Sps {
    pub fn parse(nal: &[u8], vps: &Vps) -> Result<Self, ConversionError> {
        if bits::header(nal)?.kind != 33 {
            return Err(BAD);
        }
        let rbsp = bits::rbsp(nal)?;
        let b = &mut Bits::new(&rbsp);
        if b.u(4)? != u32::from(vps.id) {
            return Err(BAD);
        }
        let max_sub = b.u8(3)?;
        let nested = b.flag()?;
        if max_sub > vps.max_sub || max_sub > 6 || (max_sub == 0 && !nested) {
            return Err(BAD);
        }
        let profile = profile_tier_level(b, max_sub)?;
        if profile.raw != vps.profile.raw {
            return Err(CONFIG);
        }
        let id = b.ue(15)?;
        let mut metadata = sample_format(b, &profile)?;
        let poc_bits = b.ue(12)? + 4;
        let max_buffer = ordering(b, max_sub)?;
        if max_buffer > vps.max_buffer {
            return Err(BAD);
        }
        let blocks = coding_blocks(b, &metadata)?;
        let count = b.ue(64)?;
        let mut rps_sets = Vec::new();
        for _ in 0..count {
            rps_sets.push(rps::parse(b, &rps_sets, false, max_buffer)?);
        }
        let long_term_present = b.flag()?;
        let mut long_term = Vec::new();
        if long_term_present {
            let count = b.ue(32)?;
            for _ in 0..count {
                long_term.push((b.u(poc_bits)?, b.flag()?));
            }
        }
        let temporal_mvp = b.flag()?;
        b.flag()?;
        let mut timing = Timing::default();
        let mut min_spatial_segmentation = None;
        if b.flag()? {
            (timing, min_spatial_segmentation) = vui::parse(b, max_sub, &mut metadata)?;
        }
        if b.flag()? && b.u(8)? != 0 {
            return Err(ConversionError("native-hevc-sps-extension-not-supported"));
        }
        b.trailing()?;
        let block = 1_u32 << blocks.log2_ctb;
        let coded_width = metadata.coded_width;
        let coded_height = metadata.coded_height;
        Ok(Self {
            id,
            profile,
            max_sub,
            nested,
            metadata,
            poc_bits,
            max_buffer,
            ctb_width: coded_width.div_ceil(block),
            ctb_height: coded_height.div_ceil(block),
            log2_ctb: blocks.log2_ctb,
            log2_min_cb: blocks.log2_min_cb,
            rps: rps_sets,
            long_term,
            long_term_present,
            temporal_mvp,
            sao: blocks.sao,
            timing,
            min_spatial_segmentation,
        })
    }
    pub fn ctbs(&self) -> Result<u32, ConversionError> {
        self.ctb_width.checked_mul(self.ctb_height).ok_or(BAD)
    }
}

fn sample_format(b: &mut Bits<'_>, profile: &Profile) -> Result<Metadata, ConversionError> {
    let chroma = b.ue8(3)?;
    if chroma != 1 {
        return Err(CONFIG);
    }
    let coded_width = b.ue(65_535)?;
    let coded_height = b.ue(65_535)?;
    if coded_width == 0 || coded_height == 0 {
        return Err(BAD);
    }
    let (mut width, mut height) = (coded_width, coded_height);
    if b.flag()? {
        let left = b.ue(coded_width)?;
        let right = b.ue(coded_width)?;
        let top = b.ue(coded_height)?;
        let bottom = b.ue(coded_height)?;
        width = width
            .checked_sub(
                left.checked_add(right)
                    .and_then(|n| n.checked_mul(2))
                    .ok_or(BAD)?,
            )
            .filter(|n| *n != 0)
            .ok_or(BAD)?;
        height = height
            .checked_sub(
                top.checked_add(bottom)
                    .and_then(|n| n.checked_mul(2))
                    .ok_or(BAD)?,
            )
            .filter(|n| *n != 0)
            .ok_or(BAD)?;
    }
    let luma = 8 + b.ue8(6)?;
    let chroma_depth = 8 + b.ue8(6)?;
    if luma != chroma_depth || !matches!(luma, 8 | 10) || luma > profile.depth() {
        return Err(CONFIG);
    }
    Ok(Metadata {
        width,
        height,
        coded_width,
        coded_height,
        chroma_format_idc: chroma,
        bit_depth_luma: luma,
        bit_depth_chroma: chroma_depth,
        progressive: true,
        sample_aspect_ratio: None,
        full_range: None,
        colour_primaries: None,
        transfer_characteristics: None,
        matrix_coefficients: None,
        chroma_location: None,
    })
}

struct Blocks {
    log2_min_cb: u32,
    log2_ctb: u32,
    sao: bool,
}
fn coding_blocks(b: &mut Bits<'_>, metadata: &Metadata) -> Result<Blocks, ConversionError> {
    let log2_min_cb = b.ue(3)? + 3;
    let log2_ctb = log2_min_cb + b.ue(3)?;
    if !(4..=6).contains(&log2_ctb) {
        return Err(BAD);
    }
    let log2_min_transform = b.ue(3)? + 2;
    let log2_max_transform = log2_min_transform + b.ue(3)?;
    if log2_min_transform >= log2_min_cb || log2_max_transform > log2_ctb.min(5) {
        return Err(BAD);
    }
    b.ue(log2_ctb - log2_min_transform)?;
    b.ue(log2_ctb - log2_min_transform)?;
    if b.flag()? && b.flag()? {
        scaling_list(b)?;
    }
    b.flag()?; // asymmetric motion partitions
    let sao = b.flag()?;
    if b.flag()? {
        if b.u(4)? + 1 > u32::from(metadata.bit_depth_luma)
            || b.u(4)? + 1 > u32::from(metadata.bit_depth_chroma)
        {
            return Err(BAD);
        }
        let min_pcm = b.ue(2)? + 3;
        let max_pcm = min_pcm + b.ue(2)?;
        if min_pcm < log2_min_cb || max_pcm > log2_ctb.min(5) {
            return Err(BAD);
        }
        b.flag()?;
    }
    Ok(Blocks {
        log2_min_cb,
        log2_ctb,
        sao,
    })
}

pub(super) fn scaling_list(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    for size_id in 0..4 {
        let stride = if size_id == 3 { 3 } else { 1 };
        for matrix in (0_u32..6).step_by(stride) {
            if b.flag()? {
                if size_id > 1 {
                    b.se(-7, 247)?;
                }
                for _ in 0..(1_u32 << (4 + 2 * size_id)).min(64) {
                    b.se(-128, 127)?;
                }
            } else {
                let delta = b.ue(matrix)?;
                if size_id == 3 && delta % 3 != 0 {
                    return Err(BAD);
                }
            }
        }
    }
    Ok(())
}
