use super::*;
use sha2::{Digest as _, Sha256};

fn backend(root: &Path, spec: &drmr_test_support::ToolSpec) -> Backend {
    let tool = drmr_test_support::install(&root.join("ffmpeg"), spec).unwrap();
    Backend {
        stop: crate::concurrency::Stop::new(),
        runner: Arc::new(
            ToolRunner::new(
                Arc::new(crate::toolrun::Secrets::new()),
                Arc::new(crate::events::NoSink),
            )
            // These subprocess fixtures test stream and cleanup behavior, not
            // contention with unrelated media tests' process-wide admission.
            .with_limiter(Arc::new(crate::resources::Limiter::new(
                2,
                1024 * 1024 * 1024,
            ))),
        ),
        packager: String::new(),
        ffmpeg: tool.to_str().unwrap().into(),
        ffprobe: String::new(),
        timeout: Duration::from_secs(20),
        validation_depth: crate::conversion::ValidationDepth::Decoded,
        measurement: None,
        activity: None,
        preparation: None,
        scheduling_hardware: None,
    }
}

#[test]
fn decoded_hash_streams_complete_raw_output_larger_than_the_capture_limit() {
    let root = tempfile::tempdir().unwrap();
    let size = crate::toolrun::DEFAULT_STDOUT_BYTES + 1;
    let command = backend(
        root.path(),
        &drmr_test_support::ToolSpec {
            stderr: b"adler32=00000000\n".to_vec(),
            ..Default::default()
        }
        .with("stdout_repeat", size.to_string()),
    );
    let input = root.path().join("input.mp4");
    std::fs::write(&input, b"fixture").unwrap();
    let mut expected = Sha256::new();
    for _ in 0..size / 8192 {
        expected.update([b'x'; 8192]);
    }
    expected.update(vec![b'x'; size % 8192]);
    assert_eq!(
        command.hash_stream(&input, 0, false, None).unwrap(),
        hex::encode(expected.finalize())
    );
}

#[test]
fn decoded_hash_never_returns_a_digest_before_complete_tool_success() {
    for (spec, timeout, expected) in [
        (
            drmr_test_support::ToolSpec {
                stdout: b"raw decoded bytes".to_vec(),
                stderr: b"adler32=00000000\n".to_vec(),
                exit_code: 7,
                ..Default::default()
            },
            Duration::from_secs(20),
            "media-tool-failed",
        ),
        (
            drmr_test_support::ToolSpec {
                stdout: b"raw decoded bytes".to_vec(),
                stderr: b"adler32=00000000\n".to_vec(),
                sleep_ms: 60_000,
                ..Default::default()
            },
            Duration::from_millis(100),
            "media-tool-timeout",
        ),
        (
            drmr_test_support::ToolSpec {
                stdout: b"raw decoded bytes".to_vec(),
                ..Default::default()
            },
            Duration::from_secs(20),
            "invalid-verification-result",
        ),
        (
            drmr_test_support::ToolSpec {
                stdout: b"raw decoded bytes".to_vec(),
                stderr: b"adler32=not-a-checksum\n".to_vec(),
                ..Default::default()
            },
            Duration::from_secs(20),
            "invalid-verification-result",
        ),
        (
            drmr_test_support::ToolSpec {
                stdout: b"raw decoded bytes".to_vec(),
                stderr: b"adler32=00000000\n".to_vec(),
                ..Default::default()
            }
            .with(
                "stderr_repeat",
                (crate::toolrun::DEFAULT_STDERR_BYTES + 1).to_string(),
            ),
            Duration::from_secs(20),
            "media-output-limit",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut command = backend(root.path(), &spec);
        command.timeout = timeout;
        assert_eq!(
            command
                .hash_stream(&root.path().join("input.mp4"), 0, false, None)
                .unwrap_err(),
            ConversionError(expected)
        );
    }
}

#[test]
fn decoded_hash_honors_cancellation_and_encoded_hash_keeps_its_format() {
    let root = tempfile::tempdir().unwrap();
    let expected = "1234567890abcdef".repeat(4);
    let command = backend(
        root.path(),
        &drmr_test_support::ToolSpec {
            stdout: format!("SHA256={expected}\n").into_bytes(),
            ..Default::default()
        },
    );
    let input = root.path().join("input.mp4");
    assert_eq!(
        command.hash_stream(&input, 0, true, None).unwrap(),
        expected
    );
    command.stop.set();
    assert_eq!(
        command.hash_stream(&input, 0, false, None).unwrap_err(),
        ConversionError("cancelled")
    );
}
