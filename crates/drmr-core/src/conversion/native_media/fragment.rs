use super::{BAD, ConversionError, MAX_METADATA_BYTES, SampleInfo, SampleProtection, UNSUPPORTED};
use super::{
    init::{Defaults, Initialization},
    read::{Layout, Reader, check_stop, full, in_mdat, integer, one, unsigned64},
};
use crate::{
    concurrency::Stop,
    mp4::{self, Mp4Box},
};
use std::ops::Range;

pub(super) struct Run {
    pub range: Range<u64>,
    node: Mp4Box,
    first: i64,
    pub end: i64,
    pub count: u32,
    defaults: Defaults,
    flags: u32,
    data_at: usize,
    first_flags: u32,
}
#[derive(Clone, Copy)]
struct RunStart {
    base: u64,
    data: Option<u64>,
    decode: i64,
}
impl Run {
    fn values(&self, data: &[u8], index: u32) -> Result<(u32, u32, u32, i64), ConversionError> {
        let p = self.node.header.payload(data);
        let fields = [0x100, 0x200, 0x400, 0x800]
            .into_iter()
            .filter(|flag| self.flags & flag != 0)
            .count();
        let mut at = self.data_at
            + usize::try_from(index)
                .map_err(|_| BAD)?
                .checked_mul(fields * 4)
                .ok_or(BAD)?;
        let mut read = |flag, default| -> Result<u32, ConversionError> {
            if self.flags & flag != 0 {
                let value = integer(p, at)?;
                at += 4;
                Ok(value)
            } else {
                Ok(default)
            }
        };
        let duration = read(0x100, self.defaults.duration)?;
        let size = read(0x200, self.defaults.size)?;
        let flags = read(
            0x400,
            if index == 0 {
                self.first_flags
            } else {
                self.defaults.flags
            },
        )?;
        let offset = read(0x800, 0)?;
        super::read::sample_flags(flags)?;
        if duration == 0 || size == 0 {
            return Err(BAD);
        }
        let offset = if p[0] == 0 {
            i64::from(offset)
        } else {
            i64::from(offset.cast_signed())
        };
        Ok((duration, size, flags, offset))
    }
    fn parse(
        node: &Mp4Box,
        data: &[u8],
        start: RunStart,
        defaults: Defaults,
        mdats: &[Range<u64>],
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        let RunStart {
            base,
            data: start,
            decode: first,
        } = start;
        let p = node.header.payload(data);
        let version = *p.first().ok_or(BAD)?;
        if version > 1 {
            return Err(BAD);
        }
        let flags = full(p, version, 0x0000_0f05)?;
        if flags & 4 != 0 && flags & 0x400 != 0 {
            return Err(BAD);
        }
        let total = integer(p, 4)?;
        if total == 0 {
            return Err(BAD);
        }
        let mut at = 8;
        let offset = if flags & 1 != 0 {
            let value = i64::from(integer(p, at)?.cast_signed());
            at += 4;
            u64::try_from(i128::from(base) + i128::from(value)).map_err(|_| BAD)?
        } else {
            start.unwrap_or(base)
        };
        let first_flags = if flags & 4 != 0 {
            let value = integer(p, at)?;
            at += 4;
            value
        } else {
            defaults.flags
        };
        let fields = [0x100, 0x200, 0x400, 0x800]
            .into_iter()
            .filter(|flag| flags & flag != 0)
            .count();
        if usize::try_from(total)
            .map_err(|_| BAD)?
            .checked_mul(fields * 4)
            .and_then(|n| n.checked_add(at))
            != Some(p.len())
        {
            return Err(BAD);
        }
        let mut run = Self {
            range: offset..offset,
            node: node.clone(),
            first,
            end: first,
            count: total,
            defaults,
            flags,
            data_at: at,
            first_flags,
        };
        super::read::sample_flags(defaults.flags)?;
        super::read::sample_flags(first_flags)?;
        let mut size = 0_u64;
        let mut duration = 0_u64;
        if fields == 0 {
            if defaults.size == 0 || defaults.duration == 0 {
                return Err(BAD);
            }
            size = u64::from(defaults.size)
                .checked_mul(u64::from(total))
                .ok_or(BAD)?;
            duration = u64::from(defaults.duration)
                .checked_mul(u64::from(total))
                .ok_or(BAD)?;
        } else {
            for index in 0..total {
                check_stop(stop)?;
                let (d, s, _, offset) = run.values(data, index)?;
                let dts = first
                    .checked_add(i64::try_from(duration).map_err(|_| BAD)?)
                    .ok_or(BAD)?;
                dts.checked_add(offset)
                    .and_then(|pts| pts.checked_add(i64::from(d)))
                    .ok_or(BAD)?;
                size = size.checked_add(u64::from(s)).ok_or(BAD)?;
                duration = duration.checked_add(u64::from(d)).ok_or(BAD)?;
            }
        }
        run.range.end = run.range.start.checked_add(size).ok_or(BAD)?;
        in_mdat(&run.range, mdats)?;
        run.end = first
            .checked_add(i64::try_from(duration).map_err(|_| BAD)?)
            .ok_or(BAD)?;
        Ok(run)
    }
    pub fn walk(
        &self,
        data: &[u8],
        stop: &Stop,
        ordinal: &mut u64,
        visit: &mut impl FnMut(SampleInfo) -> Result<(), ConversionError>,
    ) -> Result<(), ConversionError> {
        let mut position = self.range.start;
        let mut dts = self.first;
        for index in 0..self.count {
            check_stop(stop)?;
            let (duration, size, flags, offset) = self.values(data, index)?;
            let end = position
                .checked_add(u64::from(size))
                .filter(|end| *end <= self.range.end)
                .ok_or(BAD)?;
            visit(SampleInfo {
                ordinal: *ordinal,
                dts,
                pts: dts.checked_add(offset).ok_or(BAD)?,
                duration,
                size,
                range: position..end,
                flags,
                is_sync: flags & (1 << 16) == 0,
                roll_distance: None,
                preroll_distance: None,
                protection: SampleProtection::Clear,
                chunk_offset: 0,
                is_last_chunk: true,
            })?;
            *ordinal = ordinal.checked_add(1).ok_or(BAD)?;
            position = end;
            dts = dts.checked_add(i64::from(duration)).ok_or(BAD)?;
        }
        if position != self.range.end || dts != self.end {
            return Err(BAD);
        }
        Ok(())
    }
}
pub(super) struct TrackFragment {
    pub index: usize,
    pub runs: Vec<Run>,
    pub auxiliary: Vec<Mp4Box>,
    pub auxiliary_base: u64,
    pub count: u64,
    pub first: i64,
    pub end: i64,
    pub groups: super::groups::Resolver,
}
pub(super) struct Fragment {
    pub bytes: Vec<u8>,
    pub tracks: Vec<TrackFragment>,
    pub source_base: u64,
}
fn defaults(
    bytes: &[u8],
    initial: Defaults,
    moof: u64,
    previous: u64,
    first: bool,
) -> Result<(u32, Defaults, u64), ConversionError> {
    let flags = full(bytes, 0, 0x0003_003b)?;
    if flags & 0x10000 != 0 {
        return Err(UNSUPPORTED);
    }
    if flags & 1 != 0 && flags & 0x20000 != 0 {
        return Err(BAD);
    }
    let id = integer(bytes, 4)?;
    let mut at = 8;
    let base = if flags & 1 != 0 {
        let value = unsigned64(bytes, at)?;
        at += 8;
        value
    } else if first || flags & 0x20000 != 0 {
        moof
    } else {
        previous
    };
    if flags & 2 != 0 {
        if integer(bytes, at)? != 1 {
            return Err(UNSUPPORTED);
        }
        at += 4;
    }
    let mut defaults = initial;
    for (flag, value) in [
        (8, &mut defaults.duration),
        (16, &mut defaults.size),
        (32, &mut defaults.flags),
    ] {
        if flags & flag != 0 {
            *value = integer(bytes, at)?;
            at += 4;
        }
    }
    if at != bytes.len() {
        return Err(BAD);
    }
    Ok((id, defaults, base))
}
struct FragmentSource<'a> {
    init: &'a Initialization,
    mdats: &'a [Range<u64>],
    offset: u64,
    stop: &'a Stop,
}
impl FragmentSource<'_> {
    fn track(
        &self,
        traf: &Mp4Box,
        bytes: &[u8],
        previous: u64,
        is_first: bool,
    ) -> Result<TrackFragment, ConversionError> {
        let tfhd = one(&traf.children, *b"tfhd")?.header.payload(bytes);
        let id = integer(tfhd, 4)?;
        let index = self
            .init
            .tracks
            .iter()
            .position(|track| track.descriptor.track_id == id)
            .ok_or(BAD)?;
        let (_, defaults, base) = defaults(
            tfhd,
            self.init.tracks[index].defaults,
            self.offset,
            previous,
            is_first,
        )?;
        let tfdt = one(&traf.children, *b"tfdt")?.header.payload(bytes);
        let version = *tfdt.first().ok_or(BAD)?;
        if version > 1 {
            return Err(BAD);
        }
        full(tfdt, version, 0)?;
        let first = if version == 0 {
            if tfdt.len() != 8 {
                return Err(BAD);
            }
            u64::from(integer(tfdt, 4)?)
        } else {
            if tfdt.len() != 12 {
                return Err(BAD);
            }
            unsigned64(tfdt, 4)?
        };
        let first = i64::try_from(first).map_err(|_| BAD)?;
        let mut end = first;
        let mut runs = Vec::new();
        let mut position = None;
        let mut count = 0_u64;
        super::controls::semantic_children(traf)?;
        for node in &traf.children {
            if node.kind() == *b"trun" {
                let run = Run::parse(
                    node,
                    bytes,
                    RunStart {
                        base,
                        data: position,
                        decode: end,
                    },
                    defaults,
                    self.mdats,
                    self.stop,
                )?;
                end = run.end;
                position = Some(run.range.end);
                count = count.checked_add(u64::from(run.count)).ok_or(BAD)?;
                runs.push(run);
            }
        }
        if runs.is_empty() {
            return Err(BAD);
        }
        let auxiliary = traf
            .children
            .iter()
            .filter(|node| matches!(&node.kind(), b"senc" | b"saiz" | b"saio"))
            .cloned()
            .collect();
        let groups = super::groups::Resolver::new(
            &self.init.tracks[index].groups,
            &traf.children,
            bytes,
            count,
            true,
        )?;
        Ok(TrackFragment {
            index,
            runs,
            auxiliary,
            auxiliary_base: base,
            count,
            first,
            end,
            groups,
        })
    }
}

