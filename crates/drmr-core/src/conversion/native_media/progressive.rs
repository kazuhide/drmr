use super::{BAD, ConversionError, SampleInfo, SampleProtection, UNSUPPORTED};
use super::{
    init::Track,
    read::{check_stop, count, full, in_mdat, integer, one, optional, unsigned64},
};
use crate::concurrency::Stop;
use std::ops::Range;

pub(super) struct Chunk {
    pub range: Range<u64>,
    samples: usize,
}
pub(super) struct Tables<'a> {
    pub count: usize,
    sizes: &'a [u8],
    default_size: u32,
    stts: &'a [u8],
    ctts: Option<&'a [u8]>,
    stss: Option<&'a [u8]>,
    sdtp: Option<&'a [u8]>,
    offsets: &'a [u8],
    offset_width: usize,
    chunk_count: usize,
    map: &'a [u8],
    mappings: usize,
    mdats: &'a [Range<u64>],
    groups: super::groups::Resolver,
}
/// Fragmented initialization must not conceal an unvisited progressive sample table.
pub(super) fn empty(track: &Track, data: &[u8]) -> Result<(), ConversionError> {
    let nodes = &track.table.children;
    let sizes = one(nodes, *b"stsz")?.header.payload(data);
    full(sizes, 0, 0)?;
    if sizes.len() != 12 || integer(sizes, 8)? != 0 {
        return Err(UNSUPPORTED);
    }
    for kind in [*b"stts", *b"stsc"] {
        let p = one(nodes, kind)?.header.payload(data);
        full(p, 0, 0)?;
        if p.len() != 8 || integer(p, 4)? != 0 {
            return Err(BAD);
        }
    }
    let offsets = match (optional(nodes, *b"stco")?, optional(nodes, *b"co64")?) {
        (Some(node), None) | (None, Some(node)) => node.header.payload(data),
        _ => return Err(BAD),
    };
    full(offsets, 0, 0)?;
    if offsets.len() != 8 || integer(offsets, 4)? != 0 {
        return Err(BAD);
    }
    for kind in [*b"ctts", *b"stss", *b"sdtp"] {
        if let Some(node) = optional(nodes, kind)? {
            let p = node.header.payload(data);
            let version = *p.first().ok_or(BAD)?;
            if version > u8::from(kind == *b"ctts") {
                return Err(BAD);
            }
            full(p, version, 0)?;
            if p.len() != if kind == *b"sdtp" { 4 } else { 8 }
                || kind != *b"sdtp" && integer(p, 4)? != 0
            {
                return Err(BAD);
            }
        }
    }
    super::groups::Resolver::new(&track.groups, nodes, data, 0, false)?;
    Ok(())
}
fn clocks<'a>(
    nodes: &[crate::mp4::Mp4Box],
    data: &'a [u8],
    total: usize,
    media_duration: u64,
    stop: &Stop,
) -> Result<(&'a [u8], Option<&'a [u8]>), ConversionError> {
    let stts = one(nodes, *b"stts")?.header.payload(data);
    full(stts, 0, 0)?;
    count(stts, 4, 8, 8)?;
    let mut counted = 0_u64;
    let mut duration = 0_u64;
    for row in stts[8..].as_chunks::<8>().0 {
        check_stop(stop)?;
        let samples = integer(row, 0)?;
        let delta = integer(row, 4)?;
        if samples == 0 || delta == 0 {
            return Err(BAD);
        }
        counted = counted.checked_add(u64::from(samples)).ok_or(BAD)?;
        duration = duration
            .checked_add(
                u64::from(samples)
                    .checked_mul(u64::from(delta))
                    .ok_or(BAD)?,
            )
            .ok_or(BAD)?;
    }
    if counted != total as u64
        || duration > i64::MAX as u64
        || (media_duration != 0 && duration != media_duration)
    {
        return Err(BAD);
    }
    let ctts = optional(nodes, *b"ctts")?.map(|node| node.header.payload(data));
    if let Some(ctts) = ctts {
        let version = *ctts.first().ok_or(BAD)?;
        if version > 1 {
            return Err(BAD);
        }
        full(ctts, version, 0)?;
        count(ctts, 4, 8, 8)?;
        let mut entries = 0_u64;
        for row in ctts[8..].as_chunks::<8>().0 {
            check_stop(stop)?;
            let samples = integer(row, 0)?;
            if samples == 0 {
                return Err(BAD);
            }
            entries = entries.checked_add(u64::from(samples)).ok_or(BAD)?;
        }
        if entries != total as u64 {
            return Err(BAD);
        }
    }
    validate_clocks(stts, ctts, total as u64, stop)?;
    Ok((stts, ctts))
}
type DependencyTables<'a> = (Option<&'a [u8]>, Option<&'a [u8]>);
fn dependencies<'a>(
    nodes: &[crate::mp4::Mp4Box],
    data: &'a [u8],
    total: usize,
) -> Result<DependencyTables<'a>, ConversionError> {
    let stss = optional(nodes, *b"stss")?.map(|node| node.header.payload(data));
    if let Some(stss) = stss {
        full(stss, 0, 0)?;
        count(stss, 4, 8, 4)?;
        let mut previous = 0;
        for row in stss[8..].as_chunks::<4>().0 {
            let sample = integer(row, 0)?;
            if sample <= previous || sample as usize > total {
                return Err(BAD);
            }
            previous = sample;
        }
    }
    let sdtp = optional(nodes, *b"sdtp")?.map(|node| node.header.payload(data));
    if let Some(sdtp) = sdtp {
        full(sdtp, 0, 0)?;
        if sdtp.len() != 4 + total {
            return Err(BAD);
        }
        for value in &sdtp[4..] {
            super::read::sample_flags(u32::from(*value) << 20)?;
        }
    }
    Ok((stss, sdtp))
}
impl<'a> Tables<'a> {
    pub fn parse(
        track: &Track,
        data: &'a [u8],
        mdats: &'a [Range<u64>],
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        let nodes = &track.table.children;
        let sizes = one(nodes, *b"stsz")?.header.payload(data);
        full(sizes, 0, 0)?;
        let default_size = integer(sizes, 4)?;
        let total = usize::try_from(integer(sizes, 8)?).map_err(|_| BAD)?;
        if total == 0 {
            return Err(BAD);
        }
        if sizes.len()
            != if default_size == 0 {
                12usize
                    .checked_add(total.checked_mul(4).ok_or(BAD)?)
                    .ok_or(BAD)?
            } else {
                12
            }
        {
            return Err(BAD);
        }
        let (decode_timing, composition_timing) =
            clocks(nodes, data, total, track.descriptor.media_duration, stop)?;
        let (sync_samples, dependency_table) = dependencies(nodes, data, total)?;
        let offsets32 = optional(nodes, *b"stco")?;
        let offsets64 = optional(nodes, *b"co64")?;
        let (offsets, width) = match (offsets32, offsets64) {
            (Some(node), None) => (node.header.payload(data), 4),
            (None, Some(node)) => (node.header.payload(data), 8),
            _ => return Err(BAD),
        };
        full(offsets, 0, 0)?;
        let chunk_count = count(offsets, 4, 8, width)?;
        if chunk_count == 0 {
            return Err(BAD);
        }
        let map = one(nodes, *b"stsc")?.header.payload(data);
        full(map, 0, 0)?;
        let mappings = count(map, 4, 8, 12)?;
        if mappings == 0 {
            return Err(BAD);
        }
        let mut previous = 0;
        for row in map[8..].as_chunks::<12>().0 {
            let first = integer(row, 0)?;
            if first <= previous
                || first as usize > chunk_count
                || integer(row, 4)? == 0
                || integer(row, 8)? != 1
            {
                return Err(UNSUPPORTED);
            }
            previous = first;
        }
        if integer(map, 8)? != 1 {
            return Err(BAD);
        }
        let groups = super::groups::Resolver::new(&track.groups, nodes, data, total as u64, false)?;
        let tables = Self {
            count: total,
            sizes,
            default_size,
            stts: decode_timing,
            ctts: composition_timing,
            stss: sync_samples,
            sdtp: dependency_table,
            offsets,
            offset_width: width,
            chunk_count,
            map,
            mappings,
            mdats,
            groups,
        };
        tables.chunks(stop, |_| Ok(()))?;
        Ok(tables)
    }
    pub fn chunks(
        &self,
        stop: &Stop,
        mut visit: impl FnMut(Chunk) -> Result<(), ConversionError>,
    ) -> Result<(), ConversionError> {
        let mut ordinal = 0usize;
        let mut mapping = 0;
        for index in 0..self.chunk_count {
            check_stop(stop)?;
            while mapping + 1 < self.mappings
                && integer(self.map, 8 + (mapping + 1) * 12)? as usize <= index + 1
            {
                mapping += 1;
            }
            let samples =
                usize::try_from(integer(self.map, 12 + mapping * 12)?).map_err(|_| BAD)?;
            let end = ordinal
                .checked_add(samples)
                .filter(|end| *end <= self.count)
                .ok_or(BAD)?;
            let mut length = 0_u64;
            if self.default_size != 0 {
                length = u64::from(self.default_size)
                    .checked_mul(samples as u64)
                    .ok_or(BAD)?;
            } else {
                for sample in ordinal..end {
                    check_stop(stop)?;
                    length = length
                        .checked_add(u64::from(self.size(sample)?))
                        .ok_or(BAD)?;
                }
            }
            let offset = if self.offset_width == 4 {
                u64::from(integer(self.offsets, 8 + index * self.offset_width)?)
            } else {
                unsigned64(self.offsets, 8 + index * self.offset_width)?
            };
            let range = offset..offset.checked_add(length).ok_or(BAD)?;
            in_mdat(&range, self.mdats)?;
            visit(Chunk { range, samples })?;
            ordinal = end;
        }
        if ordinal != self.count {
            return Err(BAD);
        }
        Ok(())
    }
    fn size(&self, index: usize) -> Result<u32, ConversionError> {
        let size = if self.default_size == 0 {
            integer(self.sizes, 12 + index * 4)?
        } else {
            self.default_size
        };
        if size == 0 {
            return Err(BAD);
        }
        Ok(size)
    }
    pub fn walk(
        &self,
        stop: &Stop,
        mut visit: impl FnMut(SampleInfo) -> Result<(), ConversionError>,
    ) -> Result<(), ConversionError> {
        let mut ordinal = 0usize;
        let mut dts = 0_i64;
        let mut run = 0;
        let mut remaining = 0_u32;
        let mut delta = 0;
        let mut composition_run = 0;
        let mut composition_remaining = 0_u32;
        let mut composition = 0_i64;
        let mut sync_at = 8;
        self.chunks(stop, |chunk| {
            let mut offset = chunk.range.start;
            for _ in 0..chunk.samples {
                check_stop(stop)?;
                if remaining == 0 {
                    remaining = integer(self.stts, 8 + run * 8)?;
                    delta = integer(self.stts, 12 + run * 8)?;
                    run += 1;
                }
                remaining -= 1;
                if let Some(ctts) = self.ctts {
                    if composition_remaining == 0 {
                        composition_remaining = integer(ctts, 8 + composition_run * 8)?;
                        let value = integer(ctts, 12 + composition_run * 8)?;
                        composition = if ctts[0] == 0 {
                            i64::from(value)
                        } else {
                            i64::from(value.cast_signed())
                        };
                        composition_run += 1;
                    }
                    composition_remaining -= 1;
                }
                let is_sync = if let Some(stss) = self.stss {
                    let found =
                        sync_at < stss.len() && integer(stss, sync_at)? as usize == ordinal + 1;
                    if found {
                        sync_at += 4;
                    }
                    found
                } else {
                    true
                };
                let flags = self
                    .sdtp
                    .map_or(0, |bytes| u32::from(bytes[4 + ordinal]) << 20)
                    | if is_sync { 0 } else { 1 << 16 };
                let size = self.size(ordinal)?;
                let end = offset
                    .checked_add(u64::from(size))
                    .filter(|end| *end <= chunk.range.end)
                    .ok_or(BAD)?;
                let (roll_distance, preroll_distance) = self.groups.at(ordinal as u64);
                visit(SampleInfo {
                    ordinal: ordinal as u64,
                    dts,
                    pts: dts.checked_add(composition).ok_or(BAD)?,
                    duration: delta,
                    size,
                    range: offset..end,
                    flags,
                    is_sync,
                    roll_distance,
                    preroll_distance,
                    protection: SampleProtection::Clear,
                    chunk_offset: 0,
                    is_last_chunk: true,
                })?;
                dts = dts.checked_add(i64::from(delta)).ok_or(BAD)?;
                offset = end;
                ordinal += 1;
            }
            if offset != chunk.range.end {
                return Err(BAD);
            }
            Ok(())
        })?;
        if ordinal != self.count || remaining != 0 || composition_remaining != 0 {
            return Err(BAD);
        }
        Ok(())
    }
}

