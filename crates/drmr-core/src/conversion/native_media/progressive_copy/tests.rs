use super::*;
use crate::conversion::{ContentKey, native_media};
use crate::mp4::Mp4Box;
use std::{fs, ops::Range};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
fn keys() -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap()
}
fn co64(node: &Mp4Box, bytes: &[u8]) -> Vec<u8> {
    let boxed = native_media::test_files::boxed;
    if node.kind() == *b"stco" {
        let source = node.header.payload(bytes);
        let mut payload = source[..8].to_vec();
        for value in source[8..].as_chunks::<4>().0 {
            payload.extend(u64::from(u32::from_be_bytes(*value)).to_be_bytes());
        }
        return boxed(*b"co64", &payload);
    }
    if node.children.is_empty() {
        return node.header.bytes(bytes).to_vec();
    }
    let mut payload = Vec::new();
    let mut at = node.header.offset + node.header.header_size;
    for child in &node.children {
        payload.extend(&bytes[at..child.header.offset]);
        payload.extend(co64(child, bytes));
        at = child.header.offset + child.header.size;
    }
    payload.extend(&bytes[at..node.header.offset + node.header.size]);
    boxed(node.kind(), &payload)
}

/// Rebuild test framing independently of the writer, including absolute auxiliary offsets.
fn relocated(source: &[u8], movie_first: bool, wide_offsets: bool) -> Vec<u8> {
    let nodes = mp4::parse_boxes(source).unwrap();
    let file_type = read::one(&nodes, *b"ftyp").unwrap().header.bytes(source);
    let original_movie = read::one(&nodes, *b"moov").unwrap();
    let mut movie = if wide_offsets {
        co64(original_movie, source)
    } else {
        original_movie.header.bytes(source).to_vec()
    };
    let media = nodes
        .iter()
        .filter(|node| node.kind() == *b"mdat")
        .collect::<Vec<_>>();
    assert!(!media.is_empty());
    let movie_at = if movie_first {
        file_type.len()
    } else {
        file_type.len() + media.iter().map(|node| node.header.size).sum::<usize>()
    };
    let mut media_at = file_type.len() + if movie_first { movie.len() } else { 0 };
    let mut ranges = Vec::<(Range<u64>, u64)>::new();
    for node in &media {
        ranges.push((
            (node.header.offset + node.header.header_size) as u64
                ..(node.header.offset + node.header.size) as u64,
            (media_at + node.header.header_size) as u64,
        ));
        media_at += node.header.size;
    }
    let movie_nodes = mp4::parse_boxes(&movie).unwrap();
    for node in mp4::walk(&movie_nodes) {
        if matches!(&node.kind(), b"stco" | b"co64") {
            let width = if node.kind() == *b"stco" { 4 } else { 8 };
            let at = node.header.offset + node.header.header_size + 8;
            for value in movie[at..node.header.offset + node.header.size].chunks_exact_mut(width) {
                let old = if width == 4 {
                    u64::from(u32::from_be_bytes(value.try_into().unwrap()))
                } else {
                    u64::from_be_bytes(value.try_into().unwrap())
                };
                let (range, new) = ranges
                    .iter()
                    .find(|(range, _)| range.contains(&old))
                    .unwrap();
                let offset = new + old - range.start;
                if width == 4 {
                    value.copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
                } else {
                    value.copy_from_slice(&offset.to_be_bytes());
                }
            }
        }
    }
    for track in movie_nodes[0]
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
    {
        let nodes = mp4::walk(&track.children);
        if let Some(saio) = nodes.iter().find(|node| node.kind() == *b"saio") {
            let senc = nodes.iter().find(|node| node.kind() == *b"senc").unwrap();
            let first = movie_at + senc.header.offset + senc.header.header_size + 8;
            let at = saio.header.offset + saio.header.header_size;
            match movie[at] {
                0 => movie[at + 8..at + 12]
                    .copy_from_slice(&u32::try_from(first).unwrap().to_be_bytes()),
                1 => movie[at + 8..at + 16].copy_from_slice(&(first as u64).to_be_bytes()),
                _ => panic!("fixture auxiliary version"),
            }
        }
    }
    let mut output = file_type.to_vec();
    if movie_first {
        output.extend(&movie);
    }
    for node in media {
        output.extend(node.header.bytes(source));
    }
    if !movie_first {
        output.extend(movie);
    }
    output
}

