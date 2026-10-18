use drmr_test_support::{Mode, ToolSpec, install};
#[test]
fn real_probe_proxy_preserves_nonprofile_bytes_arguments_and_failure_status() {
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("argv");
    let actual = install(
        &root.path().join("actual"),
        &ToolSpec {
            stdout: b"actual\0packet\r\n".to_vec(),
            stderr: b"actual failure\r\n".to_vec(),
            exit_code: 7,
            ..Default::default()
        }
        .with("argv_file", capture.to_str().unwrap()),
    )
    .unwrap();
    let proxy = install(
        &root.path().join("proxy"),
        &ToolSpec {
            mode: Mode::FfprobeProxy,
            ..Default::default()
        }
        .with("real_probe", actual.to_str().unwrap()),
    )
    .unwrap();
    let output = std::process::Command::new(proxy)
        .args(["-show_entries", "packet=pts,dts", "native spaced input"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"actual\0packet\r\n");
    assert_eq!(output.stderr, b"actual failure\r\n");
    assert_eq!(
        std::fs::read_to_string(capture).unwrap(),
        "-show_entries\npacket=pts,dts\nnative spaced input"
    );
}
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

#[test]
fn response_state_changes_status_without_rewriting_executable_or_mode_config() {
    let directory = tempfile::tempdir().unwrap();
    let status = directory.path().join("status");
    ToolSpec::default()
        .encode(std::fs::File::create(&status).unwrap())
        .unwrap();
    let tool = install(
        &directory.path().join("stateful"),
        &ToolSpec::default().with("status_file", status.to_str().unwrap()),
    )
    .unwrap();
    let bytes = std::fs::read(&tool).unwrap();
    assert!(
        std::process::Command::new(&tool)
            .status()
            .unwrap()
            .success()
    );
    ToolSpec {
        exit_code: 7,
        stderr: b"changed\n".to_vec(),
        ..Default::default()
    }
    .encode(std::fs::File::create(status).unwrap())
    .unwrap();
    let output = std::process::Command::new(&tool).output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stderr, b"changed\n");
    assert_eq!(std::fs::read(tool).unwrap(), bytes);
}

#[test]
fn a_response_can_report_a_selected_child_environment_value_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let tool = install(
        &directory.path().join("environment"),
        &ToolSpec::default()
            .with("stdout_env", "PATH")
            .with("stdout_env_prefix", "probed ")
            .with("stdout_env_suffix", "\n"),
    )
    .unwrap();
    let output = std::process::Command::new(tool)
        .env("PATH", "native-path;spaces and 日本語")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        "probed native-path;spaces and 日本語\n".as_bytes()
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn server_cli_records_environment_and_arguments_before_hold_release() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("calls")).unwrap();
    std::fs::write(d.path().join("hold"), b"").unwrap();
    std::fs::write(d.path().join("lines"), b"one\r\ntwo\n").unwrap();
    let tool = install(
        &d.path().join("drmr"),
        &ToolSpec {
            mode: Mode::ServerCli,
            ..Default::default()
        }
        .with("control_dir", d.path().to_str().unwrap()),
    )
    .unwrap();
    let mut child = std::process::Command::new(tool)
        .args(["run", "--asset-id", "映像"])
        .env("DRM_KID", "synthetic-id")
        .env("DRM_KEY", "synthetic-key")
        .env_remove("DRM_IV")
        .env("PATH", "fixture-path")
        .env("AMBIENT_MARKER", "marker")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let ready = d.path().join("calls/0/inherited");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.is_file() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        std::fs::read(d.path().join("calls/0/argv")).unwrap(),
        "run\n--asset-id\n映像\n".as_bytes()
    );
    assert_eq!(
        std::fs::read(d.path().join("calls/0/drm")).unwrap(),
        b"synthetic-id\nsynthetic-key\n\n"
    );
    assert_eq!(std::fs::read(ready).unwrap(), b"fixture-path\nmarker\n");
    assert!(child.try_wait().unwrap().is_none());
    std::fs::write(d.path().join("release"), b"7").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"one\r\ntwo\n");
    assert!(!d.path().join("release").exists());
}

#[test]
fn server_cli_copies_synthetic_conversion_tree_and_rewrites_the_manifest() {
    let d = tempfile::tempdir().unwrap();
    let control = d.path().join("cli");
    std::fs::create_dir_all(control.join("calls")).unwrap();
    for name in ["convert", "hold"] {
        std::fs::write(control.join(name), b"").unwrap();
    }
    std::fs::write(control.join("code"), b"7").unwrap();
    let source = d
        .path()
        .join("download/manifest.example.test/asset/dash/manifest.mpd");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let original = "<MPD><BaseURL>https://cdn.example.test/asset/</BaseURL><ContentProtection value=\"cenc\"/></MPD>\n";
    std::fs::write(&source, original).unwrap();
    let media = d.path().join("download/cdn.example.test/asset/video/v0");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("init.mp4"), b"INIT").unwrap();
    std::fs::write(media.join("seg_0.m4s"), b"SEGMENT").unwrap();
    let tool = install(
        &control.join("drmr"),
        &ToolSpec {
            mode: Mode::ServerCli,
            ..Default::default()
        }
        .with("control_dir", control.to_str().unwrap()),
    )
    .unwrap();
    let output = d.path().join("output with spaces");
    let result = std::process::Command::new(tool)
        .arg("run")
        .arg("--mpd")
        .arg(&source)
        .arg("--output")
        .arg(&output)
        .args([
            "--asset-id",
            "asset",
            "--manifest-local-url-base",
            "/local/asset/",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        result.stdout,
        b"{\"stage\": \"report\", \"level\": \"info\", \"message\": \"converted\"}\n"
    );
    assert_eq!(
        std::fs::read(output.join("asset/cbcs/video/v0/seg_0.m4s")).unwrap(),
        b"SEGMENT"
    );
    assert_eq!(
        std::fs::read(output.join("asset/cbcs/video/v0/init.mp4")).unwrap(),
        b"INIT"
    );
    assert_eq!(
        std::fs::read_to_string(output.join("asset/manifests/local/host/asset/dash/manifest.mpd"))
            .unwrap(),
        "<MPD><BaseURL>/local/asset/</BaseURL><ContentProtection value=\"cbcs\"/></MPD>\n"
    );
    assert_eq!(std::fs::read_to_string(source).unwrap(), original);
}

#[test]
fn native_response_records_its_process_identity_before_completing() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("pid");
    let tool = install(
        &root.path().join("identity"),
        &ToolSpec::default().with("pid_file", pid_file.to_str().unwrap()),
    )
    .unwrap();
    let mut child = std::process::Command::new(tool).spawn().unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        std::fs::read_to_string(pid_file).unwrap(),
        format!("{pid}\n")
    );
}
