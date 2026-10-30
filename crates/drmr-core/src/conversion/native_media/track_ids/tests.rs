use super::*;
use crate::conversion::{
    InputKeys,
    native_media::{self, read, test_files},
};
use sha2::{Digest as _, Sha256};
use std::path::Path;

fn full(kind: [u8; 4], flags: u32, values: &[u32]) -> Vec<u8> {
    let mut bytes = flags.to_be_bytes().to_vec();
    for n in values {
        bytes.extend(n.to_be_bytes());
    }
    test_files::boxed(kind, &bytes)
}
fn fixture(mode: &str) -> Vec<u8> {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let stop = Stop::new();
    let mut reader = read::Reader::open(&clip, &stop, false).unwrap();
    let layout = reader.layout().unwrap();
    let template = test_files::interleaved_fragments(&layout, false);
    let nodes = mp4::parse_boxes(&template).unwrap();
    let mut movie = nodes[0].header.bytes(&template).to_vec();
    for node in mp4::walk(&mp4::parse_boxes(&movie).unwrap()) {
        let field = match &node.kind() {
            b"tkhd" => 12,
            b"trex" => 4,
            _ => continue,
        };
        let at = node.header.offset + node.header.header_size + field;
        let old = u32::from_be_bytes(movie[at..at + 4].try_into().unwrap());
        movie[at..at + 4].copy_from_slice(&[7_u32, 42][old as usize - 1].to_be_bytes());
    }
    let make = |offset: u32, absolute: u64| {
        let mut header = if mode == "absolute" {
            let mut payload = 1_u32.to_be_bytes().to_vec();
            payload.extend(7_u32.to_be_bytes());
            payload.extend(absolute.to_be_bytes());
            test_files::boxed(*b"tfhd", &payload)
        } else {
            full(*b"tfhd", if mode == "implicit" { 0 } else { 0x20000 }, &[7])
        };
        header.extend(full(*b"tfdt", 0, &[0]));
        header.extend(full(
            *b"trun",
            1,
            &[1, if mode == "absolute" { 0 } else { offset }],
        ));
        header.extend(full(*b"trun", 0, &[1]));
        let mut children = full(*b"mfhd", 0, &[1]);
        children.extend(test_files::boxed(*b"traf", &header));
        // The second traf uses the previous traf's end as its implicit base.
        children.extend(test_files::boxed(
            *b"traf",
            &[
                full(*b"tfhd", 0, &[42]),
                full(*b"tfdt", 0, &[0]),
                full(*b"trun", 0, &[1]),
            ]
            .concat(),
        ));
        children.extend(test_files::boxed(
            *b"traf",
            &[
                full(*b"tfhd", 0, &[7]),
                full(*b"tfdt", 0, &[200]),
                full(*b"trun", 0, &[1]),
            ]
            .concat(),
        ));
        test_files::boxed(*b"moof", &children)
    };
    let preliminary = make(0, 0);
    let fragment = make(
        u32::try_from(preliminary.len() + 8).unwrap(),
        (movie.len() + preliminary.len() + 8) as u64,
    );
    [
        movie,
        fragment,
        test_files::boxed(*b"mdat", b"v01v02a01v03"),
    ]
    .concat()
}
fn evidence(
    path: &Path,
) -> (
    native_media::Movie,
    Vec<(usize, native_media::SampleInfo, [u8; 32])>,
) {
    let mut samples = Vec::new();
    let movie = native_media::scan(
        path,
        &InputKeys::default(),
        &Stop::new(),
        &mut |track, info, bytes| {
            samples.push((track.index, info.clone(), Sha256::digest(bytes).into()));
            Ok(())
        },
    )
    .unwrap();
    (movie, samples)
}

