//! Saved-plan modes keep media intent separate from runtime secrets and diagnostics.
use serde_json::Value;
use std::io::Write as _;
use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}

fn usage(result: &Output) {
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(result.stdout, [] as [u8; 0]);
}

fn failure(result: &Output, command: &str, expected: &str, exit: i32) -> Value {
    assert_eq!(result.status.code(), Some(exit));
    let report: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&result.stderr)));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], command);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], expected);
    for bytes in [&result.stdout, &result.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("confidential-sentinel"));
    }
    report
}

#[test]
fn analyze_help_explains_full_input_checks_separately_from_inspect() {
    let result = invoke(&["analyze", "--help"]);
    assert!(result.status.success());
    let help = String::from_utf8(result.stdout).unwrap();
    for option in [
        "--download-dir",
        "--fetch-policy",
        "--keys-file",
        "--json",
        "--measure",
    ] {
        assert!(help.contains(option), "{help}");
    }
    let result = invoke(&["inspect", "--help"]);
    assert!(result.status.success());
    assert!(
        !String::from_utf8(result.stdout)
            .unwrap()
            .contains("--download-dir")
    );
}

#[test]
fn analyze_url_requires_a_new_download_directory_before_acquisition() {
    let result = invoke(&[
        "analyze",
        "https://example.invalid/stream.m3u8?confidential-sentinel",
        "--json",
    ]);
    failure(&result, "analyze", "download-directory-required", 2);
}

#[test]
fn analyze_rejects_remote_options_for_local_input_without_creating_a_directory() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("confidential-sentinel.mp4");
    let download = root.path().join("source");
    for flag in ["--download-dir", "--fetch-policy"] {
        let result = invoke(&[
            "analyze",
            source.to_str().unwrap(),
            flag,
            download.to_str().unwrap(),
            "--json",
        ]);
        failure(&result, "analyze", "download-options-require-url", 2);
        assert!(!download.exists());
    }
}

#[test]
fn analyze_measurements_require_json() {
    usage(&invoke(&["analyze", "missing.mp4", "--measure"]));
}

#[cfg(unix)]
#[test]
fn saved_plan_refuses_a_shared_writable_parent_before_reading_media() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = drmr_core::native::private_tempdir().unwrap();
    let shared = root.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let input = root.path().join("not-present.mp4");
    let output = shared.join("result");
    let plan = root.path().join("plan.json");
    let result = invoke(&[
        "plan",
        input.to_str().unwrap(),
        "--to",
        "clear",
        "--format",
        "dash",
        "--output",
        output.to_str().unwrap(),
        "--save",
        plan.to_str().unwrap(),
        "--packager-bin",
        "unused-tool-for-destination-preflight",
        "--json",
    ]);
    failure(&result, "plan", "execution-plan-destination-unsafe", 2);
    assert!(!input.exists() && !output.exists() && !plan.exists());
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 0);
}

#[test]
fn source_analysis_and_saved_planning_keep_invalid_key_configuration_private() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let keys = root.path().join("confidential-sentinel.json");
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(br#"{"schema_version":1,"output":{"key":"confidential-sentinel"}}"#)
        .unwrap();
    let result = invoke(&[
        "analyze",
        "missing.mp4",
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "analyze", "key-file-invalid", 2);
    let plan = root.path().join("plan.json");
    let output = root.path().join("result");
    let result = invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--output",
        output.to_str().unwrap(),
        "--save",
        plan.to_str().unwrap(),
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "plan", "key-file-invalid", 2);
    assert!(!plan.exists());
    assert!(!output.exists());
}

#[test]
fn saving_a_plan_requires_a_result_destination_and_one_output_format() {
    let root = tempfile::tempdir().unwrap();
    let plan = root.path().join("plan.json");
    let output = root.path().join("result");
    usage(&invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--save",
        plan.to_str().unwrap(),
    ]));
    usage(&invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--output",
        output.to_str().unwrap(),
    ]));
    let result = invoke(&[
        "plan",
        "confidential-sentinel.m3u8",
        "--to",
        "cbcs",
        "--format",
        "dash,hls",
        "--output",
        output.to_str().unwrap(),
        "--save",
        plan.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "plan", "plan-requires-single-output", 2);
    assert!(!plan.exists());
    assert!(!output.exists());
}

#[test]
fn legacy_plan_modes_refuse_execution_only_options() {
    usage(&invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--keys-file",
        "keys.json",
    ]));
    usage(&invoke(&[
        "plan",
        "--mpd",
        "missing.mpd",
        "--save",
        "plan.json",
        "--output",
        "result",
    ]));
    usage(&invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--packager-bin",
        "packager",
    ]));
    usage(&invoke(&[
        "plan",
        "missing.mp4",
        "--to",
        "clear",
        "--max-workers",
        "1",
    ]));
}

#[test]
fn convert_plan_refuses_every_media_intent_override() {
    for extra in [
        vec!["input.mp4"],
        vec!["--to", "clear"],
        vec!["--format", "dash"],
        vec!["--output", "result"],
        vec!["--download-dir", "source"],
        vec!["--fetch-policy", "fetch.json"],
    ] {
        let mut args = vec!["convert", "--plan", "plan.json"];
        args.extend(extra);
        usage(&invoke(&args));
    }
    usage(&invoke(&["convert"]));
    usage(&invoke(&["convert", "input.mp4", "--to", "clear"]));
}

#[test]
fn verify_plan_refuses_source_and_result_overrides() {
    usage(&invoke(&["verify", "--plan", "plan.json", "result.mpd"]));
    usage(&invoke(&[
        "verify",
        "--plan",
        "plan.json",
        "--source",
        "source.m3u8",
    ]));
    usage(&invoke(&["verify"]));
    usage(&invoke(&["verify", "result.mpd"]));
}

#[test]
fn legacy_intent_json_is_not_accepted_as_an_execution_plan() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("input.m3u8");
    std::fs::write(&source, "#EXTM3U\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n").unwrap();
    let described = invoke(&["plan", source.to_str().unwrap(), "--to", "clear", "--json"]);
    assert!(described.status.success());
    let plan = root.path().join("confidential-sentinel.json");
    drmr_core::native::create_private_new(&plan)
        .unwrap()
        .write_all(&described.stdout)
        .unwrap();
    for command in ["convert", "verify"] {
        let result = invoke(&[command, "--plan", plan.to_str().unwrap(), "--json"]);
        failure(&result, command, "execution-plan-invalid", 2);
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}
