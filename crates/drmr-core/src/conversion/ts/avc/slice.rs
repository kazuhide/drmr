//! Full AVC slice-header syntax; entropy-coded macroblocks remain undecoded.
use super::{
    BAD, ConversionError, PROGRESSIVE,
    bits::{self, Bits},
    parameters::{Pps, Sps},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Picture {
    frame_num: u32,
    pps_id: u32,
    idr_pic_id: Option<u32>,
    nal_reference: bool,
    poc_lsb: Option<u32>,
    delta_poc: (i32, i32),
}
impl Picture {
    pub(super) fn is_idr(&self) -> bool {
        self.idr_pic_id.is_some()
    }
}
pub(super) struct Header {
    pub first_mb: u32,
    pub picture: Picture,
    pub slice_type: u32,
    pub all_slices_same_type: bool,
}
pub(super) fn parse(nal: &[u8], sps: &Sps, pps: &Pps) -> Result<Header, ConversionError> {
    let kind = nal.first().map(|n| n & 31).ok_or(BAD)?;
    if !matches!(kind, 1 | 5) {
        return Err(BAD);
    }
    let rbsp = bits::rbsp(nal, kind)?;
    let b = &mut Bits::new(&rbsp);
    let header = picture_header(b, nal, sps, pps)?;
    let slice_type = header.slice_type;
    if slice_type == 1 {
        b.flag()?;
    }
    let mut l0 = pps.ref_l0;
    let mut l1 = pps.ref_l1;
    if matches!(slice_type, 0 | 1 | 3) && b.flag()? {
        l0 = b.ue(15)? + 1;
        if slice_type == 1 {
            l1 = b.ue(15)? + 1;
        }
    }
    if l0 > 16 || l1 > 16 {
        return Err(BAD);
    }
    if !matches!(slice_type, 2 | 4) {
        ref_modifications(b)?;
    }
    if slice_type == 1 {
        ref_modifications(b)?;
    }
    if (pps.prediction.weighted && matches!(slice_type, 0 | 3))
        || (pps.prediction.bipred == 1 && slice_type == 1)
    {
        b.ue(7)?;
        let chroma = sps.metadata.chroma_format_idc != 0;
        if chroma {
            b.ue(7)?;
        }
        weights(b, l0, chroma)?;
        if slice_type == 1 {
            weights(b, l1, chroma)?;
        }
    }
    let nal_reference = nal[0] & 0x60 != 0;
    if nal_reference {
        if kind == 5 {
            b.flag()?;
            b.flag()?;
        } else if b.flag()? {
            reference_marking(b)?;
        }
    }
    if pps.cabac() && !matches!(slice_type, 2 | 4) {
        b.ue(2)?;
    }
    let qp = pps.init_qp.checked_add(b.se(-87, 87)?).ok_or(BAD)?;
    let lower_qp = -6 * i32::from(sps.metadata.bit_depth_luma - 8);
    if !(lower_qp..=51).contains(&qp) {
        return Err(BAD);
    }
    if matches!(slice_type, 3 | 4) {
        if slice_type == 3 {
            b.flag()?;
        }
        let qs = pps
            .switching_quantizer
            .checked_add(b.se(-51, 51)?)
            .ok_or(BAD)?;
        if !(0..=51).contains(&qs) {
            return Err(BAD);
        }
    }
    if pps.deblocking_control && b.ue(2)? != 1 {
        b.se(-6, 6)?;
        b.se(-6, 6)?;
    }
    if pps.cabac() {
        b.byte_align_one()?;
    }
    // At least a coded slice and its RBSP termination must remain. Decoding
    // the entropy-coded syntax is deliberately reserved for optional decode.
    if b.remaining() == 0 {
        return Err(BAD);
    }
    let zero_bytes = rbsp.iter().rev().take_while(|n| **n == 0).count();
    if zero_bytes != 0 && (!pps.cabac() || zero_bytes % 2 != 0) {
        return Err(BAD);
    }
    Ok(header)
}

fn picture_header(
    b: &mut Bits<'_>,
    nal: &[u8],
    sps: &Sps,
    pps: &Pps,
) -> Result<Header, ConversionError> {
    let kind = nal[0] & 31;
    let first_mb = b.ue(u32::MAX)?;
    let declared_slice_type = b.ue(9)?;
    let slice_type = declared_slice_type % 5;
    if (sps.metadata.profile_idc == 66 && !matches!(slice_type, 0 | 2))
        || (matches!(sps.metadata.profile_idc, 77 | 100 | 110 | 122 | 144 | 244)
            && matches!(slice_type, 3 | 4))
        || (sps.metadata.profile_idc == 44 && slice_type != 2)
    {
        return Err(BAD);
    }
    let pps_id = b.ue(255)?;
    if pps_id != pps.id {
        return Err(BAD);
    }
    if sps.metadata.separate_colour_plane {
        b.bits(2)?;
        return Err(ConversionError(
            "ts-avc-separate-colour-plane-not-supported",
        ));
    }
    let frame_num = b.bits(sps.log2_frame_num)?;
    let field_picture = if sps.metadata.frame_mbs_only {
        false
    } else {
        b.flag()?
    };
    if field_picture {
        b.flag()?; // bottom_field_flag
        return Err(PROGRESSIVE);
    }
    let pic_mbs = sps
        .width_mbs
        .checked_mul(sps.height_map_units)
        .and_then(|n| n.checked_mul(if sps.metadata.frame_mbs_only { 1 } else { 2 }))
        .ok_or(BAD)?;
    if first_mb >= pic_mbs {
        return Err(BAD);
    }
    let idr_pic_id = if kind == 5 {
        if frame_num != 0 || !matches!(slice_type, 2 | 4) {
            return Err(BAD);
        }
        Some(b.ue(65_535)?)
    } else {
        None
    };
    let mut poc_lsb = None;
    let mut delta_poc = (0, 0);
    if sps.poc_type == 0 {
        poc_lsb = Some(b.bits(sps.log2_poc_lsb)?);
        if pps.bottom_poc_present {
            delta_poc.1 = b.se(i32::MIN, i32::MAX)?;
        }
    } else if sps.poc_type == 1 && !sps.delta_poc_always_zero {
        delta_poc.0 = b.se(i32::MIN, i32::MAX)?;
        if pps.bottom_poc_present {
            delta_poc.1 = b.se(i32::MIN, i32::MAX)?;
        }
    }
    if pps.redundant_pic_cnt {
        b.ue(127)?;
    }
    let nal_reference = nal[0] & 0x60 != 0;
    Ok(Header {
        first_mb,
        slice_type,
        all_slices_same_type: declared_slice_type >= 5,
        picture: Picture {
            frame_num,
            pps_id,
            idr_pic_id,
            nal_reference,
            poc_lsb,
            delta_poc,
        },
    })
}

fn ref_modifications(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    if !b.flag()? {
        return Ok(());
    }
    for _ in 0..33 {
        match b.ue(3)? {
            0..=2 => {
                b.ue(u32::MAX - 1)?;
            }
            3 => return Ok(()),
            _ => return Err(BAD),
        }
    }
    Err(BAD)
}
fn weights(b: &mut Bits<'_>, refs: u32, chroma: bool) -> Result<(), ConversionError> {
    for _ in 0..refs {
        if b.flag()? {
            b.se(-128, 127)?;
            b.se(-128, 127)?;
        }
        if chroma && b.flag()? {
            for _ in 0..2 {
                b.se(-128, 127)?;
                b.se(-128, 127)?;
            }
        }
    }
    Ok(())
}
fn reference_marking(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    for _ in 0..32 {
        let operation = b.ue(6)?;
        if operation == 0 {
            return Ok(());
        }
        if matches!(operation, 1 | 3) {
            b.ue(u32::MAX - 1)?;
        }
        if operation == 2 {
            b.ue(u32::MAX)?;
        }
        if matches!(operation, 3 | 6) {
            b.ue(u32::MAX)?;
        }
        if operation == 4 {
            b.ue(u32::MAX)?;
        }
    }
    Err(BAD)
}
