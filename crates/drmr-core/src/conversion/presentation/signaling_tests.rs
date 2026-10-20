//! Explicit provider metadata must not exceed a platform's command-line limit.
use super::*;
use crate::{
    conversion::OutputSignaling,
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::sync::Arc;

fn largest_pssh() -> Vec<u8> {
    let mut payload = hex::decode("1210000102030405060708090a0b0c0d0e0f").unwrap();
    payload.extend([0x1a, 0xca, 0xff, 0x03]);
    payload.resize(65_504, b'a');
    let result =
        crate::playready::build_pssh_box(&crate::playready::WIDEVINE_SYSTEM_ID, &payload).unwrap();
    assert_eq!(result.len(), 65_536);
    result
}

#[test]
fn maximum_dual_drm_uses_private_file_transport_without_large_arguments() {
    let root = crate::native::private_tempdir().unwrap();
    let audit = root.path().join("argv");
    let program = drmr_test_support::install(
        &root.path().join("packager"),
        &drmr_test_support::ToolSpec {
            exit_code: 1,
            ..drmr_test_support::ToolSpec::default()
        }
        .with("argv_file", audit.to_str().unwrap()),
    )
    .unwrap();
    let metadata = hex::encode(largest_pssh());
    let events = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        program.to_str().unwrap(),
        "ffmpeg",
    )
    .with_output_signaling(
        OutputSignaling::multi_drm(
            Some(&metadata),
            Some("https://example.invalid/license"),
            false,
        )
        .unwrap(),
    );
    let source = root.path().join("source.mp4");
    fs::write(&source, b"not reached before the expected tool failure").unwrap();
    let stage = super::super::files::Stage::new(Some(root.path())).unwrap();
    let tracks = [LocalTrack {
        source: source.clone(),
        kind: TrackKind::Video,
        index: 0,
    }];
    let key = OutputKey::from_hex(
        "000102030405060708090a0b0c0d0e0f",
        "101112131415161718191a1b1c1d1e1f",
    )
    .unwrap();
    assert_eq!(
        command.package_joint(
            &[source],
            stage.path(),
            &TrackConversionRequest {
                tracks: &tracks,
                output: stage.path(),
                format: OutputFormat::Dash,
                target: TargetProtection::Cbcs,
                input_keys: &InputKeys::default(),
                output_key: Some(&key),
                hls_key_uri: None,
                hls_output_key: None,
            },
        ),
        Err(ConversionError("media-tool-failed"))
    );
    let arguments = fs::read_to_string(audit).unwrap();
    let longest = arguments.lines().map(str::len).max().unwrap();
    assert!(longest < 4096, "largest actual argument length: {longest}");
    assert!(!arguments.lines().any(|value| value == "--pssh"));
    let flagfile = Path::new(
        arguments
            .lines()
            .find_map(|value| value.strip_prefix("--flagfile="))
            .unwrap(),
    );
    assert!(
        !flagfile.exists(),
        "failed packaging must clean private flags"
    );
    assert!(!flagfile.parent().unwrap().exists());
    assert!(!flagfile.starts_with(stage.path()));
    let logs = serde_json::to_string(&events.records()).unwrap();
    assert!(!logs.contains(&metadata));
    assert!(!logs.contains(flagfile.to_str().unwrap()));
}
