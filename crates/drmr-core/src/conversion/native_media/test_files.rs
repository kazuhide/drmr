//! Small structural fixtures exercise ranges independently of codec bitstream admission.
use super::{init, read};
use crate::mp4::{self, Mp4Box};

pub(super) fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend(kind);
    bytes.extend(payload);
    bytes
}
fn full(kind: [u8; 4], flags: u32, values: &[u32]) -> Vec<u8> {
    let mut payload = flags.to_be_bytes().to_vec();
    for value in values {
        payload.extend(value.to_be_bytes());
    }
    boxed(kind, &payload)
}
fn sample_table(source: &Mp4Box, data: &[u8], size: Option<u32>, offset: u64) -> Vec<u8> {
    let stsd = read::one(&source.children, *b"stsd")
        .unwrap()
        .header
        .bytes(data)
        .to_vec();
    let (time, chunks, sizes, offsets) = if let Some(size) = size {
        let mut offsets = vec![0; 4];
        offsets.extend(1_u32.to_be_bytes());
        offsets.extend(offset.to_be_bytes());
        (
            full(*b"stts", 0, &[1, 1, 100]),
            full(*b"stsc", 0, &[1, 1, 1, 1]),
            full(*b"stsz", 0, &[size, 1]),
            boxed(*b"co64", &offsets),
        )
    } else {
        (
            full(*b"stts", 0, &[0]),
            full(*b"stsc", 0, &[0]),
            full(*b"stsz", 0, &[0, 0]),
            full(*b"co64", 0, &[0]),
        )
    };
    boxed(*b"stbl", &[stsd, time, chunks, sizes, offsets].concat())
}
fn track(source: &Mp4Box, data: &[u8], size: Option<u32>, offset: u64) -> Vec<u8> {
    let tkhd = read::one(&source.children, *b"tkhd")
        .unwrap()
        .header
        .bytes(data);
    let media_container = read::one(&source.children, *b"mdia").unwrap();
    let mdhd = read::one(&media_container.children, *b"mdhd").unwrap();
    let mut clock = mdhd.header.bytes(data).to_vec();
    assert_eq!(clock[mdhd.header.header_size], 0);
    let at = mdhd.header.header_size + 16;
    clock[at..at + 4].copy_from_slice(&if size.is_some() { 100_u32 } else { 0 }.to_be_bytes());
    let handler = read::one(&media_container.children, *b"hdlr")
        .unwrap()
        .header
        .bytes(data);
    let minf = read::one(&media_container.children, *b"minf").unwrap();
    let table = read::one(&minf.children, *b"stbl").unwrap();
    let mut children = minf
        .children
        .iter()
        .filter(|node| node.kind() != *b"stbl")
        .flat_map(|node| node.header.bytes(data).iter().copied())
        .collect::<Vec<_>>();
    children.extend(sample_table(table, data, size, offset));
    let media = boxed(
        *b"mdia",
        &[clock, handler.to_vec(), boxed(*b"minf", &children)].concat(),
    );
    boxed(*b"trak", &[tkhd, &media].concat())
}
fn movie(layout: &read::Layout, size: Option<u32>, offset: u64, fragmented: bool) -> Vec<u8> {
    let nodes = mp4::parse_boxes(&layout.movie).unwrap();
    let moov = &nodes[0];
    let mut payload = read::one(&moov.children, *b"mvhd")
        .unwrap()
        .header
        .bytes(&layout.movie)
        .to_vec();
    let tracks = moov
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .collect::<Vec<_>>();
    for node in tracks
        .iter()
        .take(if fragmented { tracks.len() } else { 1 })
    {
        payload.extend(track(node, &layout.movie, size, offset));
    }
    if fragmented {
        let init = init::parse(layout).unwrap();
        let mut defaults = Vec::new();
        for track in &init.tracks {
            defaults.extend(full(
                *b"trex",
                0,
                &[track.descriptor.track_id, 1, 100, 3, 0],
            ));
        }
        payload.extend(boxed(*b"mvex", &defaults));
    }
    boxed(*b"moov", &payload)
}
pub(super) fn large_sample(layout: &read::Layout, size: u32) -> Vec<u8> {
    let preliminary = movie(layout, Some(size), 0, false);
    let mut bytes = movie(
        layout,
        Some(size),
        u64::try_from(preliminary.len() + 8).unwrap(),
        false,
    );
    bytes.extend(boxed(*b"mdat", &vec![0x5a; usize::try_from(size).unwrap()]));
    bytes
}
pub(super) fn interleaved_fragments(layout: &read::Layout, duplicate_header: bool) -> Vec<u8> {
    let init = init::parse(layout).unwrap();
    assert_eq!(init.tracks.len(), 2);
    let ids = [
        init.tracks[0].descriptor.track_id,
        init.tracks[1].descriptor.track_id,
        init.tracks[0].descriptor.track_id,
    ];
    let make = |offset: u32| {
        let mut payload = full(*b"mfhd", 0, &[1]);
        if duplicate_header {
            payload.extend(full(*b"mfhd", 0, &[1]));
        }
        for (index, id) in ids.into_iter().enumerate() {
            let data = offset + u32::try_from(index * 3).unwrap();
            let child = [
                full(*b"tfhd", 0x0002_0000, &[id]),
                full(*b"tfdt", 0, &[if index == 2 { 100 } else { 0 }]),
                full(*b"trun", 1, &[1, data]),
            ]
            .concat();
            payload.extend(boxed(*b"traf", &child));
        }
        boxed(*b"moof", &payload)
    };
    let preliminary = make(0);
    let fragment = make(u32::try_from(preliminary.len() + 8).unwrap());
    [
        movie(layout, None, 0, true),
        fragment,
        boxed(*b"mdat", b"v01a01v02"),
    ]
    .concat()
}