pub(super) fn parse(
    reader: &mut Reader<'_>,
    range: &Range<u64>,
    layout: &Layout,
    init: &Initialization,
) -> Result<Fragment, ConversionError> {
    let bytes = reader.read(range.clone(), MAX_METADATA_BYTES)?;
    let nodes = mp4::parse_boxes_with_limit(&bytes, super::MAX_PARSED_NODES).map_err(|_| BAD)?;
    let moof = one(&nodes, *b"moof")?;
    if nodes.len() != 1 {
        return Err(BAD);
    }
    let mfhd = one(&moof.children, *b"mfhd")?.header.payload(&bytes);
    full(mfhd, 0, 0)?;
    if mfhd.len() != 8 {
        return Err(BAD);
    }
    let mut tracks = Vec::new();
    let mut previous = range.start;
    let source = FragmentSource {
        init,
        mdats: &layout.mdats,
        offset: range.start,
        stop: reader.stop,
    };
    for traf in moof.children.iter().filter(|node| node.kind() == *b"traf") {
        let part = source.track(traf, &bytes, previous, tracks.is_empty())?;
        previous = part.runs.last().ok_or(BAD)?.range.end;
        tracks.push(part);
    }
    super::controls::semantic_children(moof)?;
    if tracks.is_empty() {
        return Err(UNSUPPORTED);
    }
    Ok(Fragment {
        bytes,
        tracks,
        source_base: range.start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn trun(flags: u32, fields: &[u32]) -> Vec<u8> {
        let mut payload = flags.to_be_bytes().to_vec();
        payload.extend(1_u32.to_be_bytes());
        for field in fields {
            payload.extend(field.to_be_bytes());
        }
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(b"trun");
        bytes.extend(payload);
        bytes
    }
    fn parsed(
        bytes: &[u8],
        base: u64,
        first: i64,
        default: Defaults,
        mdats: &[Range<u64>],
    ) -> Result<Run, ConversionError> {
        let nodes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
        Run::parse(
            &nodes[0],
            bytes,
            RunStart {
                base,
                data: None,
                decode: first,
            },
            default,
            mdats,
            &Stop::new(),
        )
    }
    #[test]
    fn signed_data_offsets_and_negative_composition_ticks_are_exact() {
        let bytes = trun(
            0x0100_0801,
            &[(-100_i32).cast_unsigned(), (-20_i32).cast_unsigned()],
        );
        let run = parsed(
            &bytes,
            200,
            10,
            Defaults {
                duration: 90,
                size: 4,
                flags: 0,
            },
            std::slice::from_ref(&(100..104)),
        )
        .unwrap();
        assert_eq!(run.range, 100..104);
        let mut ordinal = 0;
        let mut seen = 0;
        run.walk(&bytes, &Stop::new(), &mut ordinal, &mut |info| {
            assert_eq!((info.dts, info.pts, info.duration), (10, -10, 90));
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 1);
        assert!(
            parsed(
                &bytes,
                50,
                10,
                Defaults {
                    duration: 90,
                    size: 4,
                    flags: 0
                },
                std::slice::from_ref(&(100..104))
            )
            .is_err()
        );
    }
    #[test]
    fn trun_defaults_sizes_count_flags_and_terminal_bounds_cannot_be_invented() {
        let bytes = trun(1, &[100]);
        assert!(
            parsed(
                &bytes,
                0,
                0,
                Defaults {
                    duration: 0,
                    size: 4,
                    flags: 0
                },
                std::slice::from_ref(&(100..104))
            )
            .is_err()
        );
        assert!(
            parsed(
                &bytes,
                0,
                0,
                Defaults {
                    duration: 90,
                    size: 0,
                    flags: 0
                },
                std::slice::from_ref(&(100..104))
            )
            .is_err()
        );
        assert!(
            parsed(
                &bytes,
                0,
                0,
                Defaults {
                    duration: 90,
                    size: 5,
                    flags: 0
                },
                std::slice::from_ref(&(100..104))
            )
            .is_err()
        );
        let reserved = trun(0x1001, &[100]);
        assert!(
            parsed(
                &reserved,
                0,
                0,
                Defaults {
                    duration: 90,
                    size: 4,
                    flags: 0
                },
                std::slice::from_ref(&(100..104))
            )
            .is_err()
        );
        for length in 8..bytes.len() {
            let mut short = bytes[..length].to_vec();
            short[..4].copy_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
            assert!(
                parsed(
                    &short,
                    0,
                    0,
                    Defaults {
                        duration: 90,
                        size: 4,
                        flags: 0
                    },
                    std::slice::from_ref(&(100..104))
                )
                .is_err()
            );
        }
    }
}
