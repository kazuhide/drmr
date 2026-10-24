//! Bounded SEI envelopes and picture timing.
use super::{
    BAD, ConversionError, PROGRESSIVE, RecoveryPoint,
    bits::{self, Bits},
    parameters::Sps,
};

pub(super) struct Evidence {
    pub picture_structure: Option<u8>,
    pub recovery_point: Option<RecoveryPoint>,
}
pub(super) fn parse(nal: &[u8], sps: &Sps) -> Result<Evidence, ConversionError> {
    let rbsp = bits::rbsp(nal, 6)?;
    let (&trailing, data) = rbsp.split_last().ok_or(BAD)?;
    if trailing != 0x80 {
        return Err(BAD);
    }
    let mut at = 0;
    let mut count = 0;
    let mut structure = None;
    let mut recovery_point = None;
    while at < data.len() {
        let kind = extended(data, &mut at)?;
        let size = usize::try_from(extended(data, &mut at)?).map_err(|_| BAD)?;
        let end = at.checked_add(size).ok_or(BAD)?;
        let payload = data.get(at..end).ok_or(BAD)?;
        match kind {
            0 => buffering_period(payload, sps)?,
            1 => {
                let picture = picture_timing(payload, sps)?;
                if let Some(picture) = picture
                    && structure.replace(picture).is_some()
                {
                    return Err(BAD);
                }
            }
            5 if payload.len() < 16 => return Err(BAD),
            6 => {
                let b = &mut Bits::new(payload);
                let frame_count = b.ue(u32::MAX)?;
                let exact_match = b.flag()?;
                let broken_link = b.flag()?;
                let changing_slice_group_idc = u8::try_from(b.bits(2)?).map_err(|_| BAD)?;
                if changing_slice_group_idc > 2 {
                    return Err(BAD);
                }
                payload_end(b)?;
                if recovery_point
                    .replace(RecoveryPoint {
                        frame_count,
                        exact_match,
                        broken_link,
                        changing_slice_group_idc,
                    })
                    .is_some()
                {
                    return Err(BAD);
                }
            }
            137 if payload.len() != 24 => return Err(BAD),
            144 if payload.len() != 4 => return Err(BAD),
            147 if payload.len() != 1 => return Err(BAD),
            _ => {}
        }
        at = end;
        count += 1;
        if count > 4096 {
            return Err(BAD);
        }
    }
    Ok(Evidence {
        picture_structure: structure,
        recovery_point,
    })
}
fn extended(data: &[u8], at: &mut usize) -> Result<u32, ConversionError> {
    let mut value = 0_u32;
    loop {
        let byte = *data.get(*at).ok_or(BAD)?;
        *at = at.checked_add(1).ok_or(BAD)?;
        value = value.checked_add(u32::from(byte)).ok_or(BAD)?;
        if byte != 255 {
            return Ok(value);
        }
    }
}
fn buffering_period(payload: &[u8], sps: &Sps) -> Result<(), ConversionError> {
    let b = &mut Bits::new(payload);
    if b.ue(31)? != sps.id {
        return Err(BAD);
    }
    for hrd in [sps.sei_timing.nal_hrd, sps.sei_timing.vcl_hrd]
        .into_iter()
        .flatten()
    {
        for _ in 0..hrd.cpb_count {
            b.bits(hrd.initial_delay_length)?;
            b.bits(hrd.initial_delay_length)?;
        }
    }
    payload_end(b)
}
fn picture_timing(payload: &[u8], sps: &Sps) -> Result<Option<u8>, ConversionError> {
    const CLOCKS: [u32; 9] = [1, 1, 1, 2, 2, 3, 3, 2, 3];
    let b = &mut Bits::new(payload);
    let hrd = sps.sei_timing.nal_hrd.or(sps.sei_timing.vcl_hrd);
    if let Some(hrd) = hrd {
        b.bits(hrd.cpb_delay_length)?;
        b.bits(hrd.dpb_delay_length)?;
    }
    let mut structure = None;
    if sps.sei_timing.pic_struct_present {
        let picture = b.bits(4)?;
        let count = *CLOCKS.get(picture as usize).ok_or(BAD)?;
        structure = Some(u8::try_from(picture).map_err(|_| BAD)?);
        for _ in 0..count {
            if !b.flag()? {
                continue;
            }
            let ct_type = b.bits(2)?;
            if ct_type > 2 {
                return Err(BAD);
            }
            if ct_type == 1 {
                return Err(PROGRESSIVE);
            }
            b.flag()?; // nuit_field_based_flag
            if b.bits(5)? > 6 {
                return Err(BAD);
            }
            let full = b.flag()?;
            b.flag()?; // discontinuity_flag
            b.flag()?; // cnt_dropped_flag
            b.bits(8)?;
            if full {
                timestamp(b, true, true, true)?;
            } else if b.flag()? {
                timestamp(b, true, false, false)?;
                if b.flag()? {
                    timestamp(b, false, true, false)?;
                    if b.flag()? {
                        timestamp(b, false, false, true)?;
                    }
                }
            }
            b.bits(hrd.map_or(24, |h| h.time_offset_length))?;
        }
    }
    payload_end(b)?;
    Ok(structure)
}
fn timestamp(
    b: &mut Bits<'_>,
    seconds: bool,
    minutes: bool,
    hours: bool,
) -> Result<(), ConversionError> {
    if seconds && b.bits(6)? > 59 {
        return Err(BAD);
    }
    if minutes && b.bits(6)? > 59 {
        return Err(BAD);
    }
    if hours && b.bits(5)? > 23 {
        return Err(BAD);
    }
    Ok(())
}
fn payload_end(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    if b.remaining() == 0 {
        Ok(())
    } else {
        b.trailing()
    }
}
