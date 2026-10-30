use super::super::super::{
    ConversionError, Converter, DecodeEvidence, InputKeys, VerificationScope,
};
use super::{boundary_budget, collect_counted, count_resources};
use crate::{
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

mod audio_range;
mod crypto;

#[test]
fn boundary_metadata_budget_rejects_overflow_before_allocating_or_opening_resources() {
    let per_resource = std::mem::size_of::<crate::conversion::native_media::SegmentReceipt>()
        + std::mem::size_of::<crate::conversion::native_media::SegmentTrackEvidence>();
    let maximum = super::MAX_BOUNDARY_BYTES / per_resource;
    boundary_budget(maximum, 1).unwrap();
    for (resources, tracks) in [(maximum + 1, 1), (usize::MAX, 1), (1, usize::MAX)] {
        assert_eq!(
            boundary_budget(resources, tracks).unwrap_err(),
            ConversionError("resource-budget")
        );
    }
}

#[test]
fn a_cancelled_scan_does_not_open_inputs_or_run_tools() {
    let (converter, events) = converter();
    let stop = crate::concurrency::Stop::new();
    stop.set();
    let converter = converter.with_stop(stop);
    assert_eq!(
        refused(converter.backend.native_segmented_media(
            Path::new("must-not-open-init.mp4"),
            &[PathBuf::from("must-not-open-media.m4s")],
            &InputKeys::default()
        )),
        ConversionError("cancelled")
    );
    assert_no_tools(&events);
}

struct Fixture {
    directory: crate::native::PrivateTempDir,
    full: PathBuf,
    init: PathBuf,
    fragments: Vec<Vec<u8>>,
    resources: Vec<PathBuf>,
}

fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "must-not-run-packager", "must-not-run-ffmpeg"),
        events,
    )
}

fn fixture() -> Fixture {
    let directory = crate::native::private_tempdir().unwrap();
    let full = directory.path().join("fixture.mp4");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    // This independent fixture encoder adds B pictures and custom keyframes.
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "scale=96:54",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-threads",
            "1",
            "-bf",
            "2",
            "-g",
            "48",
            "-keyint_min",
            "1",
            "-sc_threshold",
            "0",
            "-force_key_frames",
            "1,3,5.5,7,8.5",
            "-movflags",
            "+empty_moov+default_base_moof+frag_every_frame",
            "-f",
            "mp4",
        ])
        .arg(&full)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "independent fixture encoding failed"
    );
    let bytes = std::fs::read(&full).unwrap();
    let (initialization, fragments) = crate::mp4::split_fragmented(&bytes).unwrap();
    assert_eq!(fragments.len(), 240);
    let init = directory.path().join("init.mp4");
    std::fs::write(&init, initialization).unwrap();
    let mut result = Fixture {
        directory,
        full,
        init,
        fragments,
        resources: Vec::new(),
    };
    result.resources = result.group(&[80, 200], "source");
    result
}

impl Fixture {
    fn group(&self, cuts: &[usize], prefix: &str) -> Vec<PathBuf> {
        let mut boundaries = vec![0];
        boundaries.extend_from_slice(cuts);
        boundaries.push(self.fragments.len());
        boundaries
            .windows(2)
            .enumerate()
            .map(|(ordinal, range)| {
                let path = self
                    .directory
                    .path()
                    .join(format!("{prefix}-{ordinal}.m4s"));
                std::fs::write(&path, self.fragments[range[0]..range[1]].concat()).unwrap();
                path
            })
            .collect()
    }
}

fn audio_fixture(rate: u32) -> Fixture {
    let directory = crate::native::private_tempdir().unwrap();
    let full = directory.path().join("fixture.mp4");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(source)
        .args(["-map", "0:a:0", "-vn", "-t", "2", "-c:a", "aac", "-ar"])
        .arg(rate.to_string())
        .args([
            "-threads",
            "1",
            "-movflags",
            "+empty_moov+default_base_moof+frag_every_frame",
            "-f",
            "mp4",
        ])
        .arg(&full)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "independent audio fixture encoding failed"
    );
    let (initialization, fragments) =
        crate::mp4::split_fragmented(&std::fs::read(&full).unwrap()).unwrap();
    assert!(fragments.len() > 10);
    let init = directory.path().join("init.mp4");
    std::fs::write(&init, initialization).unwrap();
    let mut result = Fixture {
        directory,
        full,
        init,
        fragments,
        resources: Vec::new(),
    };
    let count = result.fragments.len();
    result.resources = result.group(&[count / 3, count * 2 / 3], "audio");
    result
}

