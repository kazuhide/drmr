//! Flat ISO BMFF segment indexes, with checked byte extents and exact rational clocks.
use super::{
    ConversionError, MAX_BYTES, MAX_SEGMENTS, Path,
    addressing::{ByteRange, Resource, Segment},
    error,
};
use crate::{concurrency::Stop, conversion::files, native::FileIdentity};
use std::io::{Read, Seek, SeekFrom};
use zeroize::Zeroizing;

pub(super) struct Index {
    pub reference_id: u32,
    pub timescale: u64,
    pub segments: Vec<Segment>,
}

pub(super) fn resolve(
    root: &Path,
    resolved: &mut super::addressing::Representation,
    track: (u32, u32),
    remaining: usize,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let Some(index) = &resolved.index else {
        return Ok(());
    };
    let path = super::reference(root, &resolved.base, &index.uri)?;
    let range = index.range.ok_or(error("dash-index-range-required"))?;
    let parsed = read(&path, range, &index.uri, remaining, stop)?;
    if parsed.reference_id != track.0 || parsed.timescale != resolved.timescale {
        return Err(error("dash-index-track-or-timescale-mismatch"));
    }
    check_layout(
        &path,
        resolved
            .initialization
            .range
            .ok_or(error("dash-initialization-range-required"))?,
        range,
        &parsed.segments,
        stop,
    )?;
    resolved.segments = parsed.segments;
    Ok(())
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at.checked_add(4).ok_or(error("dash-invalid-index"))?)
            .ok_or(error("dash-invalid-index"))?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64, ConversionError> {
    Ok(u64::from_be_bytes(
        bytes
            .get(at..at.checked_add(8).ok_or(error("dash-invalid-index"))?)
            .ok_or(error("dash-invalid-index"))?
            .try_into()
            .unwrap(),
    ))
}

pub(super) fn parse(
    bytes: &[u8],
    extent: ByteRange,
    file_bytes: u64,
    uri: &str,
    remaining: usize,
) -> Result<Index, ConversionError> {
    let bad = error("dash-invalid-index");
    if bytes.len() < 32
        || extent.length()? != bytes.len() as u64
        || extent.end >= file_bytes
        || file_bytes > MAX_BYTES
        || bytes.get(4..8) != Some(b"sidx")
    {
        return Err(bad);
    }
    let header = if u32_at(bytes, 0)? == 1 {
        if u64_at(bytes, 8)? != bytes.len() as u64 {
            return Err(bad);
        }
        16
    } else {
        if u64::from(u32_at(bytes, 0)?) != bytes.len() as u64 {
            return Err(bad);
        }
        8
    };
    let version = *bytes.get(header).ok_or(bad)?;
    if !matches!(version, 0 | 1) || bytes.get(header + 1..header + 4) != Some(&[0, 0, 0]) {
        return Err(bad);
    }
    let reference_id = u32_at(bytes, header + 4)?;
    let timescale = u64::from(u32_at(bytes, header + 8)?);
    if reference_id == 0 || timescale == 0 {
        return Err(bad);
    }
    let (mut time, offset, at) = if version == 0 {
        (
            u64::from(u32_at(bytes, header + 12)?),
            u64::from(u32_at(bytes, header + 16)?),
            header + 20,
        )
    } else {
        (
            u64_at(bytes, header + 12)?,
            u64_at(bytes, header + 20)?,
            header + 28,
        )
    };
    if bytes.get(at..at + 2) != Some(&[0, 0]) {
        return Err(bad);
    }
    let count = usize::from(u16::from_be_bytes(
        bytes.get(at + 2..at + 4).ok_or(bad)?.try_into().unwrap(),
    ));
    if count == 0 || count > remaining.min(MAX_SEGMENTS) || bytes.len() != at + 4 + count * 12 {
        return Err(error("dash-index-entry-limit-or-size"));
    }
    let mut start = extent
        .end
        .checked_add(1)
        .and_then(|n| n.checked_add(offset))
        .ok_or(bad)?;
    let mut segments = Vec::with_capacity(count);
    for n in 0..count {
        let at = at + 4 + n * 12;
        let size = u32_at(bytes, at)?;
        let duration = u64::from(u32_at(bytes, at + 4)?);
        let sap = u32_at(bytes, at + 8)?;
        if size >> 31 != 0 {
            return Err(error("dash-hierarchical-index-not-supported"));
        }
        if size == 0
            || duration == 0
            || sap >> 31 != 1
            || !matches!((sap >> 28) & 7, 1 | 2)
            || sap & 0x0fff_ffff != 0
        {
            return Err(bad);
        }
        let end = start
            .checked_add(u64::from(size) - 1)
            .filter(|end| *end < file_bytes)
            .ok_or(error("dash-index-outside-resource"))?;
        segments.push(Segment {
            resource: Resource {
                uri: uri.into(),
                range: Some(ByteRange { start, end }),
            },
            number: n as u64 + 1,
            time,
            duration,
        });
        start = end.checked_add(1).ok_or(bad)?;
        time = time.checked_add(duration).ok_or(bad)?;
    }
    Ok(Index {
        reference_id,
        timescale,
        segments,
    })
}

