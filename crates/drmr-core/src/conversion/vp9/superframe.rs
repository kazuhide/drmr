//! Annex B tail indexes are bounded; a one-frame index is still an index.
use super::{BAD, ConversionError, MAX_BYTES};
use std::ops::Range;
pub(super) struct Layout {
    pub frames: Vec<Range<usize>>,
    pub index_bytes: usize,
}
pub(super) fn split(data: &[u8]) -> Result<Layout, ConversionError> {
    if data.is_empty() {
        return Err(BAD);
    }
    if data.len() > MAX_BYTES {
        return Err(ConversionError("native-vp9-sample-size-limit"));
    }
    let marker = *data.last().ok_or(BAD)?;
    if marker & 0xe0 != 0xc0 {
        return Ok(Layout {
            frames: std::iter::once(0..data.len()).collect(),
            index_bytes: 0,
        });
    }
    let count = usize::from(marker & 7) + 1;
    let magnitude = usize::from((marker >> 3) & 3) + 1;
    let index_bytes = count
        .checked_mul(magnitude)
        .and_then(|size| size.checked_add(2))
        .ok_or(BAD)?;
    let start = data.len().checked_sub(index_bytes).ok_or(BAD)?;
    if data.get(start) != Some(&marker) {
        return Err(BAD);
    }
    let mut cursor = start + 1;
    let mut offset = 0_usize;
    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let size = data[cursor..cursor + magnitude]
            .iter()
            .enumerate()
            .fold(0_u64, |value, (shift, byte)| {
                value | (u64::from(*byte) << (shift * 8))
            });
        cursor += magnitude;
        let size = usize::try_from(size).map_err(|_| BAD)?;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= start && *end > offset)
            .ok_or(BAD)?;
        frames.push(offset..end);
        offset = end;
    }
    if offset != start || cursor != data.len() - 1 {
        return Err(BAD);
    }
    Ok(Layout {
        frames,
        index_bytes,
    })
}