/// Insert a control while leaving the clear fixture's sample tables and media offsets intact.
fn with_track_control(data: &[u8], container: [u8; 4], control: &[u8]) -> Vec<u8> {
    fn rewrite(node: &Mp4Box, data: &[u8], track: usize, target: usize, control: &[u8]) -> Vec<u8> {
        if node.kind() != *b"moov" && node.header.offset != track && node.header.offset != target {
            return node.header.bytes(data).to_vec();
        }
        let mut payload: Vec<u8> = node
            .children
            .iter()
            .flat_map(|child| rewrite(child, data, track, target, control))
            .collect();
        if node.header.offset == target {
            payload.extend(control);
        }
        boxed(node.kind(), &payload)
    }
    let nodes = mp4::parse_boxes(data).unwrap();
    let movie = read::one(&nodes, *b"moov").unwrap();
    let track = movie
        .children
        .iter()
        .find(|node| node.kind() == *b"trak")
        .unwrap();
    let media = read::one(&track.children, *b"mdia").unwrap();
    assert_eq!(
        read::one(&media.children, *b"hdlr")
            .unwrap()
            .header
            .payload(data)
            .get(8..12),
        Some(b"vide".as_slice())
    );
    let target = match &container {
        b"trak" => track,
        b"mdia" => media,
        _ => panic!("fixture mutation target"),
    };
    let replacement = rewrite(
        movie,
        data,
        track.header.offset,
        target.header.offset,
        control,
    );
    // Keep the old moov's size occupied by padding, so no mdat offset changes.
    let padding = boxed(*b"free", &vec![0; movie.header.size - 8]);
    let mut changed = data.to_vec();
    changed[movie.header.offset..movie.header.offset + movie.header.size].copy_from_slice(&padding);
    changed.extend(replacement);
    for media in nodes.iter().filter(|node| node.kind() == *b"mdat") {
        assert_eq!(media.header.bytes(&changed), media.header.bytes(data));
    }
    changed
}

pub(super) fn with_track_aperture(data: &[u8]) -> Vec<u8> {
    let mut dimensions = vec![0; 4];
    dimensions.extend((160_u32 << 16).to_be_bytes());
    dimensions.extend((90_u32 << 16).to_be_bytes());
    with_track_control(
        data,
        *b"trak",
        &boxed(*b"tapt", &boxed(*b"clef", &dimensions)),
    )
}

pub(super) fn with_extended_language(data: &[u8]) -> Vec<u8> {
    let payload = [&[0; 4][..], b"en-US\0"].concat();
    with_track_control(data, *b"mdia", &boxed(*b"elng", &payload))
}
