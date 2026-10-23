use super::*;
use crate::{
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use drmr_test_support::ToolSpec;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

fn video_profile() -> Value {
    json!({"streams":[{
        "index":3,"codec_type":"video","codec_name":"h264","time_base":"1/90000",
        "nb_read_packets":"1","width":16,"height":16,
        "extradata":"\n00000000: 0000 0001 6780 0000 0001 6880  ............\n"
    }]})
}

fn backend(root: &Path, response: ToolSpec) -> (Backend, Arc<Collector>) {
    let tool = drmr_test_support::install(
        &root.join("probe"),
        &response.with("argv_file", root.join("argv").to_str().unwrap()),
    )
    .unwrap();
    let sink = Arc::new(Collector::default());
    let backend = Backend {
        stop: crate::concurrency::Stop::new(),
        runner: Arc::new(
            ToolRunner::new(Arc::new(Secrets::new()), sink.clone()).with_limiter(Arc::new(
                crate::resources::Limiter::new(2, 1024 * 1024 * 1024),
            )),
        ),
        packager: String::new(),
        ffmpeg: String::new(),
        ffprobe: tool.to_str().unwrap().into(),
        timeout: Duration::from_secs(20),
        measurement: None,
        activity: None,
    };
    std::fs::write(root.join("input.ts"), b"private fixture").unwrap();
    (backend, sink)
}

fn view(input: &Path) -> View<'_> {
    View {
        input,
        metadata: input,
        index: 3,
        key: None,
        transport: true,
        native: None,
        unedited: false,
    }
}

fn commands(sink: &Collector) -> usize {
    sink.records()
        .iter()
        .filter(|record| record["event"] == "command")
        .count()
}

#[test]
fn segment_profile_reads_configuration_and_count_with_one_selected_probe() {
    let root = tempfile::tempdir().unwrap();
    let (backend, sink) = backend(
        root.path(),
        ToolSpec {
            stdout: video_profile().to_string().into_bytes(),
            ..Default::default()
        },
    );
    let (profile, config) = backend
        .transport_segment_profile(view(&root.path().join("input.ts")))
        .unwrap();
    assert_eq!(
        config,
        canonical::Config::Video {
            sps: vec![0x67, 0x80],
            pps: vec![0x68, 0x80]
        }
    );
    assert_eq!(profile["nb_read_packets"], "1");
    assert!(profile.get("extradata").is_none());
    assert_eq!(commands(&sink), 1);
    let argv = std::fs::read_to_string(root.path().join("argv")).unwrap();
    assert!(argv.contains("-select_streams\n3\n"));
    assert!(argv.contains("-count_packets\n"));
    assert!(argv.contains("-show_data\n"));
    assert!(!argv.contains("-show_packets"));
    assert!(argv.contains("-protocol_whitelist\nfile,crypto\n"));
    assert!(argv.contains(&format!(
        "stream={},extradata:{}",
        backend::PROFILE_FIELDS,
        backend::PROFILE_SIDE_DATA
    )));
    assert!(sink.records().iter().all(|record| {
        !serde_json::to_string(record)
            .unwrap()
            .contains("private fixture")
    }));
}

#[test]
fn segment_profile_preserves_aac_configuration_without_requesting_another_probe() {
    for rate in ["44100", "48000"] {
        let root = tempfile::tempdir().unwrap();
        let mut profile = video_profile();
        profile["streams"][0] = json!({"index":3,"codec_type":"audio","codec_name":"aac",
            "time_base":"1/90000","nb_read_packets":"2","sample_rate":rate,"channels":8});
        let (backend, sink) = backend(
            root.path(),
            ToolSpec {
                stdout: profile.to_string().into_bytes(),
                ..Default::default()
            },
        );
        let (_, config) = backend
            .transport_segment_profile(view(&root.path().join("input.ts")))
            .unwrap();
        assert_eq!(
            config,
            canonical::Config::Audio {
                channels: 7,
                sample_rate: rate.parse().unwrap()
            }
        );
        assert_eq!(commands(&sink), 1);
    }
}