/// Validate intersecting timing runs without expanding constant-duration sample counts.
fn validate_clocks(
    stts: &[u8],
    ctts: Option<&[u8]>,
    total: u64,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut decode_at = 8;
    let mut composition_at = 8;
    let mut decode_left = 0_u64;
    let mut composition_left = 0_u64;
    let mut duration = 0_u64;
    let mut offset = 0_i64;
    let mut ordinal = 0_u64;
    let mut dts = 0_i64;
    while ordinal < total {
        check_stop(stop)?;
        if decode_left == 0 {
            decode_left = u64::from(integer(stts, decode_at)?);
            duration = u64::from(integer(stts, decode_at + 4)?);
            decode_at += 8;
        }
        if composition_left == 0 {
            if let Some(ctts) = ctts {
                composition_left = u64::from(integer(ctts, composition_at)?);
                let value = integer(ctts, composition_at + 4)?;
                offset = if ctts[0] == 0 {
                    i64::from(value)
                } else {
                    i64::from(value.cast_signed())
                };
                composition_at += 8;
            } else {
                composition_left = total - ordinal;
            }
        }
        let count = decode_left.min(composition_left);
        if count == 0 {
            return Err(BAD);
        }
        let last = dts
            .checked_add(
                i64::try_from((count - 1).checked_mul(duration).ok_or(BAD)?).map_err(|_| BAD)?,
            )
            .ok_or(BAD)?;
        dts.checked_add(offset).ok_or(BAD)?;
        last.checked_add(offset)
            .and_then(|pts| pts.checked_add(i64::try_from(duration).ok()?))
            .ok_or(BAD)?;
        dts = dts
            .checked_add(i64::try_from(count.checked_mul(duration).ok_or(BAD)?).map_err(|_| BAD)?)
            .ok_or(BAD)?;
        ordinal = ordinal
            .checked_add(count)
            .filter(|ordinal| *ordinal <= total)
            .ok_or(BAD)?;
        decode_left -= count;
        composition_left -= count;
    }
    if decode_left != 0
        || composition_left != 0
        || decode_at != stts.len()
        || ctts.is_some_and(|ctts| composition_at != ctts.len())
    {
        return Err(BAD);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn table(version: u8, rows: &[(u32, u32)]) -> Vec<u8> {
        let mut bytes = vec![version, 0, 0, 0];
        bytes.extend(u32::try_from(rows.len()).unwrap().to_be_bytes());
        for (count, value) in rows {
            bytes.extend(count.to_be_bytes());
            bytes.extend(value.to_be_bytes());
        }
        bytes
    }
    #[test]
    fn compressed_six_hour_audio_clock_has_no_arbitrary_sample_count_limit() {
        let count = 1_012_500;
        let stts = table(0, &[(count, 1024)]);
        validate_clocks(&stts, None, u64::from(count), &Stop::new()).unwrap();
    }
    #[test]
    fn signed_composition_offsets_keep_negative_raw_pts_and_reject_overflow_or_incomplete_runs() {
        let stts = table(0, &[(2, 100)]);
        let ctts = table(1, &[(1, (-20_i32).cast_unsigned()), (1, 30)]);
        validate_clocks(&stts, Some(&ctts), 2, &Stop::new()).unwrap();
        assert!(validate_clocks(&stts, Some(&table(1, &[(1, 0)])), 2, &Stop::new()).is_err());
        assert!(validate_clocks(&stts, Some(&ctts), 3, &Stop::new()).is_err());
        let extreme = table(0, &[(u32::MAX, u32::MAX)]);
        assert!(validate_clocks(&extreme, None, u64::from(u32::MAX), &Stop::new()).is_err());
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            validate_clocks(&stts, None, 2, &stop).unwrap_err(),
            ConversionError("cancelled")
        );
    }
}
