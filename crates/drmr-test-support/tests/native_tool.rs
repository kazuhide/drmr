use drmr_test_support::{Mode, ToolSpec, install};
#[test]
fn native_response_preserves_exact_bytes_exit_status_and_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let capture = directory.path().join("argv");
    let spec = ToolSpec {
        mode: Mode::Response,
        stdout: b"a\r\nb\0c".to_vec(),
        stderr: b"error\r\n".to_vec(),
        exit_code: 7,
        ..ToolSpec::default()
    }
    .with("argv_file", capture.to_str().unwrap());
    let tool = install(&directory.path().join("native-tool"), &spec).unwrap();
    let output = std::process::Command::new(tool)
        .args(["spaced argument", "unicode-映像", "key=a:b"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"a\r\nb\0c");
    assert_eq!(output.stderr, b"error\r\n");
    assert_eq!(
        std::fs::read_to_string(capture).unwrap(),
        "spaced argument\nunicode-映像\nkey=a:b"
    );
    directory.close().unwrap();
}
#[test]
fn installed_config_is_immutable_and_existing_tools_are_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture");
    let spec = ToolSpec::default();
    let tool = install(&path, &spec).unwrap();
    assert!(install(&path, &spec).is_err());
    let original = std::fs::read(&tool).unwrap();
    assert!(original.len() > 1024);
    assert!(std::process::Command::new(tool).status().unwrap().success());
}

#[test]
fn packager_mode_records_original_arguments_and_creates_exact_declared_outputs() {
    let d = tempfile::tempdir().unwrap();
    let capture = d.path().join("args");
    let init = d.path().join("init.mp4");
    let template = d.path().join("seg_$Number$.m4s");
    let tool = install(
        &d.path().join("packager"),
        &ToolSpec {
            mode: Mode::Packager,
            ..ToolSpec::default()
        }
        .with("STUB_PACKAGER_ARGV_FILE", capture.to_str().unwrap())
        .with("STUB_PACKAGER_STDERR", "echo_argv")
        .with("STUB_PACKAGER_EXIT", "7"),
    )
    .unwrap();
    let descriptor = format!(
        "input=in.mp4,stream=0,init_segment={},segment_template={}",
        init.display(),
        template.display()
    );
    let out = std::process::Command::new(tool)
        .args([&descriptor, "--segment_duration", "1000000"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert!(init.is_file());
    assert!(d.path().join("seg_1.m4s").is_file());
    assert!(!d.path().join("seg_2.m4s").exists());
    assert_eq!(
        std::fs::read_to_string(capture).unwrap(),
        format!("{descriptor}\n--segment_duration\n1000000")
    );
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("shaka packager error; args were: {descriptor} --segment_duration 1000000\n")
    );
}
