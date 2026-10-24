use super::{
    BAD, CONFIG, ConversionError, Metadata, VideoClock,
    bits::{self, Bits},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hrd {
    pub cpb_count: u32,
    pub initial_delay_length: u32,
    pub cpb_delay_length: u32,
    pub dpb_delay_length: u32,
    pub time_offset_length: u32,
}

#[derive(Clone, Copy, Default)]
pub(super) struct SeiTiming {
    pub nal_hrd: Option<Hrd>,
    pub vcl_hrd: Option<Hrd>,
    pub pic_struct_present: bool,
}

#[derive(Clone)]
pub(super) struct Sps {
    pub metadata: Metadata,
    pub id: u32,
    pub log2_frame_num: u32,
    pub poc_type: u32,
    pub log2_poc_lsb: u32,
    pub delta_poc_always_zero: bool,
    pub width_mbs: u32,
    pub height_map_units: u32,
    pub sei_timing: SeiTiming,
}
impl Sps {
    pub fn parse(data: &[u8]) -> Result<Self, ConversionError> {
        if data.len() > 65_535 {
            return Err(CONFIG);
        }
        let rbsp = bits::rbsp(data, 7)?;
        let b = &mut Bits::new(&rbsp);
        let identity = identity(b)?;
        let format = sample_format(b, identity.profile_idc)?;
        let order = picture_order(b)?;
        let max_ref_frames = b.ue(16)?;
        b.flag()?; // gaps_in_frame_num_value_allowed_flag
        let dimensions = dimensions(b, format)?;
        let mut metadata = Metadata {
            profile_idc: identity.profile_idc,
            constraint_flags: identity.constraint_flags,
            level_idc: identity.level_idc,
            width: dimensions.width,
            height: dimensions.height,
            coded_width: dimensions.coded_width,
            coded_height: dimensions.coded_height,
            chroma_format_idc: format.chroma,
            separate_colour_plane: format.separate_colour_plane,
            bit_depth_luma: format.bit_depth_luma,
            bit_depth_chroma: format.bit_depth_chroma,
            sample_aspect_ratio: None,
            video_format: None,
            full_range: None,
            colour_primaries: None,
            transfer_characteristics: None,
            matrix_coefficients: None,
            chroma_location: None,
            frame_mbs_only: dimensions.frame_mbs_only,
            mb_adaptive_frame_field: dimensions.mb_adaptive_frame_field,
            clock: None,
            max_num_reorder_frames: None,
        };
        let sei_timing = if b.flag()? {
            vui(b, &mut metadata, max_ref_frames)?
        } else {
            SeiTiming::default()
        };
        b.trailing()?;
        Ok(Self {
            metadata,
            id: identity.id,
            log2_frame_num: order.log2_frame_num,
            poc_type: order.poc_type,
            log2_poc_lsb: order.log2_poc_lsb,
            delta_poc_always_zero: order.delta_poc_always_zero,
            width_mbs: dimensions.width_mbs,
            height_map_units: dimensions.height_map_units,
            sei_timing,
        })
    }
}
struct Identity {
    profile_idc: u8,
    constraint_flags: u8,
    level_idc: u8,
    id: u32,
}
fn identity(b: &mut Bits<'_>) -> Result<Identity, ConversionError> {
    let profile_idc = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
    if !matches!(
        profile_idc,
        44 | 66
            | 77
            | 83
            | 86
            | 88
            | 100
            | 110
            | 118
            | 122
            | 128
            | 134
            | 135
            | 138
            | 139
            | 144
            | 244
    ) {
        return Err(CONFIG);
    }
    let constraint_flags = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
    if constraint_flags & 3 != 0 {
        return Err(BAD);
    }
    let level_idc = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
    if !matches!(level_idc,9..=13|20..=22|30..=32|40..=42|50..=52|60..=62) {
        return Err(CONFIG);
    }
    Ok(Identity {
        profile_idc,
        constraint_flags,
        level_idc,
        id: b.ue(31)?,
    })
}
#[derive(Clone, Copy)]
struct SampleFormat {
    chroma: u8,
    separate_colour_plane: bool,
    bit_depth_luma: u8,
    bit_depth_chroma: u8,
}
fn sample_format(b: &mut Bits<'_>, profile_idc: u8) -> Result<SampleFormat, ConversionError> {
    let mut format = SampleFormat {
        chroma: 1,
        separate_colour_plane: false,
        bit_depth_luma: 8,
        bit_depth_chroma: 8,
    };
    if !matches!(profile_idc, 66 | 77 | 88) {
        format.chroma = u8::try_from(b.ue(3)?).map_err(|_| BAD)?;
        if format.chroma == 3 {
            format.separate_colour_plane = b.flag()?;
        }
        format.bit_depth_luma += u8::try_from(b.ue(6)?).map_err(|_| BAD)?;
        format.bit_depth_chroma += u8::try_from(b.ue(6)?).map_err(|_| BAD)?;
        let transform_bypass = b.flag()?;
        if transform_bypass && matches!(profile_idc, 100 | 110 | 122) {
            return Err(BAD);
        }
        if b.flag()? {
            for index in 0..if format.chroma == 3 { 12 } else { 8 } {
                if b.flag()? {
                    scaling_list(b, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    let (max_chroma, max_depth) = match profile_idc {
        100 => (1, 8),
        110 => (1, 10),
        122 => (2, 10),
        _ => (3, 14),
    };
    if format.chroma > max_chroma
        || format.bit_depth_luma > max_depth
        || format.bit_depth_chroma > max_depth
    {
        return Err(BAD);
    }
    Ok(format)
}
struct PictureOrder {
    log2_frame_num: u32,
    poc_type: u32,
    log2_poc_lsb: u32,
    delta_poc_always_zero: bool,
}
fn picture_order(b: &mut Bits<'_>) -> Result<PictureOrder, ConversionError> {
    let log2_frame_num = b.ue(12)? + 4;
    let poc_type = b.ue(2)?;
    let mut log2_poc_lsb = 0;
    let mut delta_poc_always_zero = false;
    if poc_type == 0 {
        log2_poc_lsb = b.ue(12)? + 4;
    } else if poc_type == 1 {
        delta_poc_always_zero = b.flag()?;
        b.se(i32::MIN, i32::MAX)?;
        b.se(i32::MIN, i32::MAX)?;
        let count = b.ue(255)?;
        for _ in 0..count {
            b.se(i32::MIN, i32::MAX)?;
        }
    }
    Ok(PictureOrder {
        log2_frame_num,
        poc_type,
        log2_poc_lsb,
        delta_poc_always_zero,
    })
}
struct Dimensions {
    width: u32,
    height: u32,
    coded_width: u32,
    coded_height: u32,
    width_mbs: u32,
    height_map_units: u32,
    frame_mbs_only: bool,
    mb_adaptive_frame_field: bool,
}
fn dimensions(b: &mut Bits<'_>, format: SampleFormat) -> Result<Dimensions, ConversionError> {
    let width_mbs = b.ue(65_535)?.checked_add(1).ok_or(BAD)?;
    let height_map_units = b.ue(65_535)?.checked_add(1).ok_or(BAD)?;
    let frame_mbs_only = b.flag()?;
    let mb_adaptive_frame_field = if frame_mbs_only { false } else { b.flag()? };
    let direct_8x8_inference = b.flag()?;
    if !frame_mbs_only && !direct_8x8_inference {
        return Err(BAD);
    }
    let factor = if frame_mbs_only { 1 } else { 2 };
    let coded_width = width_mbs.checked_mul(16).ok_or(BAD)?;
    let coded_height = height_map_units.checked_mul(16 * factor).ok_or(BAD)?;
    let mut width = coded_width;
    let mut height = coded_height;
    if b.flag()? {
        let left = b.ue(u32::MAX)?;
        let right = b.ue(u32::MAX)?;
        let top = b.ue(u32::MAX)?;
        let bottom = b.ue(u32::MAX)?;
        let chroma_array_type = if format.separate_colour_plane {
            0
        } else {
            format.chroma
        };
        let (unit_x, sub_height) = match chroma_array_type {
            0 | 3 => (1, 1),
            1 => (2, 2),
            2 => (2, 1),
            _ => return Err(BAD),
        };
        let x = left
            .checked_add(right)
            .and_then(|n| n.checked_mul(unit_x))
            .ok_or(BAD)?;
        let y = top
            .checked_add(bottom)
            .and_then(|n| n.checked_mul(sub_height * factor))
            .ok_or(BAD)?;
        width = width.checked_sub(x).filter(|n| *n != 0).ok_or(BAD)?;
        height = height.checked_sub(y).filter(|n| *n != 0).ok_or(BAD)?;
    }
    Ok(Dimensions {
        width,
        height,
        coded_width,
        coded_height,
        width_mbs,
        height_map_units,
        frame_mbs_only,
        mb_adaptive_frame_field,
    })
}
#[derive(Clone, Copy)]
enum Entropy {
    Cavlc,
    Cabac,
}
#[derive(Clone)]
pub(super) struct Prediction {
    pub weighted: bool,
    pub bipred: u32,
}
#[derive(Clone)]
pub(super) struct Pps {
    pub id: u32,
    entropy: Entropy,
    pub bottom_poc_present: bool,
    pub ref_l0: u32,
    pub ref_l1: u32,
    pub prediction: Prediction,
    pub init_qp: i32,
    pub switching_quantizer: i32,
    pub deblocking_control: bool,
    pub redundant_pic_cnt: bool,
}
impl Pps {
    pub fn cabac(&self) -> bool {
        matches!(self.entropy, Entropy::Cabac)
    }
    pub fn parse(data: &[u8], sps: &Sps) -> Result<Self, ConversionError> {
        if data.len() > 65_535 {
            return Err(CONFIG);
        }
        let rbsp = bits::rbsp(data, 8)?;
        let b = &mut Bits::new(&rbsp);
        let id = b.ue(255)?;
        if b.ue(31)? != sps.id {
            return Err(BAD);
        }
        let entropy_coding = b.flag()?;
        if entropy_coding && matches!(sps.metadata.profile_idc, 44 | 66 | 88) {
            return Err(BAD);
        }
        let bottom_poc_present = b.flag()?;
        if b.ue(7)? != 0 {
            return Err(ConversionError("ts-avc-slice-groups-not-supported"));
        }
        let ref_l0 = b.ue(31)? + 1;
        let ref_l1 = b.ue(31)? + 1;
        let weighted_pred = b.flag()?;
        let weighted_bipred = b.bits(2)?;
        if weighted_bipred > 2 {
            return Err(BAD);
        }
        let qp_offset = 6 * i32::from(sps.metadata.bit_depth_luma - 8);
        let init_qp = b.se(-26 - qp_offset, 25)? + 26;
        let switching_quantizer = b.se(-26, 25)? + 26;
        b.se(-12, 12)?;
        let deblocking_control = b.flag()?;
        b.flag()?; // constrained_intra_pred_flag
        let redundant_pic_cnt = b.flag()?;
        if redundant_pic_cnt && matches!(sps.metadata.profile_idc, 77 | 100 | 110 | 122 | 144 | 244)
        {
            return Err(BAD);
        }
        if b.more_rbsp_data() {
            if matches!(sps.metadata.profile_idc, 66 | 77 | 88) {
                return Err(BAD);
            }
            let transform_8x8 = b.flag()?;
            if b.flag()? {
                let count = 6 + if transform_8x8 {
                    if sps.metadata.chroma_format_idc == 3 {
                        6
                    } else {
                        2
                    }
                } else {
                    0
                };
                for index in 0..count {
                    if b.flag()? {
                        scaling_list(b, if index < 6 { 16 } else { 64 })?;
                    }
                }
            }
            b.se(-12, 12)?;
        }
        b.trailing()?;
        Ok(Self {
            id,
            entropy: if entropy_coding {
                Entropy::Cabac
            } else {
                Entropy::Cavlc
            },
            bottom_poc_present,
            ref_l0,
            ref_l1,
            prediction: Prediction {
                weighted: weighted_pred,
                bipred: weighted_bipred,
            },
            init_qp,
            switching_quantizer,
            deblocking_control,
            redundant_pic_cnt,
        })
    }
}

fn scaling_list(b: &mut Bits<'_>, count: usize) -> Result<(), ConversionError> {
    let mut previous = 8_i32;
    let mut next = 8_i32;
    for _ in 0..count {
        if next != 0 {
            next = (previous + b.se(-128, 127)? + 256) % 256;
        }
        if next != 0 {
            previous = next;
        }
    }
    Ok(())
}

fn hrd(b: &mut Bits<'_>) -> Result<Option<Hrd>, ConversionError> {
    if !b.flag()? {
        return Ok(None);
    }
    let cpb_count = b.ue(31)? + 1;
    b.bits(8)?;
    for _ in 0..cpb_count {
        b.ue(u32::MAX - 1)?;
        b.ue(u32::MAX - 1)?;
        b.flag()?;
    }
    Ok(Some(Hrd {
        cpb_count,
        initial_delay_length: b.bits(5)? + 1,
        cpb_delay_length: b.bits(5)? + 1,
        dpb_delay_length: b.bits(5)? + 1,
        time_offset_length: b.bits(5)?,
    }))
}

fn vui(
    b: &mut Bits<'_>,
    m: &mut Metadata,
    max_ref_frames: u32,
) -> Result<SeiTiming, ConversionError> {
    m.sample_aspect_ratio = aspect_ratio(b)?;
    if b.flag()? {
        b.flag()?;
    }
    if b.flag()? {
        let format = u8::try_from(b.bits(3)?).map_err(|_| BAD)?;
        if format > 5 {
            return Err(BAD);
        }
        m.video_format = Some(format);
        m.full_range = Some(b.flag()?);
        if b.flag()? {
            let primaries = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
            let transfer = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
            let matrix = u8::try_from(b.bits(8)?).map_err(|_| BAD)?;
            if !matches!(primaries, 1 | 2 | 4..=12 | 22)
                || !matches!(transfer, 1 | 2 | 4..=18)
                || !matches!(matrix, 0..=2 | 4..=14)
            {
                return Err(BAD);
            }
            m.colour_primaries = Some(primaries);
            m.transfer_characteristics = Some(transfer);
            m.matrix_coefficients = Some(matrix);
        }
    }
    if b.flag()? {
        m.chroma_location = Some((
            u8::try_from(b.ue(5)?).map_err(|_| BAD)?,
            u8::try_from(b.ue(5)?).map_err(|_| BAD)?,
        ));
    }
    if b.flag()? {
        let num_units_in_tick = b.bits(32)?;
        let time_scale = b.bits(32)?;
        if num_units_in_tick == 0 || time_scale == 0 {
            return Err(BAD);
        }
        m.clock = Some(VideoClock {
            num_units_in_tick,
            time_scale,
            fixed_frame_rate: b.flag()?,
            pic_struct_present: false,
        });
    }
    let nal = hrd(b)?;
    let vcl = hrd(b)?;
    if let (Some(a), Some(z)) = (nal, vcl)
        && (a.cpb_delay_length != z.cpb_delay_length
            || a.dpb_delay_length != z.dpb_delay_length
            || a.time_offset_length != z.time_offset_length)
    {
        return Err(BAD);
    }
    if nal.is_some() || vcl.is_some() {
        b.flag()?;
    }
    let pic_struct_present = b.flag()?;
    if let Some(clock) = &mut m.clock {
        clock.pic_struct_present = pic_struct_present;
    }
    if b.flag()? {
        b.flag()?;
        b.ue(16)?;
        b.ue(16)?;
        b.ue(16)?;
        b.ue(16)?;
        let reorder = b.ue(16)?;
        let buffer = b.ue(16)?;
        if reorder > buffer || buffer < max_ref_frames {
            return Err(BAD);
        }
        m.max_num_reorder_frames = Some(reorder);
    }
    Ok(SeiTiming {
        nal_hrd: nal,
        vcl_hrd: vcl,
        pic_struct_present,
    })
}

fn aspect_ratio(b: &mut Bits<'_>) -> Result<Option<(u16, u16)>, ConversionError> {
    const SAR: [(u16, u16); 17] = [
        (0, 0),
        (1, 1),
        (12, 11),
        (10, 11),
        (16, 11),
        (40, 33),
        (24, 11),
        (20, 11),
        (32, 11),
        (80, 33),
        (18, 11),
        (15, 11),
        (64, 33),
        (160, 99),
        (4, 3),
        (3, 2),
        (2, 1),
    ];
    if !b.flag()? {
        return Ok(None);
    }
    match b.bits(8)? {
        0 => Ok(None),
        index @ 1..=16 => Ok(Some(SAR[usize::try_from(index).map_err(|_| BAD)?])),
        255 => {
            let width = u16::try_from(b.bits(16)?).map_err(|_| BAD)?;
            let height = u16::try_from(b.bits(16)?).map_err(|_| BAD)?;
            if width == 0 || height == 0 {
                return Err(BAD);
            }
            Ok(Some((width, height)))
        }
        _ => Err(BAD),
    }
}
