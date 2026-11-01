//! Picture parameter controls and exact tile-scan/entry-point mapping.
use super::{
    BAD, ConversionError,
    bits::{self, Bits},
    parameters::{Sps, scaling_list},
};

#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "PPS syntax flags independently control slice parsing and are not exclusive states."
)]
pub(super) struct Pps {
    pub id: u32,
    pub dependent: bool,
    pub output_flag: bool,
    pub extra_bits: u32,
    pub cabac_init: bool,
    pub l0: u32,
    pub l1: u32,
    pub init_qp: i32,
    pub cb_offset: i32,
    pub cr_offset: i32,
    pub chroma_offsets: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub tiles: bool,
    pub wpp: bool,
    pub columns: Vec<u32>,
    pub rows: Vec<u32>,
    pub loop_across_slices: bool,
    pub deblock_override: bool,
    pub deblock_disabled: bool,
    pub list_modification: bool,
    pub header_extension: bool,
}
impl Pps {
    pub fn parse(nal: &[u8], sps: &Sps) -> Result<Self, ConversionError> {
        if bits::header(nal)?.kind != 34 {
            return Err(BAD);
        }
        let rbsp = bits::rbsp(nal)?;
        let b = &mut Bits::new(&rbsp);
        let id = b.ue(63)?;
        if b.ue(15)? != sps.id {
            return Err(BAD);
        }
        let dependent = b.flag()?;
        let output_flag = b.flag()?;
        let extra_bits = b.u(3)?;
        b.flag()?;
        let cabac_init = b.flag()?;
        let l0 = b.ue(14)? + 1;
        let l1 = b.ue(14)? + 1;
        let init_qp = 26 + b.se(-(26 + 6 * i32::from(sps.metadata.bit_depth_luma - 8)), 25)?;
        b.flag()?;
        b.flag()?;
        if b.flag()? {
            b.ue(sps.log2_ctb - sps.log2_min_cb)?;
        }
        let cb_offset = b.se(-12, 12)?;
        let cr_offset = b.se(-12, 12)?;
        let chroma_offsets = b.flag()?;
        let weighted_pred = b.flag()?;
        let weighted_bipred = b.flag()?;
        b.flag()?;
        let tiles = b.flag()?;
        let wpp = b.flag()?;
        let mut columns = vec![sps.ctb_width];
        let mut rows = vec![sps.ctb_height];
        if tiles {
            let column_count = b.ue(sps.ctb_width.saturating_sub(1).min(19))? + 1;
            let row_count = b.ue(sps.ctb_height.saturating_sub(1).min(21))? + 1;
            let uniform = b.flag()?;
            columns = tile_sizes(b, column_count, sps.ctb_width, uniform)?;
            rows = tile_sizes(b, row_count, sps.ctb_height, uniform)?;
            b.flag()?;
        }
        let loop_across_slices = b.flag()?;
        let mut deblock_override = false;
        let mut deblock_disabled = false;
        if b.flag()? {
            deblock_override = b.flag()?;
            deblock_disabled = b.flag()?;
            if !deblock_disabled {
                b.se(-6, 6)?;
                b.se(-6, 6)?;
            }
        }
        if b.flag()? {
            scaling_list(b)?;
        }
        let list_modification = b.flag()?;
        b.ue(sps.log2_ctb - 2)?;
        let header_extension = b.flag()?;
        if b.flag()? && b.u(8)? != 0 {
            return Err(ConversionError("native-hevc-pps-extension-not-supported"));
        }
        b.trailing()?;
        Ok(Self {
            id,
            dependent,
            output_flag,
            extra_bits,
            cabac_init,
            l0,
            l1,
            init_qp,
            cb_offset,
            cr_offset,
            chroma_offsets,
            weighted_pred,
            weighted_bipred,
            tiles,
            wpp,
            columns,
            rows,
            loop_across_slices,
            deblock_override,
            deblock_disabled,
            list_modification,
            header_extension,
        })
    }
    /// Raster slice addresses are ordered by their tile-scan positions, not raw numbers.
    pub fn tile_scan_address(&self, address: u32, sps: &Sps) -> Result<u32, ConversionError> {
        if address >= sps.ctbs()? {
            return Err(BAD);
        }
        let (x, y) = (address % sps.ctb_width, address / sps.ctb_width);
        let mut top = 0;
        let mut preceding_rows = 0;
        for &height in &self.rows {
            if y >= top + height {
                top += height;
                preceding_rows += height * sps.ctb_width;
                continue;
            }
            let mut left = 0;
            let mut preceding_columns = 0;
            for &width in &self.columns {
                if x >= left + width {
                    left += width;
                    preceding_columns += width * height;
                    continue;
                }
                return Ok(preceding_rows + preceding_columns + (y - top) * width + x - left);
            }
        }
        Err(BAD)
    }
    pub fn entry_points(&self, start: u32, end: u32) -> Result<u32, ConversionError> {
        if start >= end {
            return Err(BAD);
        }
        if !self.tiles && !self.wpp {
            return Ok(0);
        }
        let mut base = 0_u32;
        let mut total = 0_u32;
        for &height in &self.rows {
            for &width in &self.columns {
                let extent = width.checked_mul(height).ok_or(BAD)?;
                let tile_end = base.checked_add(extent).ok_or(BAD)?;
                let low = start.checked_add(1).ok_or(BAD)?.max(base);
                let high = end.min(tile_end);
                if low < high {
                    let step = if self.wpp { width } else { extent };
                    let first = (low - base).div_ceil(step);
                    let last = (high - 1 - base) / step;
                    if first <= last {
                        total = total.checked_add(last - first + 1).ok_or(BAD)?;
                    }
                }
                base = tile_end;
            }
        }
        Ok(total)
    }
}
fn tile_sizes(
    b: &mut Bits<'_>,
    count: u32,
    extent: u32,
    uniform: bool,
) -> Result<Vec<u32>, ConversionError> {
    let mut sizes = Vec::new();
    let mut used = 0;
    for index in 0..count {
        let size = if uniform {
            (index + 1) * extent / count - index * extent / count
        } else if index + 1 == count {
            extent.checked_sub(used).ok_or(BAD)?
        } else {
            b.ue(extent - 1)? + 1
        };
        if size == 0 {
            return Err(BAD);
        }
        used = used.checked_add(size).ok_or(BAD)?;
        if used > extent {
            return Err(BAD);
        }
        sizes.push(size);
    }
    if used != extent {
        return Err(BAD);
    }
    Ok(sizes)
}