fn evidence(
    path: &Path,
    keys: &InputKeys,
) -> (
    native_media::Movie,
    Vec<(native_media::SampleInfo, [u8; 32])>,
) {
    let mut samples = Vec::new();
    let mut sample = None;
    let mut digest = Sha256::new();
    let mut next = 0_u32;
    let mut movie = native_media::scan(path, keys, &Stop::new(), &mut |_, info, bytes| {
        assert!(!bytes.is_empty() && bytes.len() <= CHUNK_BYTES);
        if info.chunk_offset == 0 {
            assert!(sample.is_none());
            sample = Some(info.clone());
            digest = Sha256::new();
            next = 0;
        }
        assert_eq!(info.chunk_offset, next);
        next += u32::try_from(bytes.len()).unwrap();
        digest.update(bytes);
        if info.is_last_chunk {
            assert_eq!(next, info.size);
            let mut info = sample.take().unwrap();
            info.protection = native_media::SampleProtection::Clear;
            info.is_last_chunk = true;
            samples.push((info, digest.clone().finalize().into()));
        }
        Ok(())
    })
    .unwrap();
    assert!(sample.is_none());
    for track in &mut movie.tracks {
        track.protection = native_media::Protection {
            scheme: native_media::Scheme::Clear,
            key_id: None,
            per_sample_iv_size: 0,
            crypt_byte_block: 0,
            skip_byte_block: 0,
        };
    }
    (movie, samples)
}

#[test]
fn both_movie_positions_and_chunk_offset_widths_retain_all_original_samples_and_controls() {
    let stage = crate::conversion::workspace().unwrap();
    for name in [
        "cenc-iv8.mp4",
        "cenc-iv16.mp4",
        "cbcs-joint.mp4",
        "cbcs-audio.mp4",
    ] {
        let original = fs::read(fixture(name)).unwrap();
        for movie_first in [false, true] {
            for wide_offsets in [false, true] {
                let bytes = relocated(&original, movie_first, wide_offsets);
                let input = stage
                    .path()
                    .join(format!("{name}-{movie_first}-{wide_offsets}-input.mp4"));
                let output = stage
                    .path()
                    .join(format!("{name}-{movie_first}-{wide_offsets}-clear.mp4"));
                crate::conversion::files::create_new(&input)
                    .unwrap()
                    .write_all(&bytes)
                    .unwrap();
                let report = crate::inspection::inspect(&input).unwrap();
                assert!(
                    crate::conversion::progressive::validate_input(&input, &report, &keys())
                        .unwrap()
                );
                let before = evidence(&input, &keys());
                copy_progressive(&input, &output, &keys(), &Stop::new()).unwrap();
                let after = evidence(&output, &InputKeys::default());
                assert_eq!(before.0, after.0);
                assert_eq!(before.1.len(), after.1.len());
                for (index, (original, clear)) in before.1.iter().zip(&after.1).enumerate() {
                    assert_eq!(
                        original, clear,
                        "{name}, front={movie_first}, co64={wide_offsets}, sample={index}"
                    );
                }
                assert_eq!(fs::metadata(&output).unwrap().len(), bytes.len() as u64);
                assert_eq!(fs::read(&input).unwrap(), bytes);
                crate::conversion::cenc::clear(&crate::conversion::audio::movie(&output).unwrap())
                    .unwrap();
            }
        }
    }
}

#[test]
fn cancellation_and_existing_destination_never_overwrite_a_file() {
    let stage = crate::conversion::workspace().unwrap();
    let output = stage.path().join("clear.mp4");
    let stop = Stop::new();
    stop.set();
    assert_eq!(
        copy_progressive(&fixture("cenc-iv8.mp4"), &output, &keys(), &stop).unwrap_err(),
        ConversionError("cancelled")
    );
    assert!(!output.exists());
    crate::conversion::files::create_new(&output)
        .unwrap()
        .write_all(b"retained")
        .unwrap();
    assert!(copy_progressive(&fixture("cenc-iv8.mp4"), &output, &keys(), &Stop::new()).is_err());
    assert_eq!(fs::read(output).unwrap(), b"retained");
}
