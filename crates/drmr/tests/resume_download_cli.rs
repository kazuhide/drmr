//! Resume is opt-in and rejected before media tools or remote acquisition at invalid boundaries.

use serde_json::Value;
use std::ffi::OsString;
use std::io::Write as _;
use std::process::{Command, Output};

fn invoke(arguments: &[OsString]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(arguments)
        .output()
        .unwrap()
}

fn private(result: &Output) {
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("private-source-sentinel"),
            "source identity was disclosed"
        );
        assert!(
            !text.contains("private-key-sentinel"),
            "source query was disclosed"
        );
    }
}

fn failed(result: &Output, expected: &str) {
    assert_eq!(result.status.code(), Some(2));
    private(result);
    let report: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&result.stderr)));
    assert_eq!(report["error"]["code"], expected);
}

#[test]
fn resume_help_explains_opt_in_attempts_and_the_new_download_directory() {
    for command in ["analyze", "plan", "convert"] {
        let result = invoke(&[command.into(), "--help".into()]);
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(help.contains("--resume-download"));
        assert!(help.contains("--download-dir"));
        assert!(help.contains("first opt-in attempt"));
        assert!(help.contains("new directory"));
    }
}

#[test]
fn local_input_refuses_resume_without_creating_downloads_or_results() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("private-source-sentinel.mp4");
    let download = root.path().join("source");
    let output = root.path().join("new.mp4");
    let plan = root.path().join("new.plan.json");
    for command in ["analyze", "plan", "convert"] {
        let mut arguments = vec![command.into(), source.as_os_str().to_owned()];
        if command != "analyze" {
            arguments.extend([
                "--encryption".into(),
                "clear".into(),
                "--output-file".into(),
                output.as_os_str().to_owned(),
            ]);
        }
        if command == "plan" {
            arguments.extend(["--save".into(), plan.as_os_str().to_owned()]);
        }
        arguments.extend([
            "--download-dir".into(),
            download.as_os_str().to_owned(),
            "--resume-download".into(),
            "--json".into(),
        ]);
        failed(&invoke(&arguments), "download-options-require-url");
        assert!(!download.exists() && !output.exists() && !plan.exists());
    }
}

#[test]
fn named_output_plan_and_convert_refuse_resume_for_local_input() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let recipe = root.path().join("outputs.json");
    drmr_core::native::create_private_new(&recipe)
        .unwrap()
        .write_all(br#"{"schema_version":4,"outputs":[{"id":"dash","output_format":"dash","encryption":"clear","drms":[]},{"id":"hls","output_format":"hls-fmp4","encryption":"clear","drms":[]}]}"#)
        .unwrap();
    let source = root.path().join("private-source-sentinel.mp4");
    let download = root.path().join("source");
    let output = root.path().join("results");
    let plan = root.path().join("new.plan.json");
    for command in ["plan", "convert"] {
        let mut arguments = vec![
            command.into(),
            source.as_os_str().to_owned(),
            "--output-config".into(),
            recipe.as_os_str().to_owned(),
            "--output-dir".into(),
            output.as_os_str().to_owned(),
            "--download-dir".into(),
            download.as_os_str().to_owned(),
            "--resume-download".into(),
            "--json".into(),
        ];
        if command == "plan" {
            arguments.extend(["--save".into(), plan.as_os_str().to_owned()]);
        }
        failed(&invoke(&arguments), "download-options-require-url");
        assert!(!download.exists() && !output.exists() && !plan.exists());
    }
}

#[test]
fn a_completed_download_directory_stays_immutable_with_resume_enabled() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    drmr_core::native::create_private_dir_all(&download).unwrap();
    let sentinel = download.join("retained");
    std::fs::write(&sentinel, b"complete original").unwrap();
    let url = "https://example.invalid/private-source-sentinel.mp4?key=private-key-sentinel";
    for command in ["analyze", "plan", "convert"] {
        let output = root.path().join("new.mp4");
        let plan = root.path().join("new.plan.json");
        let mut arguments = vec![command.into(), url.into()];
        if command != "analyze" {
            arguments.extend([
                "--encryption".into(),
                "clear".into(),
                "--output-file".into(),
                output.as_os_str().to_owned(),
            ]);
        }
        if command == "plan" {
            arguments.extend(["--save".into(), plan.as_os_str().to_owned()]);
        }
        arguments.extend([
            "--download-dir".into(),
            download.as_os_str().to_owned(),
            "--resume-download".into(),
            "--json".into(),
        ]);
        failed(&invoke(&arguments), "download-directory-exists");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"complete original");
        assert!(!output.exists() && !plan.exists());
    }
}

#[test]
fn reports_cannot_pollute_an_existing_resume_cache() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let cache = root.path().join(".source.drmr-download");
    drmr_core::native::create_private_dir_new(&cache).unwrap();
    let report = cache.join("report.json");
    let result = invoke(&[
        "analyze".into(),
        "https://example.invalid/private-source-sentinel.m3u8?key=private-key-sentinel".into(),
        "--download-dir".into(),
        root.path().join("source").into_os_string(),
        "--resume-download".into(),
        "--report-file".into(),
        report.into_os_string(),
        "--json".into(),
    ]);
    failed(&result, "report-destination-overlap");
    assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
    assert!(!root.path().join("source").exists());
}

#[test]
fn missing_resume_directory_and_incompatible_modes_keep_url_values_private() {
    let url = "https://example.invalid/private-source-sentinel.mp4?key=private-key-sentinel";
    for arguments in [
        vec!["analyze", url, "--resume-download"],
        vec![
            "plan",
            url,
            "--encryption",
            "clear",
            "--save",
            "new.plan.json",
            "--output-file",
            "new.mp4",
            "--resume-download",
        ],
        vec![
            "convert",
            url,
            "--encryption",
            "clear",
            "--output-file",
            "new.mp4",
            "--resume-download",
        ],
        vec!["analyze", url, "--metadata-only", "--resume-download"],
        vec![
            "convert",
            "--plan-file",
            "missing.plan.json",
            "--resume-download",
        ],
    ] {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let result = invoke(&arguments);
        assert_eq!(result.status.code(), Some(2));
        private(&result);
    }
}
