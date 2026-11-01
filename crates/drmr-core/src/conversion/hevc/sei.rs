//! Validate supported SEI payloads while keeping every payload byte in the digest.
use super::{
    BAD, ConversionError,
    bits::{self, Bits},
    parameters::Sps,
};
#[derive(Default)]
pub(super) struct Evidence {
    pub picture_timing: bool,
    pub recovery: Option<(i32, bool, bool)>,
}
fn finish(b: &mut Bits<'_>) -> Result<(), ConversionError> {
    if b.remaining() == 0 {
        Ok(())
    } else {
        b.trailing()
    }
}
fn number(bytes: &[u8], at: &mut usize) -> Result<usize, ConversionError> {
    let mut value = 0_usize;
    loop {
        let byte = *bytes.get(*at).ok_or(BAD)?;
        *at += 1;
        value = value.checked_add(usize::from(byte)).ok_or(BAD)?;
        if byte != 255 {
            return Ok(value);
        }
    }
}
pub(super) fn parse(nal: &[u8], sps: &Sps, vps_id: u8) -> Result<Evidence, ConversionError> {
    let kind = bits::header(nal)?.kind;
    if !matches!(kind, 39 | 40) {
        return Err(BAD);
    }
    let rbsp = bits::rbsp(nal)?;
    let mut at = 0;
    let mut count = 0;
    let mut result = Evidence::default();
    while at < rbsp.len() {
        if rbsp[at..] == [0x80] {
            return if count != 0 { Ok(result) } else { Err(BAD) };
        }
        let payload_type = number(&rbsp, &mut at)?;
        let size = number(&rbsp, &mut at)?;
        let end = at.checked_add(size).ok_or(BAD)?;
        let payload = rbsp.get(at..end).ok_or(BAD)?;
        at = end;
        count += 1;
        if count > 4096 {
            return Err(BAD);
        }
        if kind == 40 && !matches!(payload_type, 3 | 4 | 5 | 132) {
            return Err(ConversionError("native-hevc-suffix-sei-not-supported"));
        }
        parse_payload(payload_type, payload, kind, sps, vps_id, &mut result)?;
    }
    Err(BAD)
}

fn parse_payload(
    payload_type: usize,
    payload: &[u8],
    kind: u8,
    sps: &Sps,
    vps_id: u8,
    result: &mut Evidence,
) -> Result<(), ConversionError> {
    let b = &mut Bits::new(payload);
    match payload_type {
        0 => buffering_period(b, sps)?,
        1 => picture_timing(b, sps, result)?,
        3 => {
            if payload.is_empty() || payload.iter().any(|byte| *byte != 255) {
                return Err(BAD);
            }
        }
        4 => {
            if payload.len() < 2 || (payload[0] == 255 && payload.len() < 3) {
                return Err(BAD);
            }
        }
        5 => {
            if payload.len() < 16 {
                return Err(BAD);
            }
        }
        6 => {
            let recovery = (b.se(i32::MIN, i32::MAX)?, b.flag()?, b.flag()?);
            finish(b)?;
            if result.recovery.replace(recovery).is_some() {
                return Err(BAD);
            }
        }
        129 => {
            if b.u(4)? != u32::from(vps_id) {
                return Err(BAD);
            }
            b.flag()?;
            b.flag()?;
            if b.ue(15)? != 0 || b.ue(15)? != sps.id {
                return Err(ConversionError(
                    "native-hevc-parameter-set-change-not-supported",
                ));
            }
            finish(b)?;
        }
        132 => {
            let expected = match payload.first() {
                Some(0) => 49,
                Some(1) => 7,
                Some(2) => 13,
                _ => return Err(BAD),
            };
            if kind != 40 || payload.len() != expected {
                return Err(BAD);
            }
        }
        137 => {
            if payload.len() != 24 {
                return Err(BAD);
            }
        }
        144 => {
            if payload.len() != 4 {
                return Err(BAD);
            }
        }
        147 => {
            if payload.len() != 1 {
                return Err(BAD);
            }
        }
        _ => return Err(ConversionError("native-hevc-sei-payload-not-supported")),
    }
    Ok(())
}

fn buffering_period(b: &mut Bits<'_>, sps: &Sps) -> Result<(), ConversionError> {
    if b.ue(15)? != sps.id {
        return Err(BAD);
    }
    let h = sps.timing.hrd.filter(|h| h.nal || h.vcl).ok_or(BAD)?;
    let irap = !h.sub_pic && b.flag()?;
    if irap {
        b.u(h.removal)?;
        b.u(h.output)?;
    }
    b.flag()?;
    b.u(h.removal)?;
    for present in [h.nal, h.vcl] {
        if present {
            for _ in 0..h.cpb_count {
                b.u(h.initial)?;
                b.u(h.initial)?;
                if h.sub_pic || irap {
                    b.u(h.initial)?;
                    b.u(h.initial)?;
                }
            }
        }
    }
    finish(b)?;

    Ok(())
}

fn picture_timing(
    b: &mut Bits<'_>,
    sps: &Sps,
    result: &mut Evidence,
) -> Result<(), ConversionError> {
    if result.picture_timing {
        return Err(BAD);
    }
    result.picture_timing = true;
    if sps.timing.frame_field_info {
        let structure = b.u(4)?;
        let scan = b.u(2)?;
        let duplicate = b.flag()?;
        if structure != 0 || scan == 0 || scan == 3 || duplicate {
            return Err(ConversionError(
                "native-hevc-picture-duration-not-supported",
            ));
        }
    }
    if let Some(h) = sps.timing.hrd.filter(|h| h.nal || h.vcl) {
        b.u(h.removal)?;
        b.u(h.output)?;
        if h.sub_pic {
            b.u(h.du_output)?;
        }
        if h.sub_pic && h.sub_pic_in_timing {
            let units = b.ue(4095)?;
            let common = b.flag()?;
            if common {
                b.u(h.du_increment)?;
            }
            for index in 0..=units {
                b.ue(4095)?;
                if !common && index < units {
                    b.u(h.du_increment)?;
                }
            }
        }
    }
    finish(b)?;

    Ok(())
}
