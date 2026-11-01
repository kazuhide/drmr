use super::*;
pub(super) fn config(profile: u8, depth: u8, chroma: u8) -> Vec<u8> {
    vec![
        1,
        0,
        0,
        0,
        profile,
        11,
        (depth << 4) | (chroma << 1),
        2,
        2,
        2,
        0,
        0,
    ]
}
#[test]
fn every_defined_profile_depth_and_representable_chroma_configuration_is_typed() {
    for (profile, depth, chroma) in [
        (0, 8, 0),
        (0, 8, 1),
        (1, 8, 2),
        (1, 8, 3),
        (2, 10, 1),
        (2, 12, 0),
        (3, 10, 2),
        (3, 12, 3),
    ] {
        let configuration = Configuration::from_vpcc(&config(profile, depth, chroma)).unwrap();
        assert_eq!(configuration.metadata().profile, profile);
        assert_eq!(configuration.metadata().bit_depth, depth);
        assert_eq!(configuration.metadata().chroma_subsampling, chroma);
        assert_eq!(
            configuration.codec_string(),
            format!("vp09.{profile:02}.11.{depth:02}.{chroma:02}.02.02.02.00")
        );
        let identical = Configuration::from_vpcc(&config(profile, depth, chroma)).unwrap();
        assert!(configuration.semantic_eq(&identical));
        assert_eq!(
            configuration.semantic_identity(),
            identical.semantic_identity()
        );
    }
}
#[test]
fn every_configuration_field_is_part_of_semantic_identity() {
    let source = config(0, 8, 1);
    let first = Configuration::from_vpcc(&source).unwrap();
    for (at, value) in [(5, 20), (6, 0x80), (7, 1), (8, 1), (9, 1)] {
        let mut changed = source.clone();
        changed[at] = value;
        let changed = Configuration::from_vpcc(&changed).unwrap();
        assert!(!first.semantic_eq(&changed));
        assert_ne!(first.semantic_identity(), changed.semantic_identity());
    }
    assert_eq!(
        PROTECTION_NOT_SUPPORTED.0,
        "vp9-protection-scheme-not-supported"
    );
}
#[test]
fn all_six_real_profile_depth_pairs_preserve_altref_superframes_and_sync_state() {
    use super::super::{InputKeys, native_media};
    for (name, profile, depth, frame_count) in [
        ("vp9-p0-8.mp4", 0, 8, 259),
        ("vp9-p1-8.mp4", 1, 8, 258),
        ("vp9-p2-10.mp4", 2, 10, 259),
        ("vp9-p2-12.mp4", 2, 12, 259),
        ("vp9-p3-10.mp4", 3, 10, 258),
        ("vp9-p3-12.mp4", 3, 12, 258),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../drmr/tests/fixtures/bbb")
            .join(name);
        let mut parser = None;
        let mut samples = 0;
        let mut frames = 0;
        let mut hidden = 0;
        let mut indexed = 0;
        native_media::scan(
            &path,
            &InputKeys::default(),
            &crate::concurrency::Stop::new(),
            &mut |track, info, bytes| {
                if track.kind != crate::inspection::TrackKind::Video {
                    return Ok(());
                }
                if parser.is_none() {
                    let config = Configuration::from_vpcc(&track.decoder_config)?;
                    assert_eq!(config.metadata().profile, profile);
                    assert_eq!(config.metadata().bit_depth, depth);
                    parser = Some(Parser::new(
                        config,
                        u32::from(track.sample_entry_metadata.width.unwrap()),
                        u32::from(track.sample_entry_metadata.height.unwrap()),
                    )?);
                }
                assert!(info.is_last_chunk);
                let sample = parser.as_mut().unwrap().sample(bytes)?;
                assert_eq!(sample.displayed_pictures, 1);
                assert_eq!(
                    sample.is_sync, info.is_sync,
                    "{name} sample{}",
                    info.ordinal
                );
                assert_eq!(
                    sample.frames.iter().map(|frame| frame.size).sum::<usize>()
                        + sample.index_bytes,
                    bytes.len()
                );
                assert!(
                    sample
                        .frames
                        .iter()
                        .all(|frame| frame.header_bytes > 0 && frame.header_bytes <= frame.size)
                );
                samples += 1;
                frames += sample.frames.len();
                hidden += sample
                    .frames
                    .iter()
                    .filter(|frame| !frame.show_frame)
                    .count();
                indexed += usize::from(sample.index_bytes != 0);
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{name} after{samples} samples: {error}"));
        assert_eq!(samples, 240);
        assert_eq!(frames, frame_count);
        assert!(hidden > 0);
        assert!(indexed > 0);
    }
}
#[test]
fn fullbox_version_flags_initialization_and_profile_constraints_are_not_inferred() {
    let source = config(0, 8, 1);
    for end in 0..source.len() {
        assert!(Configuration::from_vpcc(&source[..end]).is_err());
    }
    for (at, value) in [
        (0, 0),
        (1, 1),
        (4, 4),
        (5, 9),
        (6, 0x84),
        (7, 3),
        (8, 3),
        (9, 3),
        (11, 1),
    ] {
        let mut bad = source.clone();
        bad[at] = value;
        assert!(Configuration::from_vpcc(&bad).is_err(), "{at}");
    }
    for (profile, depth, chroma) in [(0, 10, 1), (1, 8, 1), (2, 8, 1), (3, 10, 1), (3, 12, 4)] {
        assert!(Configuration::from_vpcc(&config(profile, depth, chroma)).is_err());
    }
}

#[test]
fn undefined_level_is_retained_without_inventing_a_decoder_level() {
    let mut unknown = config(0, 8, 0);
    unknown[5] = 0;
    let unknown = Configuration::from_vpcc(&unknown).unwrap();
    assert_eq!(unknown.metadata().level, 0);
    assert_eq!(unknown.codec_string(), "vp09.00.00.08.00.02.02.02.00");
    assert!(!unknown.semantic_eq(&Configuration::from_vpcc(&config(0, 8, 0)).unwrap()));
}