pub(super) fn read(
    path: &Path,
    extent: ByteRange,
    uri: &str,
    remaining: usize,
    stop: &Stop,
) -> Result<Index, ConversionError> {
    super::checkpoint(Some(stop))?;
    let mut file = files::regular_input(path)?;
    let metadata = file
        .metadata()
        .map_err(|_| error("dash-index-unreadable"))?;
    let identity = FileIdentity::read(&file).map_err(|_| error("dash-index-unreadable"))?;
    let length = usize::try_from(extent.length()?).map_err(|_| error("dash-index-size-limit"))?;
    if length > 16 * 1024 * 1024 || extent.end >= metadata.len() {
        return Err(error("dash-index-size-limit"));
    }
    let mut bytes = Zeroizing::new(vec![0; length]);
    file.seek(SeekFrom::Start(extent.start))
        .map_err(|_| error("dash-index-unreadable"))?;
    file.read_exact(&mut bytes)
        .map_err(|_| error("dash-index-unreadable"))?;
    let result = parse(&bytes, extent, metadata.len(), uri, remaining)?;
    if FileIdentity::read(&files::regular_input(path)?)
        .map_err(|_| error("dash-index-unreadable"))?
        != identity
        || file
            .metadata()
            .map_err(|_| error("dash-index-unreadable"))?
            .len()
            != metadata.len()
    {
        return Err(error("source-changed"));
    }
    super::checkpoint(Some(stop))?;
    Ok(result)
}

/// Every media box must belong to one complete indexed range; gaps may contain padding only.
pub(super) fn check_layout(
    path: &Path,
    initialization: ByteRange,
    index: ByteRange,
    segments: &[Segment],
    stop: &Stop,
) -> Result<(), ConversionError> {
    let bad = error("dash-index-resource-layout-mismatch");
    let mut file = files::regular_input(path)?;
    let length = file.metadata().map_err(|_| bad)?.len();
    let identity = FileIdentity::read(&file).map_err(|_| bad)?;
    if initialization.start != 0 || initialization.end >= index.start || index.end >= length {
        return Err(bad);
    }
    let mut at = 0_u64;
    let mut current = 0;
    let mut boxes = 0_usize;
    while at < length {
        super::checkpoint(Some(stop))?;
        boxes += 1;
        if boxes > 1_000_000 {
            return Err(error("dash-index-box-limit"));
        }
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes).map_err(|_| bad)?;
        let small = u32::from_be_bytes(bytes[..4].try_into().unwrap());
        let mut head = 8_u64;
        let size = match small {
            0 => length - at,
            1 => {
                let mut wide = [0; 8];
                file.read_exact(&mut wide).map_err(|_| bad)?;
                head = 16;
                u64::from_be_bytes(wide)
            }
            n => u64::from(n),
        };
        if size < head || size > length - at {
            return Err(bad);
        }
        let end = at + size - 1;
        let kind = &bytes[4..];
        while current < segments.len() && segments[current].resource.range.ok_or(bad)?.end < at {
            current += 1;
        }
        let extent = segments.get(current).and_then(|s| s.resource.range);
        let in_init = at >= initialization.start && end <= initialization.end;
        let in_index = at == index.start && end == index.end && kind == b"sidx";
        let in_media = extent.is_some_and(|r| at >= r.start && end <= r.end);
        if in_init {
            if !matches!(
                kind,
                b"ftyp" | b"moov" | b"pssh" | b"free" | b"skip" | b"wide"
            ) {
                return Err(bad);
            }
        } else if !in_index && !in_media && !matches!(kind, b"free" | b"skip" | b"mfra") {
            return Err(bad);
        }
        if (!in_init && at <= initialization.end)
            || (at < index.start && end >= index.start)
            || (at <= index.end && end > index.end && !in_media)
            || extent.is_some_and(|r| at < r.start && end >= r.start || at <= r.end && end > r.end)
        {
            return Err(bad);
        }
        at += size;
        file.seek(SeekFrom::Start(at)).map_err(|_| bad)?;
    }
    if at != length
        || FileIdentity::read(&files::regular_input(path)?).map_err(|_| bad)? != identity
        || file.metadata().map_err(|_| bad)?.len() != length
    {
        return Err(bad);
    }
    Ok(())
}

