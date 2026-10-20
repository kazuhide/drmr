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
    Converter::new(runner(), "packager", "ffmpeg")
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
