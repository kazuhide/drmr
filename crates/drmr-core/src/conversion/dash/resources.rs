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
    check_acquired_limits_admitted(
        root,
        projection,
        max_segments_per_track,
        stop,
        &mut addressing::Admission::local(),
    )
}

fn check_acquired_limits_admitted(
    root: &Path,
    projection: &[u8],
    max_segments_per_track: usize,
    stop: &Stop,
    admission: &mut addressing::Admission<'_>,
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
        let resolved =
            addressing::resolve_admitted(rep, remaining.min(max_segments_per_track), admission)?;
        let count = if let Some(resource) = &resolved.index {
            let path = reference(&root, &resolved.base, &resource.uri)?;
            index::read_admitted(
                &path,
                resource.range.ok_or(error("dash-index-range-required"))?,
                &resource.uri,
                remaining.min(max_segments_per_track),
                stop,
                admission,
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
    admission: &mut addressing::Admission<'_>,
) -> Result<(RecordResource, Vec<RecordResource>), ConversionError> {
    let root = root
        .canonicalize()
        .map_err(|_| error("dash-resource-unreadable"))?;
    let resolved = addressing::resolve_admitted(rep, MAX_SEGMENTS, admission)?;
    admit_records(admission, 1)?;
    let initialization = record_object(&root, &resolved.base, &resolved.initialization, admission)?;
    let media = if let Some(resource) = &resolved.index {
        let path = reference(&root, &resolved.base, &resource.uri)?;
        let range = resource.range.ok_or(error("dash-index-range-required"))?;
        let index =
            index::read_admitted(&path, range, &resource.uri, MAX_SEGMENTS, stop, admission)?;
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
        admit_records(admission, index.segments.len())?;
        admit_path(admission, &path, index.segments.len())?;
        index
            .segments
            .into_iter()
            .map(|segment| RecordResource {
                path: path.clone(),
                range: segment.resource.range.map(|range| (range.start, range.end)),
            })
            .collect()
    } else {
        admit_records(admission, resolved.segments.len())?;
        resolved
            .segments
            .iter()
            .map(|segment| record_object(&root, &resolved.base, &segment.resource, admission))
            .collect::<Result<Vec<_>, _>>()?
    };
    if resolved.index.is_none() {
        check_list_ranges(&initialization, &media, stop, admission)?;
    }
    Ok((initialization, media))
}

fn admit_records(
    admission: &mut addressing::Admission<'_>,
    count: usize,
) -> Result<(), ConversionError> {
    // The ordered array and retained B-tree records may coexist during insertion.
    admission.charge(
        count
            .checked_mul(std::mem::size_of::<RecordResource>())
            .and_then(|bytes| bytes.checked_mul(4))
            .ok_or(error("source-graph-limit"))?,
    )
}

fn admit_path(
    admission: &mut addressing::Admission<'_>,
    path: &PathBuf,
    count: usize,
) -> Result<(), ConversionError> {
    // Canonical roots can be much longer than the manifest URI. Pay their actual
    // allocation before cloning, including a simultaneous resource lookup key.
    admission.charge(
        path.capacity()
            .checked_mul(count)
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(error("source-graph-limit"))?,
    )
}

fn record_object(
    root: &Path,
    base: &Path,
    resource: &addressing::Resource,
    admission: &mut addressing::Admission<'_>,
) -> Result<RecordResource, ConversionError> {
    let path = reference(root, base, &resource.uri)?;
    admit_path(admission, &path, 1)?;
    Ok(RecordResource {
        path,
        range: resource.range.map(|range| (range.start, range.end)),
    })
}

fn check_list_ranges(
    initialization: &RecordResource,
    media: &[RecordResource],
    stop: &Stop,
    admission: &mut addressing::Admission<'_>,
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
                    index::read_admitted(
                        &path,
                        addressing::ByteRange { start: at, end },
                        "media.mp4",
                        MAX_SEGMENTS,
                        stop,
                        admission,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Write as _};

    fn indexed_sources(owner: &crate::conversion::files::Stage) -> String {
        let index = index::build(1, 1000, 0, &[(8, 1000, 1)]).unwrap();
        let index_end = 7 + index.len();
        let mut file = Vec::from(*b"\0\0\0\x08free");
        file.extend(&index);
        file.extend(b"\0\0\0\x08free");
        for name in ["first.mp4", "second.mp4"] {
            crate::conversion::files::create_new(&owner.path().join(name))
                .unwrap()
                .write_all(&file)
                .unwrap();
        }
        let mut xml = String::from(
            "<MPD type='static' mediaPresentationDuration='PT1S'><Period><AdaptationSet>",
        );
        for (id, name) in ["first.mp4", "second.mp4"].into_iter().enumerate() {
            use std::fmt::Write as _;
            write!(xml,"<Representation id='{id}'><BaseURL>{name}</BaseURL><SegmentBase timescale='1000' indexRange='8-{index_end}'><Initialization range='0-7'/></SegmentBase></Representation>").unwrap();
        }
        xml.push_str("</AdaptationSet></Period></MPD>");
        xml
    }

    #[test]
    fn post_acquisition_index_expansion_shares_the_manifest_admission() {
        let owner = crate::conversion::files::Stage::new(None).unwrap();
        let xml = indexed_sources(&owner);
        check_acquired_limits(owner.path(), xml.as_bytes(), 10, &Stop::new()).unwrap();
        let mut retained = 0_usize;
        let mut charge = |bytes: usize| {
            retained += bytes;
            if retained > 4000 {
                Err(error("source-graph-limit"))
            } else {
                Ok(())
            }
        };
        assert_eq!(
            check_acquired_limits_admitted(
                owner.path(),
                xml.as_bytes(),
                10,
                &Stop::new(),
                &mut addressing::Admission::new(error("source-graph-limit"), &mut charge),
            ),
            Err(error("source-graph-limit"))
        );
    }

    #[test]
    fn retained_local_index_records_share_admission_across_representations() {
        let owner = crate::conversion::files::Stage::new(None).unwrap();
        let xml = indexed_sources(&owner);
        let document = super::super::Document::parse(&xml).unwrap();
        let reps: Vec<_> = document
            .descendants()
            .filter(|node| node.has_tag_name("Representation"))
            .collect();
        let retained = Cell::new(0_usize);
        let limit = Cell::new(usize::MAX);
        let mut charge = |bytes: usize| {
            retained.set(retained.get() + bytes);
            if retained.get() > limit.get() {
                Err(error("source-graph-limit"))
            } else {
                Ok(())
            }
        };
        let mut admission = addressing::Admission::new(error("source-graph-limit"), &mut charge);
        let first = record_resources(owner.path(), reps[0], &Stop::new(), &mut admission).unwrap();
        limit.set(retained.get() + 512);
        assert_eq!(
            record_resources(owner.path(), reps[1], &Stop::new(), &mut admission).err(),
            Some(error("source-graph-limit"))
        );
        assert_eq!(first.1.len(), 1);
        assert_eq!(first.1[0].range, Some((60, 67)));
    }

    #[test]
    fn canonical_path_storage_is_admitted_before_record_cloning() {
        let mut admission = addressing::Admission::local();
        admission.index("media.mp4", 15_000).unwrap();
        admit_records(&mut admission, 15_000).unwrap();
        let path = PathBuf::from("a".repeat(5000));
        assert_eq!(
            admit_path(&mut admission, &path, 15_000),
            Err(error("source-graph-limit"))
        );
        assert_eq!(
            admit_path(&mut addressing::Admission::local(), &path, usize::MAX),
            Err(error("source-graph-limit"))
        );
        assert_eq!(
            admit_records(&mut addressing::Admission::local(), usize::MAX),
            Err(error("source-graph-limit"))
        );
    }
}
