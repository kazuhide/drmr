//! Explicit signaling admission and conversion share the same immutable source graph.
use drmr_core::{
    conversion::{
        ConversionRequest, Converter, DrmSystem, HlsInputKeys, InputKeys, LocalTrack, OutputKey,
        OutputSignaling, TrackConversionRequest, VerificationSource,
    },
    events::{NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn runner() -> Arc<ToolRunner> {
    Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)))
}
fn converter() -> Converter {
    Converter::new(runner(), "packager", "ffmpeg").with_decode(true)
}
fn pssh() -> Vec<u8> {
    drmr_core::playready::build_pssh_box(
        &drmr_core::playready::WIDEVINE_SYSTEM_ID,
        &hex::decode(format!("1210{KID}")).unwrap(),
    )
    .unwrap()
}
fn source(root: &Path) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let input = root.join("source.mp4");
    let args = [
        "ffmpeg",
        "-v",
        "error",
        "-nostdin",
        "-i",
        clip.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-c",
        "copy",
        "-map_metadata",
        "-1",
        "-bitexact",
        "-movflags",
        "delay_moov+default_base_moof+frag_keyframe",
        input.to_str().unwrap(),
    ]
    .map(str::to_owned);
    let result = runner()
        .run_tool(&args, "ffmpeg", &RunOptions::default())
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    input
}
struct ReplaceDuringConversion {
    path: PathBuf,
    replacement: Vec<u8>,
    fired: AtomicBool,
    phase: &'static str,
}
impl Sink for ReplaceDuringConversion {
    fn emit(&self, event: &Record) {
        if event.get("activity").and_then(serde_json::Value::as_str) == Some(self.phase)
            && event.get("state").and_then(serde_json::Value::as_str) == Some("started")
            && !self.fired.swap(true, Ordering::SeqCst)
        {
            std::fs::write(&self.path, &self.replacement).unwrap();
        }
    }
}
fn replacement(input: &Path) -> Arc<ReplaceDuringConversion> {
    let mut bytes = std::fs::read(input).unwrap();
    bytes.extend(pssh());
    Arc::new(ReplaceDuringConversion {
        path: input.to_owned(),
        replacement: bytes,
        fired: AtomicBool::new(false),
        phase: "inspection",
    })
}

#[test]
fn source_admission_cannot_mix_a_clear_allowlist_with_replaced_provider_metadata() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let original = std::fs::read(&input).unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &input,
        input_keys: &empty,
        hls_keys: &hls,
    };
    converter()
        .validate_drm_source(
            source,
            &[DrmSystem::Widevine],
            OutputFormat::Dash,
            TargetProtection::Cbcs,
        )
        .unwrap();
    let sink = replacement(&input);
    let result = converter()
        .with_activity_sink(sink.clone())
        .validate_drm_source(
            source,
            &[DrmSystem::Widevine],
            OutputFormat::Dash,
            TargetProtection::Cbcs,
        );
    std::fs::write(&input, &original).unwrap();
    assert!(sink.fired.load(Ordering::SeqCst));
    assert_eq!(
        result,
        Err(drmr_core::conversion::ConversionError(
            "planned-source-changed"
        ))
    );
    assert_eq!(std::fs::read(input).unwrap(), original);
}

#[test]
fn explicit_conversion_cannot_publish_after_admission_reads_replaced_provider_metadata() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let original = std::fs::read(&input).unwrap();
    let output = owner.path().join("result");
    let sink = replacement(&input);
    let signaling = OutputSignaling::multi_drm(Some(&hex::encode(pssh())), None, false).unwrap();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let result = converter()
        .with_activity_sink(sink.clone())
        .with_output_signaling(signaling)
        .convert(ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Cbcs,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: Some(&key),
            hls_key_uri: None,
            hls_output_key: None,
        });
    std::fs::write(&input, &original).unwrap();
    assert!(sink.fired.load(Ordering::SeqCst));
    assert_eq!(
        result.err(),
        Some(drmr_core::conversion::ConversionError(
            "planned-source-changed"
        ))
    );
    assert!(
        !output.exists(),
        "Changed source evidence must not publish a result"
    );
    assert_eq!(std::fs::read(input).unwrap(), original);
}