fn refused(result: Result<super::SegmentedMedia, ConversionError>) -> ConversionError {
    result.err().expect("invalid segmented source was admitted")
}

fn changed_defaults(bytes: &[u8], duration: u32) -> Vec<u8> {
    let nodes = crate::mp4::parse_boxes(bytes).unwrap();
    let header = crate::mp4::walk(&nodes)
        .into_iter()
        .rev()
        .find(|node| node.kind() == *b"tfhd")
        .unwrap();
    let payload = header.header.payload(bytes);
    let flags = u32::from_be_bytes(payload[..4].try_into().unwrap());
    assert_ne!(flags & 8, 0);
    let offset = 8 + usize::from(flags & 1 != 0) * 8 + usize::from(flags & 2 != 0) * 4;
    let at = header.header.offset + header.header.header_size + offset;
    let mut changed = bytes.to_vec();
    changed[at..at + 4].copy_from_slice(&duration.to_be_bytes());
    changed
}

fn patch_box(bytes: &[u8], kind: [u8; 4], relative: usize, value: &[u8]) -> Vec<u8> {
    let nodes = crate::mp4::parse_boxes(bytes).unwrap();
    let node = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == kind)
        .unwrap();
    let offset = node.header.offset + node.header.header_size + relative;
    let mut changed = bytes.to_vec();
    changed[offset..offset + value.len()].copy_from_slice(value);
    changed
}

fn edited_initialization(bytes: &[u8], scale: u32, duration: u32) -> Vec<u8> {
    fn boxed(kind: [u8; 4], payload: Vec<u8>) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend_from_slice(&kind);
        bytes.extend(payload);
        bytes
    }
    fn rebuild(node: &crate::mp4::Mp4Box, bytes: &[u8], edits: &[u8]) -> Vec<u8> {
        if !matches!(&node.kind(), b"moov" | b"trak") {
            return node.header.bytes(bytes).to_vec();
        }
        let mut payload = node
            .children
            .iter()
            .flat_map(|child| rebuild(child, bytes, edits))
            .collect::<Vec<_>>();
        if node.kind() == *b"trak" {
            assert!(node.children.iter().all(|child| child.kind() != *b"edts"));
            payload.extend_from_slice(edits);
        }
        boxed(node.kind(), payload)
    }
    let bytes = patch_box(bytes, *b"mvhd", 12, &scale.to_be_bytes());
    let mut payload = vec![0; 4];
    payload.extend_from_slice(&1_u32.to_be_bytes());
    payload.extend_from_slice(&duration.to_be_bytes());
    payload.extend_from_slice(&0_i32.to_be_bytes());
    payload.extend_from_slice(&1_u16.to_be_bytes());
    payload.extend_from_slice(&0_u16.to_be_bytes());
    let edits = boxed(*b"edts", boxed(*b"elst", payload));
    crate::mp4::parse_boxes(&bytes)
        .unwrap()
        .iter()
        .flat_map(|node| rebuild(node, &bytes, &edits))
        .collect()
}

#[test]
fn a_one_tick_discontinuity_and_a_declared_missing_tail_are_refused() {
    let source = fixture();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let path = &source.resources[1];
    let original = std::fs::read(path).unwrap();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let tfdt = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tfdt")
        .unwrap();
    let payload = tfdt.header.payload(&original);
    assert_eq!(payload[0], 1);
    let time = u64::from_be_bytes(payload[4..12].try_into().unwrap());
    std::fs::write(
        path,
        patch_box(&original, *b"tfdt", 4, &(time + 1).to_be_bytes()),
    )
    .unwrap();
    assert_eq!(
        refused(backend.native_segmented_media(&source.init, &source.resources, &keys)),
        ConversionError("fragment-timeline-gap-or-overlap")
    );
    std::fs::write(path, original).unwrap();
    let admitted = backend
        .native_segmented_media(&source.init, &source.resources, &keys)
        .unwrap();
    let coded = admitted.media.tracks[0].samples.coded_duration;
    let scale = admitted.media.tracks[0].descriptor.timescale;
    let ticks =
        u32::try_from(coded.numerator * i128::from(scale) / i128::from(coded.denominator)).unwrap();
    let original_init = std::fs::read(&source.init).unwrap();
    std::fs::write(
        &source.init,
        patch_box(&original_init, *b"mdhd", 16, &ticks.to_be_bytes()),
    )
    .unwrap();
    assert!(
        backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .is_ok()
    );
    assert_eq!(
        refused(backend.native_segmented_media(&source.init, &source.resources[..2], &keys)),
        ConversionError("native-media-duration-mismatch")
    );
    assert_no_tools(&events);
}