pub(super) fn build(
    reference_id: u32,
    timescale: u32,
    earliest: u64,
    items: &[(u64, u64, u8)],
) -> Result<Vec<u8>, ConversionError> {
    let bad = error("dash-index-not-representable");
    if reference_id == 0
        || timescale == 0
        || items.is_empty()
        || items.len() > usize::from(u16::MAX)
    {
        return Err(bad);
    }
    let length = 40 + items.len() * 12;
    let mut bytes = Vec::with_capacity(length);
    bytes.extend(u32::try_from(length).map_err(|_| bad)?.to_be_bytes());
    bytes.extend(b"sidx");
    bytes.extend([1, 0, 0, 0]);
    bytes.extend(reference_id.to_be_bytes());
    bytes.extend(timescale.to_be_bytes());
    bytes.extend(earliest.to_be_bytes());
    bytes.extend(0_u64.to_be_bytes());
    bytes.extend([0, 0]);
    bytes.extend(u16::try_from(items.len()).map_err(|_| bad)?.to_be_bytes());
    for &(size, duration, sap) in items {
        let size = u32::try_from(size).map_err(|_| bad)?;
        let duration = u32::try_from(duration).map_err(|_| bad)?;
        if size == 0 || size >= 1 << 31 || duration == 0 || !matches!(sap, 1 | 2) {
            return Err(bad);
        }
        bytes.extend(size.to_be_bytes());
        bytes.extend(duration.to_be_bytes());
        bytes.extend((0x8000_0000 | u32::from(sap) << 28).to_be_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_index_round_trip_checks_large_timestamps_and_extents() {
        let bytes = build(
            7,
            90_000,
            u64::from(u32::MAX) + 3,
            &[(100, 540_000, 1), (80, 270_000, 2)],
        )
        .unwrap();
        let range = ByteRange {
            start: 1000,
            end: 1000 + bytes.len() as u64 - 1,
        };
        let parsed = parse(&bytes, range, 2000, "video.mp4", 2).unwrap();
        assert_eq!(parsed.reference_id, 7);
        assert_eq!(parsed.timescale, 90_000);
        assert_eq!(parsed.segments[0].time, u64::from(u32::MAX) + 3);
        assert_eq!(
            parsed.segments[0].resource.range.unwrap().start,
            range.end + 1
        );
        assert_eq!(parsed.segments[1].duration, 270_000);
    }
    #[test]
    fn invalid_and_hierarchical_indexes_are_refused() {
        let bytes = build(1, 1000, 0, &[(100, 6000, 1)]).unwrap();
        let range = ByteRange {
            start: 100,
            end: 100 + bytes.len() as u64 - 1,
        };
        for at in [8, 38, 40, 48] {
            let mut bad = bytes.clone();
            bad[at] ^= 0x80;
            assert!(parse(&bad, range, 1000, "media.mp4", 2).is_err(), "{at}");
        }
        let mut zero_scale = bytes.clone();
        zero_scale[16..20].fill(0);
        assert!(parse(&zero_scale, range, 1000, "media.mp4", 2).is_err());
        assert!(parse(&bytes, range, range.end + 50, "media.mp4", 2).is_err());
        assert!(parse(&bytes, range, 1000, "media.mp4", 0).is_err());
    }
}
