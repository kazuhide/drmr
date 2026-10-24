use super::super::{
    ContentKey, Converter, DecodeEvidence, HlsInputKeys, InputKeys, SourceValidationPurpose,
    VerificationSource, backend::Backend, ts,
};
use crate::{
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4")
}
fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "must-not-run-packager", "must-not-run-ffmpeg"),
        events,
    )
}
#[test]
fn complete_progressive_source_and_original_comparison_need_no_media_tool() {
    let (converter, events) = converter();
    let empty = InputKeys::default();
    let source = fixture();
    let original = std::fs::read(&source).unwrap();
    let admitted = converter
        .validate_source(
            VerificationSource {
                input: &source,
                input_keys: &empty,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    assert!(admitted.verification.passed);
    assert_eq!(
        admitted.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    let compared = converter
        .verify_mp4(&source, &empty, &source, &empty)
        .unwrap();
    assert!(compared.passed);
    assert_eq!(compared.decode_evidence, DecodeEvidence::not_requested());
    assert!(!compared.license_playback_verified);
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
    assert_eq!(std::fs::read(source).unwrap(), original);
}
#[test]
fn changed_original_sample_content_is_a_negative_completed_comparison() {
    let (converter, _) = converter();
    let empty = InputKeys::default();
    let source = fixture();
    let root = crate::native::private_tempdir().unwrap();
    let changed = root.path().join("changed.mp4");
    let mut bytes = std::fs::read(&source).unwrap();
    let mut position = None;
    super::super::native_media::scan(
        &source,
        &empty,
        &crate::concurrency::Stop::new(),
        &mut |track, info, _| {
            if position.is_none() && track.kind == crate::inspection::TrackKind::Audio {
                position = Some(usize::try_from(info.range.end - 1).unwrap());
            }
            Ok(())
        },
    )
    .unwrap();
    bytes[position.unwrap()] ^= 1;
    std::fs::write(&changed, bytes).unwrap();
    let report = converter
        .verify_mp4(&source, &empty, &changed, &empty)
        .unwrap();
    assert!(!report.passed);
    assert!(!report.checks.packet_payloads_equal);
    assert!(report.checks.packet_timing_equal);
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
}

#[test]
fn independent_packager_track_normalization_keeps_original_sample_evidence() {
    let (converter, _) = converter();
    let root = super::super::workspace().unwrap();
    let source = root.path().join("source.mp4");
    super::super::copy_private(&fixture(), &source).unwrap();
    // The fixture/oracle packager is explicit; the verifier remains entirely native.
    let mut backend = converter.backend.clone();
    backend.packager = "packager".into();
    for index in 0..2 {
        let directory = root.path().join(format!("track-{index}"));
        super::super::files::create_dir(&directory).unwrap();
        let output = directory.join("track.mp4");
        backend
            .package_stream(&source, index, &output, None, None)
            .unwrap();
        backend
            .native_media(&source, None)
            .unwrap_or_else(|error| panic!("original track {index}: {error}"));
        backend.native_media(&output, None).unwrap_or_else(|error| {
            let metadata = super::super::audio::movie(&output).unwrap();
            let boxes = crate::mp4::parse_boxes(&metadata.bytes).unwrap();
            let kinds: Vec<_> = crate::mp4::walk(&boxes)
                .iter()
                .map(|node| crate::mp4::fourcc_text(&node.kind()))
                .collect();
            panic!("normalized track {index}: {error}; metadata box kinds {kinds:?}");
        });
        let report = backend
            .native_verify_views(
                super::super::verify::StreamView::new(&source, index, None),
                super::super::verify::StreamView::new(&output, 0, None),
            )
            .unwrap();
        if !report.passed {
            let first = backend.native_media(&source, None).unwrap();
            let second = backend.native_media(&output, None).unwrap();
            let a = &first.tracks[index];
            let b = &second.tracks[0];
            assert!(
                report.checks.stream_metadata.is_pass(),
                "track {index} metadata: {:?} versus {:?}",
                a.descriptor.sample_entry_metadata,
                b.descriptor.sample_entry_metadata
            );
            assert!(
                report.checks.packet_payloads_equal,
                "track {index} encoded content differs"
            );
            assert!(
                report.checks.packet_timing_equal,
                "track {index} clocks_equal={} durations_equal={} sync_equal={} recovery_equal={} first={:?}/{:?} end={:?}/{:?}",
                a.samples.clocks == b.samples.clocks,
                a.samples.durations == b.samples.durations,
                a.samples.sync_samples == b.samples.sync_samples,
                a.samples.recovery == b.samples.recovery,
                a.samples.first_pts,
                b.samples.first_pts,
                a.samples.end_pts,
                b.samples.end_pts
            );
        }
        assert!(report.passed);
    }
}

fn muxed_fixture_keys(include_audio: bool) -> InputKeys {
    let mut keys = vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
    ];
    if include_audio {
        keys.push(
            ContentKey::from_hex(
                "303132333435363738393a3b3c3d3e3f",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        );
    }
    InputKeys::new(keys).unwrap()
}

fn write_video_transport_fixture(output: &Path) {
    // This explicit fixture remux is independent of the native verifier.
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-copyts", "-i"])
        .arg(fixture())
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c:v",
            "copy",
            "-copytb",
            "1",
            "-bsf:v",
            "h264_mp4toannexb",
            "-mpegts_copyts",
            "1",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-avoid_negative_ts",
            "disabled",
            "-f",
            "mpegts",
        ])
        .arg(output)
        .output()
        .unwrap();
    assert!(result.status.success(), "transport fixture remux failed");
}

fn assert_original_video_transport_evidence(backend: &Backend, native: &ts::NativeEvidence) {
    let original = backend.native_media(&fixture(), None).unwrap();
    let track = original
        .tracks
        .iter()
        .find(|track| track.descriptor.kind == crate::inspection::TrackKind::Video)
        .unwrap();
    let evidence = native.evidence().unwrap();
    let timing = native.timing().unwrap();
    assert_eq!(
        (timing.first_sample_pts, timing.first_pts, timing.first_dts),
        (0, 0, 0)
    );
    assert_eq!(track.config.as_ref(), Some(&native.config));
    assert_eq!(track.samples.count, evidence.count);
    assert_eq!(
        track.samples.canonical_payload,
        Some(evidence.payload_digest())
    );
    assert_eq!(
        track.samples.transport_clocks,
        Some(evidence.clock_digest())
    );
    assert_eq!(
        track.samples.presented_first_pts,
        super::samples::Ratio::ticks(timing.first_pts, 90_000).unwrap()
    );
    assert_eq!(
        track.samples.presented_end_pts,
        super::samples::Ratio::ticks(timing.end_pts, 90_000).unwrap()
    );
}

#[test]
fn selected_transport_video_comparison_keeps_every_muxed_mp4_key_in_both_directions() {
    let root = crate::native::private_tempdir().unwrap();
    let transport = root.path().join("video.ts");
    write_video_transport_fixture(&transport);
    let mut scanner = ts::NativeScanner::new();
    scanner
        .segment(&transport, &crate::concurrency::Stop::new())
        .unwrap();
    let (admission, mut anchors) = scanner.finish().unwrap();
    assert_eq!(admission.tracks.len(), 1);
    let declared = &admission.tracks[0];
    assert_eq!(declared.kind, crate::inspection::TrackKind::Video);
    let native = anchors.remove(&declared.pid).unwrap();
    let (converter, events) = converter();
    let backend = &converter.backend;
    assert_original_video_transport_evidence(backend, &native);
    let transport_view = ts::View {
        input: &transport,
        metadata: &transport,
        index: 0,
        key: None,
        all_keys: None,
        transport: true,
        native: Some(&native),
        unedited: false,
    };
    let keys = muxed_fixture_keys(true);
    let missing = muxed_fixture_keys(false);
    let selected_key = &keys.keys[0];
    for name in ["cenc-iv8.mp4", "cenc-iv16.mp4"] {
        let input = fixture().with_file_name(name);
        let movie = super::super::native_media::inspect(&input, &backend.stop).unwrap();
        assert_eq!(movie.tracks.len(), 2);
        assert_eq!(movie.tracks[0].kind, crate::inspection::TrackKind::Video);
        assert_eq!(movie.tracks[1].kind, crate::inspection::TrackKind::Audio);
        let mp4 = ts::View {
            input: &input,
            metadata: &input,
            index: 0,
            key: Some(selected_key),
            all_keys: None,
            transport: false,
            native: None,
            unedited: false,
        }
        .with_all_keys(&keys);
        for (source, result) in [(transport_view, mp4), (mp4, transport_view)] {
            let report = backend
                .native_verify_transport(source, result)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(report.checks.stream_metadata.is_pass(), "{name}");
            assert!(report.checks.packet_payloads_equal, "{name}");
            assert!(report.checks.packet_timing_equal, "{name}");
            assert!(report.passed, "{name}");
            assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
        }
        let incomplete = ts::View {
            key: Some(&missing.keys[0]),
            ..mp4
        }
        .with_all_keys(&missing);
        for (source, result) in [(transport_view, incomplete), (incomplete, transport_view)] {
            assert_eq!(
                backend
                    .native_verify_transport(source, result)
                    .unwrap_err()
                    .0,
                "input-key-required",
                "{name}"
            );
        }
        assert!(std::ptr::eq(mp4.key.unwrap(), selected_key));
        assert!(transport_view.key.is_none());
        assert!(transport_view.all_keys.is_none());
    }
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}

#[test]
fn recognizing_a_codec_name_never_qualifies_its_unimplemented_sample_syntax() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("unqualified.mp4");
    let mut bytes = std::fs::read(fixture()).unwrap();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    for (old, new) in [(b"avc1", b"hvc1"), (b"avcC", b"hvcC")] {
        let at = crate::mp4::walk(&nodes)
            .into_iter()
            .find(|node| node.kind() == *old)
            .unwrap()
            .header
            .offset
            + 4;
        bytes[at..at + 4].copy_from_slice(new);
    }
    std::fs::write(&input, bytes).unwrap();
    let metadata = crate::inspection::inspect(&input).unwrap();
    assert!(!metadata.media_verified);
    assert_eq!(metadata.tracks[0].codec.as_deref(), Some("hvc1"));
    let (converter, events) = converter();
    let empty = InputKeys::default();
    for depth in [
        super::super::ValidationDepth::Native,
        super::super::ValidationDepth::Decoded,
    ] {
        let mut backend = converter.backend.clone();
        backend.validation_depth = depth;
        assert!(backend.native_media(&input, None).is_err());
        assert!(backend.verify_media(&input, &input, None, None).is_err());
    }
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
    assert!(
        converter
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &HlsInputKeys::default()
                },
                SourceValidationPurpose::Conversion
            )
            .is_err()
    );
}
