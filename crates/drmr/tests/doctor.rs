//! Discovery never executes tools; explicit probes use bounded native subprocess fixtures.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn doctor(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["doctor"])
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("DRM_KEY", "secret-must-not-appear")
        .output()
        .unwrap()
}

fn fixture_tool(directory: &Path, name: &str, executable: bool) {
    let path = directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    // Deliberately invalid executable content: discovery must not try running it.
    std::fs::write(&path, "this is not an executable program").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = executable;
}

#[test]
fn missing_dependencies_report_failure_as_json_without_environment_values() {
    let directory = tempfile::tempdir().unwrap();
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, [] as [u8; 0]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], false);
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ffmpeg", "ffprobe", "packager"]
    );
    for tool in tools {
        assert_eq!(tool["available"], false);
        assert_eq!(tool["required"], tool["name"] == "packager");
    }
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("secret-must-not-appear"));
    assert!(!text.contains(directory.path().to_str().unwrap()));
}

#[test]
fn discovery_lists_only_current_runtime_dependencies_without_running_them() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["ffmpeg", "ffprobe", "mp4dump", "packager"] {
        fixture_tool(directory.path(), name, true);
    }
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ffmpeg", "ffprobe", "packager"]
    );
    for tool in tools {
        assert_eq!(tool["available"], true);
    }
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn selected_route_does_not_require_unrelated_tools() {
    let directory = tempfile::tempdir().unwrap();
    let output = doctor(directory.path(), &["--route", "progressive-mp4", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    assert_eq!(report["mode"], "discovery");
    for tool in report["tools"].as_array().unwrap() {
        assert_eq!(tool["required"], false);
        assert_eq!(tool["available"], false);
    }
}

#[test]
fn native_routes_do_not_require_decoder_executables() {
    for (route, needs_packager) in [
        ("all", true),
        ("progressive-mp4", false),
        ("fragmented-mp4", true),
        ("dash-presentation", true),
        ("fmp4-hls-presentation", true),
        ("mpeg-ts-hls", true),
        ("mpeg-ts-dash", true),
        ("mpeg-ts-sample-aes-hls", true),
        ("mpeg-ts-hls-ts", true),
        ("fmp4-segments", false),
        ("mpeg-ts-segments", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        if needs_packager {
            fixture_tool(directory.path(), "packager", true);
        }
        let output = doctor(directory.path(), &["--route", route, "--json"]);
        assert_eq!(output.status.code(), Some(0), "{route}: {output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ready"], true);
        assert_eq!(report["tools"][0]["required"], false);
        assert_eq!(report["tools"][1]["required"], false);
        assert_eq!(report["tools"][2]["required"], needs_packager);
    }
}

#[test]
fn native_probe_skips_optional_decoder_executables_and_decode_makes_them_required() {
    let directory = tempfile::tempdir().unwrap();
    fixture_tool(directory.path(), "ffmpeg", true);
    fixture_tool(directory.path(), "ffprobe", true);
    let native = doctor(
        directory.path(),
        &["--probe", "--route", "progressive-mp4", "--json"],
    );
    assert_eq!(native.status.code(), Some(0), "{native:?}");
    let report: Value = serde_json::from_slice(&native.stdout).unwrap();
    assert_eq!(report["ready"], true);
    for tool in &report["tools"].as_array().unwrap()[..2] {
        assert_eq!(tool["available"], true);
        assert_eq!(tool["required"], false);
        assert_eq!(tool["version_status"], "not-checked");
        assert_eq!(tool["feature_status"], "not-checked");
    }
    let decoded = doctor(
        directory.path(),
        &["--decode", "--route", "progressive-mp4", "--json"],
    );
    assert_eq!(decoded.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&decoded.stdout).unwrap();
    assert_eq!(report["tools"][0]["required"], true);
    assert_eq!(report["tools"][1]["required"], true);
    assert_eq!(report["tools"][2]["required"], false);

    let absent = tempfile::tempdir().unwrap();
    let decoded = doctor(
        absent.path(),
        &["--decode", "--route", "progressive-mp4", "--json"],
    );
    assert_eq!(decoded.status.code(), Some(1));
}

fn probing_tool(directory: &Path, name: &str, version: &str, options: &str) {
    let components = if name == "ffmpeg" {
        "Muxer data [raw data]:\nMuxer hash [Hash testing]:\nEncoder rawvideo [raw video]:\nEncoder pcm_s16le [PCM signed 16-bit little-endian]:\n-thread_queue_size"
    } else {
        ""
    };
    probing_tool_with_components(directory, name, version, options, components);
}

fn probing_tool_with_components(
    directory: &Path,
    name: &str,
    version: &str,
    options: &str,
    components: &str,
) {
    drmr_test_support::install(
        &directory.join(name),
        &drmr_test_support::ToolSpec {
            stdout: format!(
                "{name} version {version}\n{options}\n{components}\nuntrusted-secret-must-not-appear\n"
            )
            .into_bytes(),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn decoded_comparison_requires_both_muxers_and_both_default_encoders() {
    let components = [
        ("data_muxer", "Muxer data [raw data]:"),
        ("hash_muxer", "Muxer hash [Hash testing]:"),
        ("rawvideo_encoder", "Encoder rawvideo [raw video]:"),
        (
            "pcm_s16le_encoder",
            "Encoder pcm_s16le [PCM signed 16-bit little-endian]:",
        ),
        ("thread_queue_size", "-thread_queue_size"),
    ];
    for (missing, _) in components {
        let directory = tempfile::tempdir().unwrap();
        probing_tool(
            directory.path(),
            "ffprobe",
            "9.0.1",
            "-show_packets\n-show_entries\n-show_data_hash\n-decryption_keys",
        );
        let present = components
            .iter()
            .filter(|(name, _)| *name != missing)
            .map(|(_, header)| *header)
            .collect::<Vec<_>>()
            .join("\n");
        probing_tool_with_components(
            directory.path(),
            "ffmpeg",
            "9.0.1",
            "-encryption_scheme\n-decryption_key\n-decryption_keys",
            &present,
        );
        let output = doctor(
            directory.path(),
            &[
                "--probe",
                "--decode",
                "--route",
                "progressive-mp4",
                "--json",
            ],
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1), "missing {missing}: {report}");
        assert_eq!(report["ready"], false);
        assert_eq!(report["tools"][0]["feature_status"], "missing");
        assert!(
            report["tools"][0]["features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|feature| feature["name"] == missing && feature["available"] == false)
        );
    }
}

fn probing_pair(directory: &Path, version: &str, ffmpeg_options: &str) {
    let ffmpeg_options = format!("{ffmpeg_options}\n-decryption_keys");
    probing_tool(directory, "ffmpeg", version, &ffmpeg_options);
    probing_tool(
        directory,
        "ffprobe",
        "9.0.1",
        "-show_packets\n-show_entries\n-show_data_hash\n-decryption_keys",
    );
}

#[test]
fn complete_probe_requires_only_the_three_runtime_tools() {
    let directory = tempfile::tempdir().unwrap();
    probing_pair(
        directory.path(),
        "9.0.1",
        "-encryption_scheme\n-decryption_key",
    );
    probing_tool(
        directory.path(),
        "packager",
        "v3.8.0",
        "--enable_raw_key_encryption\n--enable_raw_key_decryption\n--protection_scheme\n--segment_duration\n--mpd_output\n--hls_master_playlist_output",
    );
    let output = doctor(directory.path(), &["--probe", "--decode", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    for tool in tools {
        assert_eq!(tool["required"], true);
        assert_eq!(tool["version_status"], "supported");
        assert_eq!(tool["feature_status"], "available");
        assert_eq!(tool["failure_code"], Value::Null);
    }
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("untrusted-secret-must-not-appear"));
    }
}

#[test]
fn explicit_probe_reports_resolved_paths_versions_and_required_features() {
    let directory = tempfile::tempdir().unwrap();
    probing_pair(
        directory.path(),
        "9.0.2",
        "-encryption_scheme\n-decryption_key",
    );
    let output = doctor(
        directory.path(),
        &[
            "--probe",
            "--decode",
            "--route",
            "progressive-mp4",
            "--json",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["mode"], "probe");
    assert_eq!(report["media_compatibility_checked"], false);
    assert_eq!(report["tools"][0]["version"], "9.0.2");
    assert_eq!(report["tools"][0]["version_status"], "supported");
    assert_eq!(report["tools"][0]["feature_status"], "available");
    let resolved = drmr_test_support::program_path(&directory.path().join("ffmpeg"))
        .canonicalize()
        .unwrap();
    assert_eq!(report["tools"][0]["path"], resolved.to_str().unwrap());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all.contains("untrusted-secret"));
    assert!(!all.contains("secret-must-not-appear"));
}

#[test]
fn release_tag_banners_are_recognized_without_guessing_unversioned_builds() {
    for (version, status, parsed) in [
        ("n9.0.2-17-g2a571b6068-20260930", "supported", Some("9.0.2")),
        ("n8.0.0", "below-minimum", Some("8.0.0")),
        ("n9.0.0", "below-minimum", Some("9.0.0")),
        ("N-123456-g2a571b6068", "unrecognized", None),
        ("unknown", "unrecognized", None),
    ] {
        let directory = tempfile::tempdir().unwrap();
        for name in ["ffmpeg", "ffprobe"] {
            let options = if name == "ffmpeg" {
                "-encryption_scheme\n-decryption_key\n-decryption_keys"
            } else {
                "-show_packets\n-show_entries\n-show_data_hash\n-decryption_keys"
            };
            probing_tool(directory.path(), name, version, options);
        }
        let output = doctor(
            directory.path(),
            &[
                "--probe",
                "--decode",
                "--route",
                "progressive-mp4",
                "--json",
            ],
        );
        assert_eq!(output.status.code(), Some(i32::from(status != "supported")));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ready"], status == "supported");
        assert_eq!(report["cleanup_status"], "confirmed");
        assert_eq!(report["media_compatibility_checked"], false);
        for tool in &report["tools"].as_array().unwrap()[..2] {
            assert_eq!(tool["version_status"], status);
            assert_eq!(tool["version"].as_str(), parsed);
            assert_eq!(
                tool["feature_status"],
                if parsed.is_some() {
                    "available"
                } else {
                    "not-checked"
                }
            );
        }
    }
}

#[test]
fn installed_tools_pass_the_actual_progressive_route_probe() {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args([
            "doctor",
            "--probe",
            "--decode",
            "--route",
            "progressive-mp4",
            "--json",
        ])
        .env("DRM_KEY", "real-probe-secret-must-not-appear")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "actual installed tool probe failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["mode"], "probe");
    assert_eq!(report["profile"], "progressive-mp4");
    assert_eq!(report["ready"], true);
    assert_eq!(report["cleanup_status"], "confirmed");
    assert_eq!(report["media_compatibility_checked"], false);
    for (tool, name) in report["tools"].as_array().unwrap()[..2]
        .iter()
        .zip(["ffmpeg", "ffprobe"])
    {
        assert_eq!(tool["name"], name);
        assert_eq!(tool["required"], true);
        assert_eq!(tool["available"], true);
        assert_eq!(tool["version_status"], "supported");
        assert_eq!(tool["feature_status"], "available");
        assert_eq!(tool["failure_code"], Value::Null);
        let expected: &[&str] = if name == "ffmpeg" {
            &[
                "decryption_key",
                "decryption_keys",
                "data_muxer",
                "hash_muxer",
                "rawvideo_encoder",
                "pcm_s16le_encoder",
                "thread_queue_size",
            ]
        } else {
            &[
                "show_packets",
                "show_data_hash",
                "show_entries",
                "decryption_keys",
            ]
        };
        let features = tool["features"].as_array().unwrap();
        assert_eq!(features.len(), expected.len());
        for name in expected {
            assert!(
                features
                    .iter()
                    .any(|f| f["name"] == *name && f["available"] == true)
            );
        }
    }
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("real-probe-secret-must-not-appear"));
    }
}

#[test]
fn keyed_routes_require_the_plural_kid_dictionary_option_in_both_tools() {
    let mut actual = Vec::new();
    for lacking in ["ffmpeg", "ffprobe"] {
        let directory = tempfile::tempdir().unwrap();
        for name in ["ffmpeg", "ffprobe"] {
            let options = if name == "ffmpeg" {
                "-encryption_scheme\n-decryption_key"
            } else {
                "-show_packets\n-show_entries\n-show_data_hash"
            };
            let options = if lacking == name {
                options.to_owned()
            } else {
                format!("{options}\n-decryption_keys")
            };
            probing_tool(directory.path(), name, "9.0.1", &options);
        }
        let output = doctor(
            directory.path(),
            &[
                "--probe",
                "--decode",
                "--route",
                "progressive-mp4",
                "--json",
            ],
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        let index = usize::from(lacking == "ffprobe");
        actual.push((
            lacking,
            output.status.code(),
            report["tools"][index]["feature_status"].clone(),
            report["ready"].clone(),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                "ffmpeg",
                Some(1),
                Value::from("missing"),
                Value::Bool(false)
            ),
            (
                "ffprobe",
                Some(1),
                Value::from("missing"),
                Value::Bool(false)
            )
        ]
    );
}

#[test]
fn an_old_version_and_missing_options_are_separate_failures() {
    for (version, options, version_status, feature_status) in [
        (
            "8.0.0",
            "-encryption_scheme\n-decryption_key",
            "below-minimum",
            "available",
        ),
        ("99.0.0", "-encryption_scheme", "supported", "missing"),
        (
            "malformed",
            "-encryption_scheme\n-decryption_key",
            "unrecognized",
            "not-checked",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        probing_pair(directory.path(), version, options);
        let output = doctor(
            directory.path(),
            &[
                "--probe",
                "--decode",
                "--route",
                "progressive-mp4",
                "--json",
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tools"][0]["version_status"], version_status);
        assert_eq!(report["tools"][0]["feature_status"], feature_status);
    }
}

#[test]
fn probes_bound_hanging_and_oversized_version_output() {
    for oversized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut spec = drmr_test_support::ToolSpec::default();
        if oversized {
            spec.values
                .insert("stdout_repeat".into(), (2 * 1024 * 1024).to_string());
        } else {
            spec.sleep_ms = 8000;
        }
        drmr_test_support::install(&directory.path().join("ffmpeg"), &spec).unwrap();
        probing_tool(
            directory.path(),
            "ffprobe",
            "9.0.1",
            "-show_packets\n-show_entries\n-show_data_hash\n-decryption_keys",
        );
        let started = std::time::Instant::now();
        let output = doctor(
            directory.path(),
            &[
                "--probe",
                "--decode",
                "--route",
                "progressive-mp4",
                "--json",
            ],
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(7));
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["tools"][0]["failure_code"],
            if oversized {
                "probe-output-limit"
            } else {
                "probe-timeout"
            }
        );
    }
}

// Linux permits non-UTF-8 names; macOS APFS rejects this fixture at mkdir.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_tool_paths_are_reported_without_panics_or_ambiguous_execution() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join(std::ffi::OsString::from_vec(b"tools-\xff".to_vec()));
    std::fs::create_dir(&directory).unwrap();
    fixture_tool(&directory, "ffmpeg", true);
    let output = doctor(&directory, &["--probe", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tools"][0]["available"], true);
    assert_eq!(report["tools"][0]["path"], Value::Null);
    assert_eq!(report["tools"][0]["path_status"], "not-utf8");
    assert_eq!(report["tools"][0]["failure_code"], "probe-path-not-utf8");
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn default_report_explains_next_steps_for_missing_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let output = doctor(directory.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    for name in ["ffmpeg", "ffprobe", "packager"] {
        assert!(text.contains(name));
    }
    assert!(text.contains("PATH"));
    assert!(!text.contains("Bento4"));
    assert!(!text.contains("mp4dump"));
    assert!(text.contains("Shaka Packager"));
    assert!(!text.contains("secret-must-not-appear"));
}

#[cfg(unix)]
#[test]
fn directories_and_non_executable_files_do_not_count_as_available() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("ffmpeg")).unwrap();
    fixture_tool(directory.path(), "ffprobe", false);
    fixture_tool(directory.path(), "packager", true);
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(tools[0]["available"], false);
    assert_eq!(tools[1]["available"], false);
    assert_eq!(tools[2]["available"], true);
}

#[test]
fn root_help_shows_getting_started_examples_and_command_help_is_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for example in [
        "drmr doctor",
        "drmr analyze media.mp4 --metadata-only",
        "drmr plan media.mp4 --to cbcs",
        "drmr convert",
        "drmr verify",
        "--download-dir",
        "drmr analyze URL --download-dir SOURCE_DIR",
    ] {
        assert!(text.contains(example), "missing {example}: {text}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["help", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--json"));
}