#[test]
fn direct_track_conversion_preserves_source_guards_until_publication() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let original = std::fs::read(&input).unwrap();
    let tracks = [LocalTrack {
        source: input.clone(),
        kind: drmr_core::inspection::TrackKind::Video,
        index: 0,
    }];
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let signaling = OutputSignaling::multi_drm(Some(&hex::encode(pssh())), None, false).unwrap();
    let baseline = owner.path().join("baseline");
    let request = TrackConversionRequest {
        tracks: &tracks,
        output: &baseline,
        format: OutputFormat::Dash,
        target: TargetProtection::Cbcs,
        input_keys: &InputKeys::default(),
        output_key: Some(&key),
        hls_key_uri: None,
        hls_output_key: None,
    };
    let command = converter().with_output_signaling(signaling);
    assert!(command.convert_tracks(request).unwrap().verification.passed);
    let mut replacement = replacement(&input);
    Arc::get_mut(&mut replacement).unwrap().phase = "normalization";
    let output = owner.path().join("result");
    let result = command
        .with_activity_sink(replacement.clone())
        .convert_tracks(TrackConversionRequest {
            output: &output,
            ..request
        });
    std::fs::write(&input, &original).unwrap();
    assert!(replacement.fired.load(Ordering::SeqCst));
    assert_eq!(
        result.err(),
        Some(drmr_core::conversion::ConversionError(
            "planned-source-changed"
        ))
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), original);
}

#[test]
fn legacy_playready_verification_checks_the_manifest_resolved_kid_and_scheme() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let output = owner.path().join("legacy");
    let mut keys = drmr_core::conversion::key_file::OperatorKeys::default();
    keys.output = Some(OutputKey::from_hex(KID, KEY).unwrap());
    keys.signaling = Some(OutputSignaling::playready("https://example.invalid/license").unwrap());
    assert!(
        converter()
            .with_output_signaling(keys.signaling.clone().unwrap())
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Dash,
                target: TargetProtection::Cbcs,
                input_keys: &keys.input,
                hls_input_keys: &keys.hls_input,
                output_key: keys.output.as_ref(),
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap()
            .verification
            .passed
    );
    let manifest = output.join("manifest.mpd");
    let verify = || {
        converter().verify_output_signaling(
            &manifest,
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            &keys,
        )
    };
    verify().unwrap();
    let init_path = output.join("track-0000/init.mp4");
    let init = std::fs::read(&init_path).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&init).unwrap();
    let nodes = drmr_core::mp4::walk(&boxes);
    let payload = |kind: [u8; 4]| {
        let node = nodes.iter().find(|node| node.kind() == kind).unwrap();
        node.header.offset + node.header.header_size
    };
    let mut wrong_kid = init.clone();
    wrong_kid[payload(*b"tenc") + 8] ^= 1;
    std::fs::write(&init_path, &wrong_kid).unwrap();
    let mut checks = vec![verify().is_err()];
    let mut wrong_scheme = init.clone();
    wrong_scheme[payload(*b"schm") + 4..payload(*b"schm") + 8].copy_from_slice(b"cenc");
    std::fs::write(&init_path, &wrong_scheme).unwrap();
    checks.push(verify().is_err());
    std::fs::write(&init_path, &init).unwrap();

    // These graphs qualify metadata resolution only, independently of complete-media proof.
    let original = std::fs::read_to_string(&manifest).unwrap();
    let document = roxmltree::Document::parse(&original).unwrap();
    let representation = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "Representation")
        .unwrap();
    let actual = output.join("actual");
    drmr_core::native::create_media_dir_all(&actual).unwrap();
    std::fs::write(actual.join("good-A.mp4"), &init).unwrap();
    std::fs::write(actual.join("a-1.m4s"), &init).unwrap();
    let replacement = "<Representation id=\"A\"><BaseURL>actual/</BaseURL><SegmentTemplate timescale=\"1\" initialization=\"good-$RepresentationID$.mp4\" media=\"a-$Number$.m4s\"><SegmentTimeline><S d=\"1\"/></SegmentTimeline></SegmentTemplate></Representation>";
    let mut relocated = original.clone();
    relocated.replace_range(representation.range(), replacement);
    std::fs::write(&manifest, &relocated).unwrap();
    checks.push(verify().is_ok());
    let mut decoy = original.clone();
    decoy.replace_range(representation.range(), "<Representation id=\"A\"><SegmentTemplate timescale=\"1\" initialization=\"actual/good-$RepresentationID$.mp4\" media=\"track-0000/init.mp4\"><SegmentTimeline><S d=\"1\"/></SegmentTimeline></SegmentTemplate></Representation>");
    std::fs::write(&manifest, decoy).unwrap();
    std::fs::write(actual.join("good-A.mp4"), wrong_kid).unwrap();
    checks.push(verify().is_err());
    std::fs::write(actual.join("good-A.mp4"), &init).unwrap();
    checks.push(verify().is_ok());
    assert_eq!(
        checks, [true; 5],
        "Legacy PlayReady must verify actual KID/scheme and canonical BaseURL/template references, including a valid generated-name decoy"
    );
}
