use super::*;

fn structural_segment() -> (crate::native::PrivateTempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = crate::native::private_tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb/clip.mp4");
    let stop = Stop::new();
    let mut reader = read::Reader::open(&fixture, &stop, false).unwrap();
    let layout = reader.layout().unwrap();
    reader.finish().unwrap();
    let combined = [
        test_files::boxed(*b"ftyp", b"isom\0\0\0\0isomiso6"),
        test_files::interleaved_fragments(&layout, false),
    ].concat();
    let (init, parts) = crate::mp4::split_fragmented(&combined).unwrap();
    assert_eq!(parts.len(), 1);
    let init_path = root.path().join("init.mp4");
    let media_path = root.path().join("segment.m4s");
    std::fs::write(&init_path, init).unwrap();
    std::fs::write(&media_path, &parts[0]).unwrap();
    (root, init_path, media_path)
}

#[test]
fn separate_initialization_and_resource_keep_raw_sample_clocks_without_joining() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    assert!(input.movie().fragmented);
    let evidence = input.evidence(&media, &InputKeys::default(), &stop).unwrap();
    assert_eq!(evidence.tracks.len(), 2);
    assert_eq!(evidence.tracks[0].sample_count, 2);
    assert_eq!(evidence.tracks[0].first_dts, 0);
    assert_eq!(evidence.tracks[0].end_dts, 200);
    assert_eq!(evidence.tracks[1].sample_count, 1);
    assert_eq!(evidence.tracks[1].first_dts, 0);
    assert_eq!(evidence.tracks[1].end_dts, 100);
}

#[test]
fn cancelled_resource_scan_does_not_yield_plaintext() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    stop.set();
    let mut callbacks = 0;
    let result = input.scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    assert_eq!(callbacks, 0);
}

#[test]
fn resource_boundary_clock_changes_and_payload_changes_have_distinct_evidence() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    let original = std::fs::read(&media).unwrap();
    let expected = input.evidence(&media, &InputKeys::default(), &stop).unwrap();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let mdats = crate::mp4::walk(&nodes).into_iter().find(|node| node.kind() == *b"mdat").unwrap();
    let mut payload = original.clone();
    payload[mdats.header.offset + mdats.header.header_size] ^= 1;
    std::fs::write(&media, payload).unwrap();
    let changed = input.evidence(&media, &InputKeys::default(), &stop).unwrap();
    assert_ne!(expected.tracks[0].payload, changed.tracks[0].payload);
    assert_eq!(expected.tracks[0].samples, changed.tracks[0].samples);
    std::fs::write(&media, original).unwrap();
    assert_eq!(expected, input.evidence(&media, &InputKeys::default(), &stop).unwrap());
}

#[test]
fn changed_initialization_and_invalid_resource_refuse_before_plaintext_callbacks() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    let original_init = std::fs::read(&init).unwrap();
    let mut changed = original_init.clone();
    changed[16] ^= 1;
    std::fs::write(&init, &changed).unwrap();
    let mut callbacks = 0;
    let result = input.scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), ConversionError("native-source-changed"));
    assert_eq!(callbacks, 0);
    std::fs::write(&init, original_init).unwrap();
    let original = std::fs::read(&media).unwrap();
    std::fs::write(&media, &original[..original.len() - 1]).unwrap();
    assert!(input.scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    }).is_err());
    assert_eq!(callbacks, 0);
    std::fs::write(&media, original).unwrap();
    assert!(input.evidence(&media, &InputKeys::default(), &stop).is_ok());
}