#[test]
fn segment_profile_refuses_missing_or_inconsistent_stream_and_codec_facts() {
    let mut cases = Vec::new();
    for count in [
        Value::Null,
        json!("0"),
        json!("-1"),
        json!("18446744073709551616"),
        json!(1),
    ] {
        let mut data = video_profile();
        data["streams"][0]["nb_read_packets"] = count;
        cases.push((data, "media-packets-missing"));
    }
    for (field, value, error) in [
        ("index", json!(2), "stream-selector-mismatch"),
        ("codec_type", json!("text"), "media-packets-missing"),
        (
            "codec_name",
            json!("hevc"),
            "ts-codec-configuration-not-supported",
        ),
        (
            "time_base",
            json!("1/1000"),
            "ts-clock-timescale-not-supported",
        ),
        (
            "extradata",
            Value::Null,
            "ts-codec-configuration-not-supported",
        ),
        (
            "extradata",
            json!("partial"),
            "invalid-ts-access-unit-evidence",
        ),
    ] {
        let mut data = video_profile();
        data["streams"][0][field] = value;
        cases.push((data, error));
    }
    cases.push((json!({"streams": []}), "single-track-mp4-required"));
    cases.push((json!({"streams": [video_profile()["streams"][0].clone(), video_profile()["streams"][0].clone()]}), "single-track-mp4-required"));
    for (data, error) in cases {
        let root = tempfile::tempdir().unwrap();
        let (backend, sink) = backend(
            root.path(),
            ToolSpec {
                stdout: data.to_string().into_bytes(),
                ..Default::default()
            },
        );
        assert_eq!(
            backend
                .transport_segment_profile(view(&root.path().join("input.ts")))
                .unwrap_err(),
            ConversionError(error)
        );
        assert_eq!(commands(&sink), 1);
    }
}

#[test]
fn segment_profile_requires_complete_successful_bounded_output() {
    for (spec, timeout, error) in [
        (
            ToolSpec {
                stdout: video_profile().to_string().into_bytes(),
                exit_code: 7,
                ..Default::default()
            },
            Duration::from_secs(20),
            "media-tool-failed",
        ),
        (
            ToolSpec {
                stdout: b"{\"streams\":[".to_vec(),
                ..Default::default()
            },
            Duration::from_secs(20),
            "invalid-media-probe",
        ),
        (
            ToolSpec::default().with("stdout_repeat", (PROFILE_LIMIT + 1).to_string()),
            Duration::from_secs(20),
            "media-output-limit",
        ),
        (
            ToolSpec {
                stdout: video_profile().to_string().into_bytes(),
                ..Default::default()
            }
            .with(
                "stderr_repeat",
                (crate::toolrun::DEFAULT_STDERR_BYTES + 1).to_string(),
            ),
            Duration::from_secs(20),
            "media-output-limit",
        ),
        (
            ToolSpec {
                stdout: video_profile().to_string().into_bytes(),
                sleep_ms: 60_000,
                ..Default::default()
            },
            Duration::from_millis(100),
            "media-tool-timeout",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (mut backend, _) = backend(root.path(), spec);
        backend.timeout = timeout;
        assert_eq!(
            backend
                .transport_segment_profile(view(&root.path().join("input.ts")))
                .unwrap_err(),
            ConversionError(error)
        );
    }
}

#[test]
fn segment_profile_never_changes_key_metadata_or_edit_list_roles() {
    let root = tempfile::tempdir().unwrap();
    let (backend, sink) = backend(root.path(), ToolSpec::default());
    let input = root.path().join("input.ts");
    let metadata = root.path().join("other.ts");
    let key = crate::conversion::ContentKey::from_hex(
        "000102030405060708090a0b0c0d0e0f",
        "42424242424242424242424242424242",
    )
    .unwrap();
    for candidate in [
        View {
            transport: false,
            ..view(&input)
        },
        View {
            metadata: &metadata,
            ..view(&input)
        },
        View {
            key: Some(&key),
            ..view(&input)
        },
        View {
            unedited: true,
            ..view(&input)
        },
    ] {
        assert_eq!(
            backend.transport_segment_profile(candidate).unwrap_err(),
            ConversionError("ts-segment-probe-profile-not-supported")
        );
    }
    backend.stop.set();
    assert_eq!(
        backend.transport_segment_profile(view(&input)).unwrap_err(),
        ConversionError("cancelled")
    );
    assert_eq!(commands(&sink), 0);
}
