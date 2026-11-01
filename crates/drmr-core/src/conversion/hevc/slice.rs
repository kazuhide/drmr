//! Complete slice-segment headers; encoded coding-tree data stays undecoded.
use super::{
    BAD, ConversionError,
    bits::{self, Bits},
    parameters::Sps,
    pps::Pps,
    rps::{self, Rps},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Picture {
    pub nal: bits::Header,
    pub pps: u32,
    pub no_prior: bool,
    pub poc: Option<u32>,
    pub output: bool,
    pub short_term: Rps,
    pub long_term: Vec<(u32, bool, bool, u32)>,
}
pub(super) struct Header {
    pub first: bool,
    pub dependent: bool,
    pub address: u32,
    pub picture: Option<Picture>,
    pub nal: bits::Header,
    pub no_prior: bool,
    pub clear_bytes: usize,
    pub slice_type: Option<u32>,
    pub entry_points: u32,
}
pub(super) fn parse(nal: &[u8], sps: &Sps, pps: &Pps) -> Result<Header, ConversionError> {
    let head = bits::header(nal)?;
    if head.kind > 31 || head.temporal > sps.max_sub {
        return Err(BAD);
    }
    let rbsp = bits::rbsp(nal)?;
    let b = &mut Bits::new(&rbsp);
    let first = b.flag()?;
    let no_prior = if (16..=21).contains(&head.kind) {
        b.flag()?
    } else {
        false
    };
    if b.ue(63)? != pps.id {
        return Err(BAD);
    }
    let dependent = !first && pps.dependent && b.flag()?;
    let address = if first {
        0
    } else {
        b.u(bits::log2_ceil(sps.ctbs()?))?
    };
    if address >= sps.ctbs()? || (!first && address == 0) {
        return Err(BAD);
    }
    let mut picture = None;
    let mut slice_type = None;
    if !dependent {
        for _ in 0..pps.extra_bits {
            if b.flag()? {
                return Err(BAD);
            }
        }
        let kind = b.ue(2)?;
        slice_type = Some(kind);
        if (16..=21).contains(&head.kind) && kind != 2 {
            return Err(BAD);
        }
        let output = if pps.output_flag { b.flag()? } else { true };
        let mut current = Picture {
            nal: head,
            pps: pps.id,
            no_prior,
            poc: None,
            output,
            short_term: Rps {
                pictures: Vec::new(),
            },
            long_term: Vec::new(),
        };
        let mut temporal_mvp = false;
        if !matches!(head.kind, 19 | 20) {
            current.poc = Some(b.u(sps.poc_bits)?);
            current.short_term = if !b.flag()? {
                rps::parse(b, &sps.rps, true, sps.max_buffer)?
            } else {
                if sps.rps.is_empty() {
                    return Err(BAD);
                }
                let index = if sps.rps.len() > 1 {
                    b.u(bits::log2_ceil(sps.rps.len() as u32))? as usize
                } else {
                    0
                };
                sps.rps.get(index).ok_or(BAD)?.clone()
            };
            if sps.long_term_present {
                let from_sps = if sps.long_term.is_empty() {
                    0
                } else {
                    b.ue(sps.long_term.len() as u32)?
                };
                let explicit = b.ue(sps.max_buffer)?;
                if from_sps + explicit + current.short_term.pictures.len() as u32 > sps.max_buffer {
                    return Err(BAD);
                }
                for index in 0..from_sps + explicit {
                    let (poc, used) = if index < from_sps {
                        let reference = if sps.long_term.len() > 1 {
                            b.u(bits::log2_ceil(sps.long_term.len() as u32))? as usize
                        } else {
                            0
                        };
                        *sps.long_term.get(reference).ok_or(BAD)?
                    } else {
                        (b.u(sps.poc_bits)?, b.flag()?)
                    };
                    let msb = b.flag()?;
                    let cycle = if msb { b.ue(u32::MAX - 1)? } else { 0 };
                    current.long_term.push((poc, used, msb, cycle));
                }
            }
            if sps.temporal_mvp {
                temporal_mvp = b.flag()?;
            }
        }
        let mut sao_luma = false;
        let mut sao_chroma = false;
        if sps.sao {
            sao_luma = b.flag()?;
            sao_chroma = b.flag()?;
        }
        let total = current.short_term.used()
            + current
                .long_term
                .iter()
                .filter(|(_, used, _, _)| *used)
                .count() as u32;
        if kind != 2 {
            if total == 0 {
                return Err(BAD);
            }
            let (mut l0, mut l1) = (pps.l0, pps.l1);
            if b.flag()? {
                l0 = b.ue(14)? + 1;
                if kind == 0 {
                    l1 = b.ue(14)? + 1;
                }
            }
            if pps.list_modification && total > 1 {
                list(b, l0, total)?;
                if kind == 0 {
                    list(b, l1, total)?;
                }
            }
            if kind == 0 {
                b.flag()?;
            }
            if pps.cabac_init {
                b.flag()?;
            }
            if temporal_mvp {
                let from_l0 = kind != 0 || b.flag()?;
                let count = if from_l0 { l0 } else { l1 };
                if count > 1 {
                    b.ue(count - 1)?;
                }
            }
            if (kind == 1 && pps.weighted_pred) || (kind == 0 && pps.weighted_bipred) {
                weights(b, l0, if kind == 0 { l1 } else { 0 })?;
            }
            b.ue(4)?;
        }
        let qp = pps.init_qp.checked_add(b.se(-87, 87)?).ok_or(BAD)?;
        if !(-6 * i32::from(sps.metadata.bit_depth_luma - 8)..=51).contains(&qp) {
            return Err(BAD);
        }
        if pps.chroma_offsets {
            if !(-12..=12).contains(&(pps.cb_offset + b.se(-12, 12)?))
                || !(-12..=12).contains(&(pps.cr_offset + b.se(-12, 12)?))
            {
                return Err(BAD);
            }
        }
        let mut disabled = pps.deblock_disabled;
        if pps.deblock_override && b.flag()? {
            disabled = b.flag()?;
            if !disabled {
                b.se(-6, 6)?;
                b.se(-6, 6)?;
            }
        }
        if pps.loop_across_slices && (sao_luma || sao_chroma || !disabled) {
            b.flag()?;
        }
        picture = Some(current);
    }
    let mut offsets = Vec::new();
    let mut entry_points = 0;
    if pps.tiles || pps.wpp {
        let maximum = if pps.wpp {
            sps.ctb_height
                .checked_mul(pps.columns.len() as u32)
                .ok_or(BAD)?
        } else {
            (pps.rows.len() * pps.columns.len()) as u32
        };
        let count = b.ue(maximum.saturating_sub(1))?;
        entry_points = count;
        if count != 0 {
            let width = b.ue(31)? + 1;
            for _ in 0..count {
                offsets.push(u64::from(b.u(width)?) + 1);
            }
        }
    }
    if pps.header_extension {
        let length = b.ue(256)?;
        if length != 0 {
            return Err(ConversionError(
                "native-hevc-slice-header-extension-not-supported",
            ));
        }
    }
    b.align()?;
    if b.remaining() == 0 {
        return Err(BAD);
    }
    let clear_bytes = bits::original_offset(nal, b.position() / 8)?;
    let coded = nal.len().checked_sub(clear_bytes).ok_or(BAD)? as u64;
    let sum = offsets
        .iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(*value))
        .ok_or(BAD)?;
    if sum >= coded && !offsets.is_empty() {
        return Err(BAD);
    }
    Ok(Header {
        first,
        dependent,
        address,
        picture,
        nal: head,
        no_prior,
        clear_bytes,
        slice_type,
        entry_points,
    })
}
fn list(b: &mut Bits<'_>, count: u32, total: u32) -> Result<(), ConversionError> {
    if b.flag()? {
        for _ in 0..count {
            if b.u(bits::log2_ceil(total))? >= total {
                return Err(BAD);
            }
        }
    }
    Ok(())
}
fn weights(b: &mut Bits<'_>, l0: u32, l1: u32) -> Result<(), ConversionError> {
    let luma = b.ue(7)? as i32;
    let chroma = luma + b.se(-7, 7)?;
    if !(0..=7).contains(&chroma) {
        return Err(BAD);
    }
    let mut total = 0;
    for count in [l0, l1] {
        if count == 0 {
            continue;
        }
        let mut luma_flags = Vec::new();
        let mut chroma_flags = Vec::new();
        for _ in 0..count {
            let flag = b.flag()?;
            total += u32::from(flag);
            luma_flags.push(flag);
        }
        for _ in 0..count {
            let flag = b.flag()?;
            total += 2 * u32::from(flag);
            chroma_flags.push(flag);
        }
        if total > 24 {
            return Err(BAD);
        }
        for (luma, chroma) in luma_flags.into_iter().zip(chroma_flags) {
            if luma {
                b.se(-128, 127)?;
                b.se(-128, 127)?;
            }
            if chroma {
                for _ in 0..2 {
                    b.se(-128, 127)?;
                    b.se(-512, 511)?;
                }
            }
        }
    }
    Ok(())
}
