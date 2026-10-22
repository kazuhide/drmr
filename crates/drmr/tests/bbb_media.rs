//! Real-media regression using the attributed, pinned ten-second Big Buck Bunny fixture.

use std::path::Path;
use std::process::Command;

#[test]
fn the_bbb_fixture_is_pinned_and_has_five_two_second_video_segments() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    assert!(
        fixture.is_file(),
        "the attributed BBB clip must be checked in"
    );
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(fixture.with_file_name("regression.py"))
        .arg("--verify-fixture")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_real_cli_saves_converts_and_independently_verifies_the_bbb_multi_drm_plan() {
    let root = tempfile::tempdir().unwrap();
    let harness = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/regression.py");
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(harness)
        .arg("--drmr")
        .arg(env!("CARGO_BIN_EXE_drmr"))
        .arg("--output")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["success"], true);
    assert_eq!(report["workflow"], "saved-plan-multi-output");
    assert_eq!(report["source_and_plan_unchanged"], true);
    assert_eq!(report["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(report["video_segments"], 5);
    assert_eq!(report["packaged_tracks"], 2);
    assert_eq!(report["complete_presentation_identity"], "pass");
    assert_eq!(report["complete_packet_timing"], "pass");
    for stage in ["analysis", "planning", "packaging", "verification"] {
        assert!(report["timing_seconds"][stage].as_f64().unwrap() > 0.0);
        let memory = &report["memory"][stage];
        match memory["status"].as_str().unwrap() {
            "sampled" => {
                assert!(memory["samples"].as_u64().unwrap() > 0);
                assert!(
                    memory["cli_parent_sampled_peak_rss_bytes"]
                        .as_u64()
                        .unwrap()
                        > 0
                );
                assert!(memory["child_process_tree_sampled_peak_rss_bytes"].is_u64());
            }
            "unmeasured" => {
                assert!(memory["cli_parent_sampled_peak_rss_bytes"].is_null());
                assert!(memory["child_process_tree_sampled_peak_rss_bytes"].is_null());
                assert!(memory["reason"].is_string());
            }
            status => panic!("unexpected memory evidence status: {status}"),
        }
    }
    assert_eq!(report["evidence"]["real_license"], "not-verified");
    assert_eq!(
        report["evidence"]["windows"],
        if cfg!(windows) {
            "measured-native"
        } else {
            "not-tested"
        }
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn the_real_cli_verifies_all_sixteen_bbb_protection_routes() {
    let root = tempfile::tempdir().unwrap();
    let harness =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/conversion_regression.py");
    let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .arg(harness)
        .arg("--drmr")
        .arg(env!("CARGO_BIN_EXE_drmr"))
        .arg("--output")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["success"], true);
    assert_eq!(report["routes"].as_array().unwrap().len(), 16);
    for route in report["routes"].as_array().unwrap() {
        assert_eq!(route["passed"], true);
        assert_eq!(route["verification"]["checks"]["packet_timing_equal"], true);
    }
    assert_eq!(report["source_video_segments"], 5);
    assert_eq!(
        report["evidence"]["windows"],
        if cfg!(windows) {
            "measured-native"
        } else {
            "not-tested"
        }
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}
