//! Exact resource slices for record validation in the already captured DASH graph.
use super::{
    ConversionError, MAX_SEGMENTS, Node, Path, PathBuf, addressing, error, index, reference,
};
use crate::concurrency::Stop;
use std::{
    collections::BTreeMap,
    io::{Read as _, Seek as _, SeekFrom},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::conversion) struct RecordResource {
    pub path: PathBuf,
    pub range: Option<(u64, u64)>,
}

/// Indexed objects expose their finite segment counts only after their index bytes arrive.
pub(in crate::conversion) fn check_acquired_limits(
    root: &Path,
    projection: &[u8],
    max_segments_per_track: usize,
    stop: &Stop,
) -> Result<(), ConversionError> {
    super::checkpoint(Some(stop))?;
    super::source_references(projection)?;
    let text = std::str::from_utf8(projection).map_err(|_| error("dash-invalid-xml"))?;
    let document = super::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let root = root
        .canonicalize()
        .map_err(|_| error("dash-resource-unreadable"))?;
    let mut remaining = MAX_SEGMENTS;
    for rep in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
    {
        super::checkpoint(Some(stop))?;
        let resolved = addressing::resolve(rep, remaining.min(max_segments_per_track))?;
        let count = if let Some(resource) = &resolved.index {
            let path = reference(&root, &resolved.base, &resource.uri)?;
            index::read(
                &path,
                resource.range.ok_or(error("dash-index-range-required"))?,
                &resource.uri,
                remaining.min(max_segments_per_track),
                stop,
            )?
            .segments
            .len()
        } else {
            resolved.segments.len()
        };
        remaining = remaining
            .checked_sub(count)
            .ok_or(error("dash-segment-limit"))?;
    }
    Ok(())
}

pub(in crate::conversion) fn record_resources(
    root: &Path,
    rep: Node<'_, '_>,
    stop: &Stop,
) -> Result<(RecordResource, Vec<RecordResource>), ConversionError> {
    let root = root
        .canonicalize()
        .map_err(|_| error("dash-resource-unreadable"))?;
    let resolved = addressing::resolve(rep, MAX_SEGMENTS)?;
    let object = |resource: &addressing::Resource| -> Result<RecordResource, ConversionError> {
        Ok(RecordResource {
            path: reference(&root, &resolved.base, &resource.uri)?,
            range: resource.range.map(|range| (range.start, range.end)),
        })
    };
    let initialization = object(&resolved.initialization)?;
    let segments = if let Some(resource) = &resolved.index {
        let path = reference(&root, &resolved.base, &resource.uri)?;
        let range = resource.range.ok_or(error("dash-index-range-required"))?;
        let index = index::read(&path, range, &resource.uri, MAX_SEGMENTS, stop)?;
        index::check_layout(
            &path,
            resolved
                .initialization
                .range
                .ok_or(error("dash-initialization-range-required"))?,
            range,
            &index.segments,
            stop,
        )?;
        index.segments
    } else {
        resolved.segments.clone()
    };
    let media = segments
        .iter()
        .map(|segment| object(&segment.resource))
        .collect::<Result<Vec<_>, _>>()?;
    if resolved.index.is_none() {
        check_list_ranges(&initialization, &media, stop)?;
    }
    Ok((initialization, media))
}

fn check_list_ranges(
    initialization: &RecordResource,
    media: &[RecordResource],
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut files = BTreeMap::<PathBuf, Vec<(addressing::ByteRange, bool)>>::new();
    for (resource, init) in std::iter::once((initialization, true))
        .chain(media.iter().map(|resource| (resource, false)))
    {
        if let Some((start, end)) = resource.range {
            files
                .entry(resource.path.clone())
                .or_default()
                .push((addressing::ByteRange { start, end }, init));
        }
    }
    for (path, mut ranges) in files {
        super::checkpoint(Some(stop))?;
        ranges.sort_by_key(|(range, _)| range.start);
        let bad = error("dash-resource-range-layout-mismatch");
        let mut file = crate::conversion::files::regular_input(&path)?;
        let length = file.metadata().map_err(|_| bad)?.len();
        let identity = crate::native::FileIdentity::read(&file).map_err(|_| bad)?;
        if ranges.last().is_none_or(|(range, _)| range.end >= length)
            || ranges
                .windows(2)
                .any(|pair| pair[0].0.end >= pair[1].0.start)
        {
            return Err(bad);
        }
        let mut at = 0_u64;
        let mut selected = 0;
        let mut boxes = 0_u64;
        while at < length {
            super::checkpoint(Some(stop))?;
            boxes += 1;
            if boxes > 1_000_000 {
                return Err(error("dash-index-box-limit"));
            }
            let mut header = [0; 8];
            file.read_exact(&mut header).map_err(|_| bad)?;
            let mut width = 8;
            let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
                0 => length - at,
                1 => {
                    let mut wide = [0; 8];
                    file.read_exact(&mut wide).map_err(|_| bad)?;
                    width = 16;
                    u64::from_be_bytes(wide)
                }
                size => u64::from(size),
            };
            if size < width || size > length - at {
                return Err(bad);
            }
            let end = at + size - 1;
            while selected < ranges.len() && ranges[selected].0.end < at {
                selected += 1;
            }
            if let Some((range, init)) = ranges
                .get(selected)
                .filter(|(range, _)| range.start <= at && end <= range.end)
            {
                if *init
                    && !matches!(
                        &header[4..],
                        b"ftyp" | b"moov" | b"pssh" | b"free" | b"skip" | b"wide"
                    )
                {
                    return Err(bad);
                }
                if at < range.start || end > range.end {
                    return Err(bad);
                }
            } else {
                if ranges.get(selected).is_some_and(|(range, _)| {
                    at < range.start && end >= range.start || at <= range.end && end > range.end
                }) {
                    return Err(bad);
                }
                if &header[4..] == b"sidx" {
                    index::read(
                        &path,
                        addressing::ByteRange { start: at, end },
                        "media.mp4",
                        MAX_SEGMENTS,
                        stop,
                    )?;
                } else if !matches!(&header[4..], b"free" | b"skip" | b"mfra") {
                    return Err(bad);
                }
            }
            at += size;
            file.seek(SeekFrom::Start(at)).map_err(|_| bad)?;
        }
        if file.metadata().map_err(|_| bad)?.len() != length
            || crate::native::FileIdentity::read(&crate::conversion::files::regular_input(&path)?)
                .map_err(|_| bad)?
                != identity
        {
            return Err(error("source-changed"));
        }
    }
    Ok(())
}
