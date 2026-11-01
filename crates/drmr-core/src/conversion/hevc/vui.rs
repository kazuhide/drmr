//! VUI/HRD syntax is bounded and preserves explicitly signaled colour identifiers.
use super::{BAD, ConversionError, Metadata, bits::Bits};
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Hrd {
    pub nal: bool,
    pub vcl: bool,
    pub sub_pic: bool,
    pub sub_pic_in_timing: bool,
    pub initial: u32,
    pub removal: u32,
    pub output: u32,
    pub du_increment: u32,
    pub du_output: u32,
    pub cpb_count: u32,
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Timing {
    pub frame_field_info: bool,
    pub hrd: Option<Hrd>,
}
pub(super) fn hrd(
    b: &mut Bits<'_>,
    max_sub: u8,
    common: Option<Hrd>,
) -> Result<Hrd, ConversionError> {
    let mut value = if let Some(common) = common {
        common
    } else {
        let mut value = Hrd {
            nal: b.flag()?,
            vcl: b.flag()?,
            ..Hrd::default()
        };
        if value.nal || value.vcl {
            value.sub_pic = b.flag()?;
            if value.sub_pic {
                b.u(8)?;
                value.du_increment = b.u(5)? + 1;
                value.sub_pic_in_timing = b.flag()?;
                value.du_output = b.u(5)? + 1;
            }
            b.u(4)?;
            b.u(4)?;
            if value.sub_pic {
                b.u(4)?;
            }
            value.initial = b.u(5)? + 1;
            value.removal = b.u(5)? + 1;
            value.output = b.u(5)? + 1;
        }
        value
    };
    for _ in 0..=max_sub {
        let general = b.flag()?;
        let fixed = general || b.flag()?;
        let low_delay = if fixed {
            b.ue(2047)?;
            false
        } else {
            b.flag()?
        };
        let cpb = if low_delay { 0 } else { b.ue(31)? };
        value.cpb_count = cpb + 1;
        for present in [value.nal, value.vcl] {
            if present {
                for _ in 0..=cpb {
                    b.ue(u32::MAX - 1)?;
                    b.ue(u32::MAX - 1)?;
                    if value.sub_pic {
                        b.ue(u32::MAX - 1)?;
                        b.ue(u32::MAX - 1)?;
                    }
                    b.flag()?;
                }
            }
        }
    }
    Ok(value)
}
pub(super) fn parse(
    b: &mut Bits<'_>,
    max_sub: u8,
    metadata: &mut Metadata,
) -> Result<(Timing, Option<u16>), ConversionError> {
    if b.flag()? {
        let index = b.u(8)?;
        metadata.sample_aspect_ratio = match index {
            0 => None,
            1..=16 => Some(
                [
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
                ][index as usize - 1],
            ),
            255 => {
                let width = b.u(16)? as u16;
                let height = b.u(16)? as u16;
                if width == 0 || height == 0 {
                    return Err(BAD);
                }
                Some((width, height))
            }
            _ => return Err(BAD),
        };
    }
    if b.flag()? {
        b.flag()?;
    }
    if b.flag()? {
        if b.u(3)? > 5 {
            return Err(BAD);
        }
        metadata.full_range = Some(b.flag()?);
        if b.flag()? {
            metadata.colour_primaries = Some(b.u(8)? as u8);
            metadata.transfer_characteristics = Some(b.u(8)? as u8);
            metadata.matrix_coefficients = Some(b.u(8)? as u8);
        }
    }
    if b.flag()? {
        metadata.chroma_location = Some((b.ue(5)? as u8, b.ue(5)? as u8));
    }
    b.flag()?;
    if b.flag()? {
        return Err(ConversionError("native-hevc-field-pictures-not-supported"));
    }
    let frame_field_info = b.flag()?;
    if b.flag()? {
        let mut nonzero = false;
        for maximum in [
            metadata.coded_width,
            metadata.coded_width,
            metadata.coded_height,
            metadata.coded_height,
        ] {
            nonzero |= b.ue(maximum)? != 0;
        }
        if nonzero {
            return Err(ConversionError(
                "native-hevc-default-display-window-not-supported",
            ));
        }
    }
    let mut timing = Timing {
        frame_field_info,
        ..Timing::default()
    };
    if b.flag()? {
        if b.u(32)? == 0 || b.u(32)? == 0 {
            return Err(BAD);
        }
        if b.flag()? {
            b.ue(u32::MAX - 1)?;
        }
        if b.flag()? {
            timing.hrd = Some(hrd(b, max_sub, None)?);
        }
    }
    let mut spatial = None;
    if b.flag()? {
        b.flag()?;
        b.flag()?;
        b.flag()?;
        spatial = Some(b.ue(4095)? as u16);
        b.ue(16)?;
        b.ue(16)?;
        b.ue(15)?;
        b.ue(15)?;
    }
    Ok((timing, spatial))
}