#[test]
fn identity_rebinding_preserves_multiple_trafs_truns_and_every_native_base_form() {
    for mode in ["explicit", "implicit", "absolute"] {
        let owner = crate::conversion::files::UnpublishedStage::new(None).unwrap();
        let path = owner.path().join("source.mp4");
        let before = fixture(mode);
        let mut file = owner.create_file(&path).unwrap();
        file.write_all(&before).unwrap();
        drop(file);
        let (mut movie, samples) = evidence(&path);
        let mut file = crate::native::open_media_update(&path).unwrap();
        canonicalize_tool_ids(&mut file, &movie, &Stop::new()).unwrap();
        drop(file);
        for (i, track) in movie.tracks.iter_mut().enumerate() {
            track.track_id = u32::try_from(i + 1).unwrap();
        }
        let (actual, after_samples) = evidence(&path);
        assert_eq!(actual, movie);
        assert_eq!(after_samples, samples);
        let mut expected = before.clone();
        for node in mp4::walk(&mp4::parse_boxes(&before).unwrap()) {
            let field = match &node.kind() {
                b"tkhd" => 12,
                b"trex" | b"tfhd" => 4,
                _ => continue,
            };
            let at = node.header.offset + node.header.header_size + field;
            let old = u32::from_be_bytes(before[at..at + 4].try_into().unwrap());
            expected[at..at + 4].copy_from_slice(&if old == 7 { 1_u32 } else { 2 }.to_be_bytes());
        }
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
}

#[test]
fn recognized_index_references_change_without_certifying_or_dropping_opaque_indexes() {
    let owner = crate::conversion::files::UnpublishedStage::new(None).unwrap();
    let path = owner.path().join("source.mp4");
    let mut before = fixture("implicit");
    let matching = full(*b"sidx", 0, &[7, 0xfeed, 0xbeef]);
    let unmatched = full(*b"sidx", 0, &[99, 0xdead]);
    let unknown = full(*b"sidx", 0x0200_0000, &[7, 0x1234]);
    let unknown_flags = full(*b"sidx", 0x0000_0001, &[7, 0x1234]);
    let known_tfra = full(*b"tfra", 0x0100_0000, &[42, 0x55, 0x66]);
    let unmatched_tfra = full(*b"tfra", 0, &[99, 0x77]);
    let opaque_tail = b"unparsed index extension";
    before.extend(&matching);
    before.extend(&unmatched);
    before.extend(&unknown);
    before.extend(&unknown_flags);
    before.extend(test_files::boxed(
        *b"mfra",
        &[
            known_tfra.clone(),
            unmatched_tfra.clone(),
            opaque_tail.to_vec(),
        ]
        .concat(),
    ));
    // Truncated/opaque reference prefixes were already ignored by native admission.
    before.extend(test_files::boxed(*b"sidx", b"opaque"));
    let mut file = owner.create_file(&path).unwrap();
    file.write_all(&before).unwrap();
    drop(file);
    let (movie, samples) = evidence(&path);
    let mut file = crate::native::open_media_update(&path).unwrap();
    canonicalize_tool_ids(&mut file, &movie, &Stop::new()).unwrap();
    drop(file);
    let after = std::fs::read(&path).unwrap();
    let prefix = fixture("implicit").len();
    let mut expected_tail = matching;
    expected_tail[12..16].copy_from_slice(&1_u32.to_be_bytes());
    expected_tail.extend(unmatched);
    expected_tail.extend(unknown);
    expected_tail.extend(unknown_flags);
    let mut tfra = known_tfra;
    tfra[12..16].copy_from_slice(&2_u32.to_be_bytes());
    expected_tail.extend(test_files::boxed(
        *b"mfra",
        &[tfra, unmatched_tfra, opaque_tail.to_vec()].concat(),
    ));
    expected_tail.extend(test_files::boxed(*b"sidx", b"opaque"));
    assert_eq!(&after[prefix..], expected_tail);
    assert_eq!(evidence(&path).1, samples);
}

#[test]
fn descriptive_metadata_does_not_authorize_a_nested_same_name_track_header() {
    let owner = crate::conversion::files::UnpublishedStage::new(None).unwrap();
    let path = owner.path().join("source.mp4");
    let original = fixture("implicit");
    let nodes = mp4::parse_boxes(&original).unwrap();
    let moov = &nodes[0];
    let opaque = test_files::boxed(
        *b"udta",
        &test_files::boxed(*b"tkhd", b"uninterpreted descriptive bytes"),
    );
    let replacement = test_files::boxed(
        *b"moov",
        &[moov.header.payload(&original), opaque.as_slice()].concat(),
    );
    // Keep every original media address by occupying the original moov with padding.
    let mut before = original.clone();
    before[..moov.header.size]
        .copy_from_slice(&test_files::boxed(*b"free", &vec![0; moov.header.size - 8]));
    before.extend(replacement);
    let mut file = owner.create_file(&path).unwrap();
    file.write_all(&before).unwrap();
    drop(file);
    let (mut movie, samples) = evidence(&path);
    let mut file = crate::native::open_media_update(&path).unwrap();
    canonicalize_tool_ids(&mut file, &movie, &Stop::new()).unwrap();
    drop(file);
    for (i, track) in movie.tracks.iter_mut().enumerate() {
        track.track_id = u32::try_from(i + 1).unwrap();
    }
    let (actual, after_samples) = evidence(&path);
    assert_eq!(actual, movie);
    assert_eq!(after_samples, samples);
    let after = std::fs::read(path).unwrap();
    assert_eq!(&after[after.len() - opaque.len()..], opaque);
    let mut expected = before.clone();
    let roots = mp4::parse_boxes(&before).unwrap();
    for root in &roots {
        for container in &root.children {
            let kind = match (&root.kind(), &container.kind()) {
                (b"moov", b"trak") => *b"tkhd",
                (b"moov", b"mvex") => *b"trex",
                (b"moof", b"traf") => *b"tfhd",
                _ => continue,
            };
            for node in container.children.iter().filter(|node| node.kind() == kind) {
                let field = if kind == *b"tkhd" { 12 } else { 4 };
                let at = node.header.offset + node.header.header_size + field;
                let old = u32::from_be_bytes(before[at..at + 4].try_into().unwrap());
                expected[at..at + 4]
                    .copy_from_slice(&if old == 7 { 1_u32 } else { 2 }.to_be_bytes());
            }
        }
    }
    assert_eq!(after, expected);
}
