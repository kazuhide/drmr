//! Test-only media generation and independent HLS resource checks.
use aes::cipher::{BlockDecryptMut as _, KeyIvInit as _, block_padding::Pkcs7};
use drmr_core::{
    conversion::{
        Aes128Key, ConversionReport, Converter, DecodeEvidence, HlsOutputKey, ProcessingMode,
        ValidationDepth, VerificationReport, VerificationScope, VerificationSource,
        key_file::{BundleOperatorKeys, OperatorKeys},
        plan::{
            ExecutionPlan, ExecutionPlanRequest,
            bundle::{BundleOutputIntent, ManifestValidation},
        },
        processing::BoundaryHandling,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const AES_KEY: [u8; 16] = [0x31; 16];

pub fn converter(mode: ProcessingMode) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_processing(mode)
}

pub fn native_converter(mode: ProcessingMode) -> (Converter, Arc<Collector>) {
    let observed = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), observed.clone()));
    (
        Converter::new(
            runner,
            "a-packager-that-must-not-run",
            "an-ffmpeg-that-must-not-run",
        )
        .with_ffprobe("a-probe-that-must-not-run")
        .with_processing(mode),
        observed,
    )
}

pub fn assert_native(observed: &Collector) {
    assert!(
        observed.records().iter().all(|record| {
            record.get("event").and_then(serde_json::Value::as_str) != Some("command")
        }),
        "native operation must not attempt an external media tool"
    );
}

pub fn fixture_mp4(path: &Path) {
    fs::copy(clip(), path).unwrap();
}

fn clip() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4")
}

pub fn transport(directory: &Path) -> PathBuf {
    fs::create_dir(directory).unwrap();
    let input = directory.join("media.m3u8");
    // The attributed ten-second BBB clip has a different boundary schedule from the
    // ordinary two-second output target. Generation is outside the converter runner.
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(clip())
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-hls_time",
            "3",
            "-hls_playlist_type",
            "vod",
            "-start_number",
            "7",
            "-hls_segment_filename",
        ])
        .arg(directory.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(result.status.success(), "BBB HLS fixture generation failed");
    let original = segments(&input);
    assert!(original.len() >= 3);
    assert!(
        original
            .iter()
            .any(|segment| segment.duration > Duration::from_millis(2100))
    );
    input
}

pub fn source<'a>(input: &'a Path, keys: &'a OperatorKeys) -> VerificationSource<'a> {
    VerificationSource {
        input,
        input_keys: &keys.input,
        hls_keys: &keys.hls_input,
    }
}

pub fn saved_segments(command: &Converter, input: &Path, output: &Path) -> ExecutionPlan {
    command
        .create_execution_plan(ExecutionPlanRequest {
            input: source(input, &OperatorKeys::default()),
            output,
            format: None,
            target: TargetProtection::Clear,
        })
        .unwrap()
}

pub fn output(
    id: &str,
    format: OutputFormat,
    target: TargetProtection,
    processing: Option<ProcessingMode>,
) -> BundleOutputIntent {
    BundleOutputIntent {
        id: id.into(),
        format,
        target,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing,
    }
}

pub fn bundle_keys() -> BundleOperatorKeys {
    let mut keys = BundleOperatorKeys::default();
    let mut encrypted = OperatorKeys::default();
    encrypted.hls_output = Some(
        HlsOutputKey::new(
            "https://keys.example.invalid/output.key",
            Aes128Key::from_hex(&hex::encode(AES_KEY)).unwrap(),
        )
        .unwrap(),
    );
    keys.outputs.insert("aes".into(), encrypted);
    keys
}

pub fn assert_verified(report: &VerificationReport, depth: ValidationDepth) {
    assert!(report.passed && report.checks.is_pass());
    assert_eq!(report.required_depth, depth);
    assert_eq!(report.scope, VerificationScope::Comparison);
    if depth == ValidationDepth::Decoded {
        assert_eq!(
            report.decode_evidence,
            DecodeEvidence::ContentCompared { equal: true }
        );
    } else {
        assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    }
    assert!(!report.license_playback_verified);
}

pub fn assert_processing(
    report: &ConversionReport,
    mode: ProcessingMode,
    boundaries: BoundaryHandling,
) {
    let actual = report.processing.as_ref().expect("completed route report");
    assert_eq!(actual.mode, mode);
    assert_eq!(actual.boundaries, boundaries);
    if report.verification.required_depth == ValidationDepth::Native
        && mode == ProcessingMode::Segments
    {
        assert!(actual.required_tools.is_empty());
    }
}

pub struct Segment {
    pub path: PathBuf,
    pub duration: Duration,
    iv: Option<[u8; 16]>,
}

// This small fixture reader deliberately does not use the production manifest parser.
pub fn segments(input: &Path) -> Vec<Segment> {
    let text = fs::read_to_string(input).unwrap();
    if !text.contains("#EXTINF:") {
        let children: Vec<_> = text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert_eq!(children.len(), 1, "fixture has one muxed HLS rendition");
        return segments(&input.parent().unwrap().join(children[0]));
    }
    let mut sequence = 0_u128;
    let mut duration = None;
    let mut encrypted = false;
    let mut explicit_iv = None;
    let mut result = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = value.parse().unwrap();
        } else if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            encrypted = value.split(',').any(|value| value == "METHOD=AES-128");
            explicit_iv = value
                .split(',')
                .find_map(|attribute| attribute.strip_prefix("IV=0x"))
                .map(|value| hex::decode(value).unwrap().try_into().unwrap());
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            duration = Some(Duration::from_secs_f64(
                value.split(',').next().unwrap().parse().unwrap(),
            ));
        } else if !line.is_empty() && !line.starts_with('#') {
            result.push(Segment {
                path: input.parent().unwrap().join(line),
                duration: duration.take().expect("each resource has an EXTINF"),
                iv: encrypted.then(|| explicit_iv.unwrap_or(sequence.to_be_bytes())),
            });
            sequence += 1;
        }
    }
    assert!(!result.is_empty());
    result
}

pub fn assert_original_segments(input: &Path, output: &Path, encrypted: bool) {
    let original = segments(input);
    let result = segments(output);
    assert_eq!(result.len(), original.len());
    for (before, after) in original.iter().zip(&result) {
        assert_eq!(before.duration, after.duration);
        let mut bytes = fs::read(&after.path).unwrap();
        let decoded = if encrypted {
            let iv = after
                .iv
                .expect("encrypted resource must declare its key domain");
            cbc::Decryptor::<aes::Aes128>::new(&AES_KEY.into(), &iv.into())
                .decrypt_padded_mut::<Pkcs7>(&mut bytes)
                .unwrap()
        } else {
            assert!(after.iv.is_none());
            &bytes
        };
        // Byte equality after independent CBC removal proves resource membership,
        // complete original TS packets, sample payloads, and packet clocks together.
        assert_eq!(decoded, fs::read(&before.path).unwrap());
    }
}