#[test]
fn semantic_controls_are_admitted_and_compared_instead_of_discarded() {
    let source = fixture();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let original = std::fs::read(&source.init).unwrap();
    let baseline = backend
        .native_segmented_media(&source.init, &source.resources, &keys)
        .unwrap();
    for (offset, value) in [
        (32, 1_i16.to_be_bytes().to_vec()),
        (40, 131_072_i32.to_be_bytes().to_vec()),
    ] {
        let changed = source.init.with_file_name("changed-control.mp4");
        std::fs::write(&changed, patch_box(&original, *b"tkhd", offset, &value)).unwrap();
        let candidate = backend
            .native_segmented_media(&changed, &source.resources, &keys)
            .unwrap();
        let report = backend
            .native_verify_segmented(&baseline, &candidate)
            .unwrap();
        assert!(!report.passed);
        assert!(!report.checks.stream_metadata.is_pass());
        assert!(report.checks.packet_payloads_equal);
        assert!(report.checks.packet_timing_equal);
    }
    std::fs::write(
        &source.init,
        patch_box(&original, *b"mvhd", 20, &131_072_i32.to_be_bytes()),
    )
    .unwrap();
    assert_eq!(
        refused(backend.native_segmented_media(&source.init, &source.resources, &keys)),
        ConversionError("presentation-window-not-supported")
    );
    assert_no_tools(&events);
}

#[test]
fn finite_audio_edits_retain_exact_subframe_duration_in_segmented_comparison() {
    let source = audio_fixture(48_000);
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let original = std::fs::read(&source.init).unwrap();
    let mut admitted = Vec::new();
    for duration in [1, 1023, 1024] {
        let init = source.init.with_file_name(format!("edit-{duration}.mp4"));
        std::fs::write(&init, edited_initialization(&original, 48_000, duration)).unwrap();
        admitted.push(
            backend
                .native_segmented_media(&init, &source.resources, &keys)
                .unwrap(),
        );
    }
    for media in &admitted {
        assert!(
            backend
                .native_verify_segmented(media, media)
                .unwrap()
                .passed
        );
    }
    for pair in admitted.windows(2) {
        let report = backend.native_verify_segmented(&pair[0], &pair[1]).unwrap();
        assert!(!report.passed);
        assert!(report.checks.stream_metadata.is_pass());
        assert!(report.checks.packet_payloads_equal);
        assert!(!report.checks.packet_timing_equal);
    }
    assert_no_tools(&events);
}

#[test]
fn aac_at_both_rates_keeps_one_track_wide_clock_and_rejects_a_short_middle_frame() {
    for rate in [44_100, 48_000] {
        let source = audio_fixture(rate);
        let (converter, events) = converter();
        let backend = &converter.backend;
        let keys = InputKeys::default();
        let whole = backend.native_media(&source.full, None).unwrap();
        let segmented = backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .unwrap();
        let a = &whole.tracks[0].samples;
        let b = &segmented.media.tracks[0].samples;
        assert_eq!(a.count, b.count);
        assert_eq!(a.raw_payload, b.raw_payload);
        assert_eq!(a.canonical_payload, b.canonical_payload);
        assert!(a.clocks_equal(b));
        assert_eq!(a.durations, b.durations);
        assert!(backend.native_admit_segmented(&segmented).unwrap().passed);
        let path = &source.resources[1];
        let original = std::fs::read(path).unwrap();
        std::fs::write(path, changed_defaults(&original, 512)).unwrap();
        assert_eq!(
            refused(backend.native_segmented_media(&source.init, &source.resources, &keys)),
            ConversionError("native-aac-clock-mismatch")
        );
        std::fs::write(path, original).unwrap();
        let restored = backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .unwrap();
        assert!(
            backend
                .native_verify_segmented(&segmented, &restored)
                .unwrap()
                .passed
        );
        assert_no_tools(&events);
    }
}

#[test]
fn missing_duplicate_and_reordered_resources_never_reset_the_global_clock() {
    let source = fixture();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    for selected in [vec![0, 2], vec![0, 1, 1, 2], vec![1, 0, 2]] {
        let resources = selected
            .iter()
            .map(|&index| source.resources[index].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            refused(backend.native_segmented_media(&source.init, &resources, &keys)),
            ConversionError("fragment-timeline-gap-or-overlap")
        );
    }
    assert!(
        backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .is_ok()
    );
    assert_no_tools(&events);
}

