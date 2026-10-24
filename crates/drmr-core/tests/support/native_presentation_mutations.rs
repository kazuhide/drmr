//! Legal presentation-only mutations must not pass encoded-content preservation checks.
use drmr_core::mp4::{self, Mp4Box};

pub fn rotated(data: &[u8]) -> Vec<u8> {
    let nodes = mp4::parse_boxes(data).unwrap();
    let header = mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tkhd")
        .unwrap();
    let payload = header.header.payload(data);
    let matrix = match payload[0] {
        0 => 40,
        1 => 52,
        _ => panic!("fixture track header version"),
    };
    let at = header.header.offset + header.header.header_size + matrix;
    let mut changed = data.to_vec();
    for (index, value) in [0_i32, 0x10000, 0, -0x10000, 0, 0, 0, 0, 0x4000_0000]
        .into_iter()
        .enumerate()
    {
        changed[at + index * 4..at + (index + 1) * 4].copy_from_slice(&value.to_be_bytes());
    }
    changed
}
fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend(kind);
    bytes.extend(payload);
    bytes
}
fn window(node: &Mp4Box, data: &[u8], edit: &[u8]) -> Vec<u8> {
    if !matches!(&node.kind(), b"moov" | b"trak") {
        return node.header.bytes(data).to_vec();
    }
    let mut payload = Vec::new();
    for child in &node.children {
        if child.kind() != *b"edts" {
            payload.extend(window(child, data, edit));
        }
    }
    if node.kind() == *b"trak" {
        payload.extend(edit);
    }
    boxed(node.kind(), &payload)
}
pub fn cropped_window(data: &[u8]) -> Vec<u8> {
    let nodes = mp4::parse_boxes(data).unwrap();
    let movie = mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"mvhd")
        .unwrap();
    let payload = movie.header.payload(data);
    let at = match payload[0] {
        0 => 12,
        1 => 20,
        _ => panic!("fixture movie header version"),
    };
    let scale = u32::from_be_bytes(payload[at..at + 4].try_into().unwrap());
    let mut edit = vec![1, 0, 0, 0];
    edit.extend(1_u32.to_be_bytes());
    edit.extend((u64::from(scale) * 2).to_be_bytes());
    edit.extend(0_i64.to_be_bytes());
    edit.extend([0, 1, 0, 0]);
    let edit = boxed(*b"edts", &boxed(*b"elst", &edit));
    nodes
        .iter()
        .flat_map(|node| window(node, data, &edit))
        .collect()
}

pub fn shifted_open_window(data: &[u8]) -> Vec<u8> {
    let nodes = mp4::parse_boxes(data).unwrap();
    let edit = mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"elst")
        .unwrap();
    let payload = edit.header.payload(data);
    let mut changed = data.to_vec();
    let start = edit.header.offset + edit.header.header_size;
    match payload[0] {
        0 => {
            assert_eq!(&payload[8..12], &[0; 4]);
            let old = i32::from_be_bytes(payload[12..16].try_into().unwrap());
            changed[start + 12..start + 16].copy_from_slice(&(old + 1).to_be_bytes());
        }
        1 => {
            assert_eq!(&payload[8..16], &[0; 8]);
            let old = i64::from_be_bytes(payload[16..24].try_into().unwrap());
            changed[start + 16..start + 24].copy_from_slice(&(old + 1).to_be_bytes());
        }
        _ => panic!("fixture edit version"),
    }
    changed
}

pub fn without_edits(data: &[u8]) -> Vec<u8> {
    mp4::parse_boxes(data)
        .unwrap()
        .iter()
        .flat_map(|node| window(node, data, &[]))
        .collect()
}

fn wrong_mehd(node: &Mp4Box, data: &[u8]) -> Vec<u8> {
    if !matches!(&node.kind(), b"moov" | b"mvex") {
        return node.header.bytes(data).to_vec();
    }
    let mut payload = Vec::new();
    for child in &node.children {
        if child.kind() != *b"mehd" {
            payload.extend(wrong_mehd(child, data));
        }
    }
    if node.kind() == *b"mvex" {
        payload.extend(boxed(*b"mehd", &[0, 0, 0, 0, 0, 0, 0, 1]));
    }
    boxed(node.kind(), &payload)
}

pub fn wrong_header_durations(data: &[u8]) -> Vec<Vec<u8>> {
    let nodes = mp4::parse_boxes(data).unwrap();
    let all = mp4::walk(&nodes);
    let mut variants = Vec::new();
    for kind in [*b"mvhd", *b"tkhd", *b"mdhd"] {
        let header = all.iter().find(|node| node.kind() == kind).unwrap();
        let payload = header.header.payload(data);
        let (at, width) = match (kind, payload[0]) {
            (k, 0) if k == *b"tkhd" => (20, 4),
            (_, 0) => (16, 4),
            (k, 1) if k == *b"tkhd" => (28, 8),
            (_, 1) => (24, 8),
            _ => panic!("fixture duration version"),
        };
        let start = header.header.offset + header.header.header_size + at;
        let mut changed = data.to_vec();
        changed[start..start + width].fill(0);
        changed[start + width - 1] = 1;
        assert_ne!(changed, data);
        variants.push(changed);
    }
    variants.push(
        nodes
            .iter()
            .flat_map(|node| wrong_mehd(node, data))
            .collect(),
    );
    variants
}

pub fn changed_track_controls(data: &[u8]) -> Vec<Vec<u8>> {
    let nodes = mp4::parse_boxes(data).unwrap();
    let all = mp4::walk(&nodes);
    let header = all.iter().find(|node| node.kind() == *b"tkhd").unwrap();
    let payload = header.header.payload(data);
    let start = header.header.offset + header.header.header_size;
    let mut flags = data.to_vec();
    flags[start + 3] ^= 8;
    let mut layer = data.to_vec();
    let at = if payload[0] == 0 { 32 } else { 44 };
    layer[start + at..start + at + 2].copy_from_slice(&1_i16.to_be_bytes());
    let mut variants = vec![flags, layer];
    let media = all
        .iter()
        .find(|node| matches!(&node.kind(), b"smhd" | b"vmhd"))
        .unwrap();
    let start = media.header.offset + media.header.header_size;
    let mut changed = data.to_vec();
    let value: u16 = if media.kind() == *b"smhd" {
        0x0100
    } else {
        0x0020
    };
    changed[start + 4..start + 6].copy_from_slice(&value.to_be_bytes());
    variants.push(changed);
    variants
}

fn with_recovery(node: &Mp4Box, data: &[u8], group: &[u8]) -> Vec<u8> {
    if !matches!(
        &node.kind(),
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl"
    ) {
        return node.header.bytes(data).to_vec();
    }
    let mut payload: Vec<u8> = node
        .children
        .iter()
        .flat_map(|node| with_recovery(node, data, group))
        .collect();
    if node.kind() == *b"stbl" {
        payload.extend(group);
    }
    boxed(node.kind(), &payload)
}

pub fn changed_recovery_groups(data: &[u8]) -> Vec<Vec<u8>> {
    let nodes = mp4::parse_boxes(data).unwrap();
    [*b"roll", *b"prol"]
        .into_iter()
        .map(|kind| {
            let mut payload = vec![2, 0, 0, 0];
            payload.extend(kind);
            payload.extend(2_u32.to_be_bytes());
            payload.extend(1_u32.to_be_bytes());
            payload.extend(1_u32.to_be_bytes());
            payload.extend((-1_i16).to_be_bytes());
            let group = boxed(*b"sgpd", &payload);
            nodes
                .iter()
                .flat_map(|node| with_recovery(node, data, &group))
                .collect()
        })
        .collect()
}