#[test]
fn raw_resource_changes_between_count_and_sample_passes_do_not_publish_evidence() {
    let source = fixture();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let input =
        crate::conversion::native_media::SegmentInput::new(&source.init, &backend.stop).unwrap();
    let counted = count_resources(&input, &source.resources, &keys, backend).unwrap();
    let path = &source.resources[0];
    let original = std::fs::read(path).unwrap();
    let mut changed = original.clone();
    // A padding-only change preserves all decrypted samples and clocks.
    changed.extend_from_slice(&[0, 0, 0, 8, b'f', b'r', b'e', b'e']);
    std::fs::write(path, changed).unwrap();
    assert_eq!(
        refused(collect_counted(
            &input,
            &source.resources,
            &keys,
            backend,
            counted,
            None,
        )),
        ConversionError("native-source-changed")
    );
    std::fs::write(path, original).unwrap();
    assert!(
        backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .is_ok()
    );
    assert_no_tools(&events);
}

#[test]
fn changed_initialization_identity_is_refused_even_when_the_bytes_are_equal() {
    let source = fixture();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let input =
        crate::conversion::native_media::SegmentInput::new(&source.init, &backend.stop).unwrap();
    let counted = count_resources(&input, &source.resources, &keys, backend).unwrap();
    let original = std::fs::read(&source.init).unwrap();
    let saved = source.init.with_file_name("saved-init.mp4");
    std::fs::rename(&source.init, &saved).unwrap();
    std::fs::write(&source.init, original).unwrap();
    assert_eq!(
        refused(collect_counted(
            &input,
            &source.resources,
            &keys,
            backend,
            counted,
            None,
        )),
        ConversionError("native-source-changed")
    );
    std::fs::remove_file(&source.init).unwrap();
    std::fs::rename(saved, &source.init).unwrap();
    assert!(
        backend
            .native_segmented_media(&source.init, &source.resources, &keys)
            .is_ok()
    );
    assert_no_tools(&events);
}

fn assert_no_tools(events: &Collector) {
    assert!(
        events.records().iter().all(|event| {
            event.get("stage").and_then(serde_json::Value::as_str) != Some("tool")
        })
    );
}

#[test]
fn nonuniform_resources_keep_complete_codec_clock_and_keyframe_evidence_without_joining() {
    let source = fixture();
    let original = source
        .resources
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect::<Vec<_>>();
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let whole = backend.native_media(&source.full, None).unwrap();
    let segmented = backend
        .native_segmented_media(&source.init, &source.resources, &keys)
        .unwrap();
    let admission = backend.native_admit_segmented(&segmented).unwrap();
    assert!(admission.passed);
    assert_eq!(admission.scope, VerificationScope::SourceAdmission);
    assert_eq!(admission.decode_evidence, DecodeEvidence::not_requested());
    assert_eq!(segmented.boundaries.len(), 3);
    assert_eq!(
        segmented
            .boundaries
            .iter()
            .map(|part| part.tracks[0].sample_count)
            .collect::<Vec<_>>(),
        [80, 120, 40]
    );
    let a = &whole.tracks[0].samples;
    let b = &segmented.media.tracks[0].samples;
    assert_eq!(a.count, 240);
    assert_eq!(a.raw_payload, b.raw_payload);
    assert_eq!(a.canonical_payload, b.canonical_payload);
    assert!(a.clocks_equal(b));
    assert_eq!(a.durations, b.durations);
    assert_eq!(a.sync_samples, b.sync_samples);
    assert!(b.continuous_decode);
    assert!(b.initial_composition_offset.numerator > 0);
    assert!(
        backend
            .native_verify_segmented(&segmented, &segmented)
            .unwrap()
            .passed
    );
    assert_eq!(
        source
            .resources
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>(),
        original
    );
    assert_no_tools(&events);
}

#[test]
fn moving_one_sample_between_resources_fails_even_when_complete_media_is_equal() {
    let source = fixture();
    let moved = source.group(&[81, 200], "changed");
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::default();
    let a = backend
        .native_segmented_media(&source.init, &source.resources, &keys)
        .unwrap();
    let b = backend
        .native_segmented_media(&source.init, &moved, &keys)
        .unwrap();
    assert_eq!(
        a.media.tracks[0].samples.raw_payload,
        b.media.tracks[0].samples.raw_payload
    );
    assert!(
        a.media.tracks[0]
            .samples
            .clocks_equal(&b.media.tracks[0].samples)
    );
    let report = backend.native_verify_segmented(&a, &b).unwrap();
    assert!(!report.passed);
    assert!(report.checks.stream_metadata.is_pass());
    assert!(report.checks.packet_payloads_equal);
    assert!(!report.checks.packet_timing_equal);
    assert_no_tools(&events);
}
