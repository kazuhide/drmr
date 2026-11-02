//! Version 1.2 configuration, reporting and direct-verification contracts.
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "support/download_http.rs"]
mod download_http;

fn source(root: &Path) -> (PathBuf, Vec<u8>) {
    let bytes = include_bytes!("fixtures/bbb/clip.mp4").to_vec();
    let path = root.join("source.mp4");
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

fn path(value: &Path) -> &str {
    value.to_str().unwrap()
}

fn cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(cwd)
        .args(args)
        .args(["--progress", "off"])
        .output()
        .unwrap()
}

fn passed(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}: {}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn result(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "result JSON: {error}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn config(file: &Path, output_dir: &str) {
    std::fs::write(
        file,
        serde_json::to_vec(&json!({
            "schema_version": 4,
            "output_dir": output_dir,
            "outputs": [{"id":"clear", "output_format":"mp4", "encryption":"clear", "drms":[]}]
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn single_mp4_statistics_and_private_json_preserve_readable_summary_and_source() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let output = root.join("result.mp4");
    let report = root.join("result.json");
    let converted = cli(
        root,
        &[
            "convert",
            path(&input),
            "--encryption",
            "clear",
            "--output-format",
            "mp4",
            "--output-file",
            path(&output),
            "--stats",
            "--report-file",
            path(&report),
        ],
    );
    passed(&converted);
    let text = String::from_utf8(converted.stdout).unwrap();
    assert_eq!(text.matches("\nSummary\n").count(), 1, "{text}");
    assert_eq!(text.matches("\nStatistics\n").count(), 1, "{text}");
    assert!(!text.starts_with('{'), "readable output changed to JSON");
    let recorded: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(recorded["verification"]["passed"], true);
    assert!(recorded["measurements"].is_object());
    assert_eq!(recorded["measurements"]["schema_version"], 1);
    assert!(output.is_file());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&report).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn configuration_destination_uses_its_directory_and_explicit_override_ignores_unused_parent() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let settings = root.join("config");
    let cwd = root.join("work");
    std::fs::create_dir(&settings).unwrap();
    std::fs::create_dir(&cwd).unwrap();
    let specification = settings.join("outputs.json");
    config(&specification, "relative-result");
    let converted = cli(
        &cwd,
        &[
            "convert",
            path(&input),
            "--output-config",
            path(&specification),
            "--json",
        ],
    );
    passed(&converted);
    assert!(settings.join("relative-result/clear/media.mp4").is_file());
    assert!(!cwd.join("relative-result").exists());
    assert_eq!(
        result(&converted)["outputs"][0]["conversion"]["verification"]["passed"],
        true
    );

    config(&specification, "unused/missing/result");
    let overridden = cli(
        &cwd,
        &[
            "convert",
            path(&input),
            "--output-config",
            path(&specification),
            "--output-dir",
            "override",
            "--json",
        ],
    );
    passed(&overridden);
    assert!(cwd.join("override/clear/media.mp4").is_file());
    assert!(!settings.join("unused").exists());
    assert_eq!(
        result(&overridden)["outputs"][0]["conversion"]["verification"]["passed"],
        true
    );
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn configured_results_verify_without_a_saved_plan_and_reject_changed_media() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let specification = root.join("outputs.json");
    config(&specification, "unused-result");
    let output = root.join("actual-result");
    passed(&cli(
        root,
        &[
            "convert",
            path(&input),
            "--output-config",
            path(&specification),
            "--output-dir",
            path(&output),
            "--json",
        ],
    ));
    let verify = || {
        cli(
            root,
            &[
                "verify",
                path(&output),
                "--source",
                path(&input),
                "--output-config",
                path(&specification),
                "--json",
            ],
        )
    };
    let checked = verify();
    passed(&checked);
    assert_eq!(result(&checked)["passed"], true);
    assert!(!root.join("unused-result").exists());

    let media = output.join("clear/media.mp4");
    let mut bytes = std::fs::read(&media).unwrap();
    let kind = bytes.windows(4).position(|word| word == b"mdat").unwrap();
    assert!(kind >= 4);
    let length = u32::from_be_bytes(bytes[kind - 4..kind].try_into().unwrap()) as usize;
    assert!(length > 8, "fixture uses a finite 32-bit media box");
    let end = kind - 4 + length;
    assert!(end <= bytes.len());
    bytes[end - 1] ^= 1;
    std::fs::write(&media, &bytes).unwrap();
    let rejected = verify();
    assert_eq!(rejected.status.code(), Some(1));
    assert_ne!(result(&rejected)["passed"], true);
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn an_existing_report_is_never_replaced_and_invalid_preflight_publishes_no_media() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let output = root.join("result.mp4");
    let report = root.join("existing.json");
    let existing = b"operator-owned report\n";
    std::fs::write(&report, existing).unwrap();
    let rejected = cli(
        root,
        &[
            "convert",
            path(&input),
            "--encryption",
            "clear",
            "--output-format",
            "mp4",
            "--output-file",
            path(&output),
            "--report-file",
            path(&report),
            "--json",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(
        result(&rejected)["error"]["code"],
        "report-destination-exists"
    );
    assert_eq!(std::fs::read(&report).unwrap(), existing);
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn a_report_inside_the_verified_result_is_refused_without_creating_an_error_report() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let specification = root.join("outputs.json");
    config(&specification, "result");
    passed(&cli(
        root,
        &[
            "convert",
            path(&input),
            "--output-config",
            path(&specification),
            "--json",
        ],
    ));
    let output = root.join("result");
    let report = output.join("error.json");
    let media = output.join("clear/media.mp4");
    let before = std::fs::read(&media).unwrap();
    let rejected = cli(
        root,
        &[
            "verify",
            path(&output),
            "--source",
            path(&input),
            "--output-config",
            path(&specification),
            "--report-file",
            path(&report),
            "--json",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(
        result(&rejected)["error"]["code"],
        "report-destination-overlap"
    );
    assert!(
        !report.exists(),
        "the error itself must not write into the result tree"
    );
    assert_eq!(std::fs::read(&media).unwrap(), before);
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn saved_url_plan_retains_its_original_and_executes_and_verifies_offline() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let original = include_bytes!("fixtures/bbb/clip.mp4");
    let server = download_http::Server::start([("/source.mp4".into(), original.to_vec())].into());
    let policy = root.join("fetch.json");
    let settings = json!({
        "allowed_origins": [server.origin], "allow_http": true, "allow_private_network": true,
        "headers_by_origin": { &server.origin: { "Authorization": download_http::AUTH } }
    });
    drmr_core::native::create_private_new(&policy)
        .unwrap()
        .write_all(settings.to_string().as_bytes())
        .unwrap();
    let url = format!(
        "{}/source.mp4?token={}",
        server.origin,
        download_http::QUERY
    );
    let download = root.join("originals");
    let output = root.join("result.mp4");
    let saved = root.join("conversion.plan.json");
    let planned = cli(
        root,
        &[
            "plan",
            &url,
            "--download-dir",
            path(&download),
            "--fetch-config",
            path(&policy),
            "--encryption",
            "clear",
            "--output-format",
            "mp4",
            "--output-file",
            path(&output),
            "--save",
            path(&saved),
            "--json",
        ],
    );
    passed(&planned);
    assert_eq!(result(&planned)["schema_version"], 9);
    assert_eq!(std::fs::read(download.join("input.mp4")).unwrap(), original);
    let plan_bytes = std::fs::read(&saved).unwrap();
    let counts = server.stop();
    assert!(counts.served > 0);
    assert_eq!(counts.refused, 0);
    for secret in [download_http::AUTH, download_http::QUERY, &url] {
        assert!(!String::from_utf8_lossy(&plan_bytes).contains(secret));
        for stream in [&planned.stdout, &planned.stderr] {
            assert!(!String::from_utf8_lossy(stream).contains(secret));
        }
    }
    passed(&cli(
        root,
        &["convert", "--plan-file", path(&saved), "--json"],
    ));
    let checked = cli(root, &["verify", "--plan-file", path(&saved), "--json"]);
    passed(&checked);
    let verified = result(&checked);
    assert_eq!(verified["source_unchanged"], true);
    assert_eq!(verified["output_matches_plan"], true);
    assert_eq!(verified["verification"]["passed"], true);
    assert_eq!(std::fs::read(download.join("input.mp4")).unwrap(), original);
    assert_eq!(std::fs::read(&saved).unwrap(), plan_bytes);
}

#[test]
fn a_saved_plan_result_rejects_an_inside_report_without_writing_or_mutating_files() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let specification = root.join("outputs.json");
    config(&specification, "result");
    let saved = root.join("conversion.plan.json");
    let planned = cli(
        root,
        &[
            "plan",
            path(&input),
            "--output-config",
            path(&specification),
            "--save",
            path(&saved),
            "--json",
        ],
    );
    passed(&planned);
    assert_eq!(result(&planned)["schema_version"], 10);
    let plan_bytes = std::fs::read(&saved).unwrap();
    let converted = cli(root, &["convert", "--plan-file", path(&saved), "--json"]);
    passed(&converted);
    assert_eq!(
        result(&converted)["outputs"][0]["conversion"]["verification"]["passed"],
        true
    );
    let output = root.join("result");
    let report = output.join("inside.json");
    let media = output.join("clear/media.mp4");
    let media_bytes = std::fs::read(&media).unwrap();
    let rejected = cli(
        root,
        &[
            "verify",
            "--plan-file",
            path(&saved),
            "--report-file",
            path(&report),
            "--json",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(
        result(&rejected)["error"]["code"],
        "report-destination-overlap"
    );
    assert!(
        !report.exists(),
        "the error itself must not write into the saved result tree"
    );
    assert_eq!(std::fs::read(&media).unwrap(), media_bytes);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(std::fs::read(&saved).unwrap(), plan_bytes);
}

fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let file = entry.unwrap().path();
            assert!(!file.is_symlink());
            if file.is_dir() {
                pending.push(file);
            } else {
                files.insert(
                    file.strip_prefix(root).unwrap().to_owned(),
                    std::fs::read(file).unwrap(),
                );
            }
        }
    }
    files
}

fn invalid_key_files(root: &Path) -> [PathBuf; 2] {
    let missing = root.join("missing-keys.json");
    let invalid = root.join("invalid-keys.json");
    drmr_core::native::create_private_new(&invalid).unwrap().write_all(
        br#"{"schema_version":2,"outputs":{"default":{"kid":"000102030405060708090a0b0c0d0e0f","key":"invalid"}}}"#,
    ).unwrap();
    [missing, invalid]
}

fn assert_overlap(output: &Output, report: &Path) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        result(output)["error"]["code"],
        "report-destination-overlap"
    );
    assert!(
        !report.exists(),
        "an unsafe report destination must receive no error report"
    );
}

fn fragmented_hls(root: &Path) -> PathBuf {
    std::fs::create_dir(root).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let manifest = root.join("input.m3u8");
    let generated = Command::new("ffmpeg")
        .current_dir(root)
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_segment_type",
            "fmp4",
            "-hls_playlist_type",
            "vod",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "public stream-copy fixture failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    manifest
}

fn executed_hls_plan(root: &Path, input: &Path, named: bool) -> (PathBuf, PathBuf) {
    let label = if named { "named" } else { "single" };
    let saved = root.join(format!("{label}.plan.json"));
    let output = root.join(format!("{label}-result"));
    let specification = root.join(format!("{label}.outputs.json"));
    let mut args = vec![
        "plan",
        path(input),
        "--processing",
        "segments",
        "--save",
        path(&saved),
        "--json",
    ];
    if named {
        std::fs::write(&specification, serde_json::to_vec(&json!({
            "schema_version":4,"output_dir":output,
            "outputs":[{"id":"clear","output_format":"hls-fmp4","encryption":"clear","drms":[],"processing":"segments"}]
        })).unwrap()).unwrap();
        args.extend(["--output-config", path(&specification)]);
    } else {
        args.extend([
            "--encryption",
            "clear",
            "--output-format",
            "hls-fmp4",
            "--output-dir",
            path(&output),
        ]);
    }
    let planned = cli(root, &args);
    passed(&planned);
    assert_eq!(
        result(&planned)["schema_version"],
        if named { 10 } else { 9 }
    );
    passed(&cli(
        root,
        &["convert", "--plan-file", path(&saved), "--json"],
    ));
    assert!(output.is_dir());
    assert!(!snapshot(&output).is_empty());
    (saved, output)
}

#[test]
fn saved_operations_protect_hidden_source_and_result_roots_before_loading_keys() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let input = fragmented_hls(&root.join("source"));
    let source_root = input.parent().unwrap();
    let original = snapshot(source_root);
    let keys = invalid_key_files(root);
    for named in [false, true] {
        let (saved, output) = executed_hls_plan(root, &input, named);
        let plan_bytes = std::fs::read(&saved).unwrap();
        let media = snapshot(&output);
        for operation in ["convert", "verify"] {
            for (index, key) in keys.iter().enumerate() {
                for directory in [source_root, output.as_path()] {
                    let report = directory.join(format!("{operation}-{index}-{named}.json"));
                    let rejected = cli(
                        root,
                        &[
                            operation,
                            "--plan-file",
                            path(&saved),
                            "--keys-file",
                            path(key),
                            "--report-file",
                            path(&report),
                            "--json",
                        ],
                    );
                    assert_overlap(&rejected, &report);
                    assert_eq!(snapshot(source_root), original);
                    assert_eq!(snapshot(&output), media);
                    assert_eq!(std::fs::read(&saved).unwrap(), plan_bytes);
                }
            }
        }
    }
}

#[test]
fn configured_output_roots_are_protected_before_invalid_keys_can_record_a_failure() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let specification = root.join("outputs.json");
    let selected = root.join("selected");
    std::fs::create_dir(&selected).unwrap();
    std::fs::write(selected.join("operator.txt"), b"existing operator bytes").unwrap();
    let before = snapshot(&selected);
    let keys = invalid_key_files(root);
    for explicit in [false, true] {
        config(
            &specification,
            if explicit {
                "unused/missing/result"
            } else {
                "selected"
            },
        );
        let config_bytes = std::fs::read(&specification).unwrap();
        for (index, key) in keys.iter().enumerate() {
            let report = selected.join(format!("error-{index}-{explicit}.json"));
            let mut args = vec![
                "convert",
                path(&input),
                "--output-config",
                path(&specification),
                "--keys-file",
                path(key),
                "--report-file",
                path(&report),
                "--json",
            ];
            if explicit {
                args.extend(["--output-dir", path(&selected)]);
            }
            assert_overlap(&cli(root, &args), &report);
            assert_eq!(snapshot(&selected), before);
            assert_eq!(std::fs::read(&input).unwrap(), original);
            assert_eq!(std::fs::read(&specification).unwrap(), config_bytes);
            assert!(!root.join("unused").exists());
        }
    }
}

fn declaration(format: &str) -> (&'static str, &'static str) {
    match format {
        "hls" => (
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n",
            "missing.ts",
        ),
        "dash" => (
            "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT2S\"><Period><AdaptationSet contentType=\"video\"><Representation id=\"video\" bandwidth=\"1000\" codecs=\"avc1.64001f\"><SegmentList timescale=\"1\" duration=\"2\"><Initialization sourceURL=\"missing-init.mp4\"/><SegmentURL media=\"missing.m4s\"/></SegmentList></Representation></AdaptationSet></Period></MPD>",
            "missing.m4s",
        ),
        _ => panic!("unknown fixture format"),
    }
}

#[test]
fn manifest_bytes_protect_extensionless_and_text_inputs_and_missing_resources() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    for format in ["hls", "dash"] {
        for name in ["manifest", "manifest.txt"] {
            let directory = root.join(format!("{format}-{name}"));
            std::fs::create_dir(&directory).unwrap();
            let input = directory.join(name);
            let (text, missing) = declaration(format);
            std::fs::write(&input, text).unwrap();
            let before = snapshot(&directory);
            let metadata = cli(
                root,
                &["analyze", "--metadata-only", path(&input), "--json"],
            );
            passed(&metadata);
            assert_eq!(result(&metadata)["format"], format);
            for report in [directory.join("sibling.json"), directory.join(missing)] {
                let rejected = cli(
                    root,
                    &[
                        "analyze",
                        "--metadata-only",
                        path(&input),
                        "--report-file",
                        path(&report),
                        "--json",
                    ],
                );
                assert_overlap(&rejected, &report);
                assert_eq!(snapshot(&directory), before);
            }
            let missing_report = directory.join(missing);
            let rejected = cli(
                root,
                &[
                    "analyze",
                    path(&input),
                    "--report-file",
                    path(&missing_report),
                    "--json",
                ],
            );
            assert_overlap(&rejected, &missing_report);
            assert_eq!(snapshot(&directory), before);
        }
    }
}

#[test]
fn extensionless_mp4_metadata_can_save_a_sibling_report_without_mutating_the_input() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let input = root.join("media");
    let original = include_bytes!("fixtures/bbb/clip.mp4");
    std::fs::write(&input, original).unwrap();
    let report = root.join("metadata.json");
    let metadata = cli(
        root,
        &[
            "analyze",
            "--metadata-only",
            path(&input),
            "--report-file",
            path(&report),
            "--json",
        ],
    );
    passed(&metadata);
    let value = result(&metadata);
    assert_eq!(value["format"], "mp4");
    assert_eq!(value["media_verified"], false);
    let recorded: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(recorded, value);
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn direct_manifest_result_reports_stay_outside_the_result_tree_before_key_loading() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let output = root.join("result");
    std::fs::create_dir(&output).unwrap();
    let missing_keys = root.join("missing-keys.json");
    for (name, bytes) in [
        (
            "master.m3u8",
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n",
        ),
        ("manifest.mpd", "<MPD/>"),
        (
            "manifest",
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n",
        ),
    ] {
        let manifest = output.join(name);
        std::fs::write(&manifest, bytes).unwrap();
        for manifest_only in [false, true] {
            let report = output.join("missing.ts");
            let mut args = vec![
                "verify",
                path(&manifest),
                "--json",
                "--report-file",
                path(&report),
            ];
            if manifest_only {
                args.push("--manifest-only");
            } else {
                args.extend(["--source", path(&input), "--keys-file", path(&missing_keys)]);
            }
            let refused = cli(root, &args);
            assert_eq!(refused.status.code(), Some(2));
            assert_eq!(
                result(&refused)["error"]["code"],
                "report-destination-overlap"
            );
            assert!(!report.exists());
            assert_eq!(std::fs::read(&manifest).unwrap(), bytes.as_bytes());
        }
    }
    let report = root.join("safe-key-error.json");
    let failed = cli(
        root,
        &[
            "verify",
            path(&output.join("manifest")),
            "--source",
            path(&input),
            "--keys-file",
            path(&missing_keys),
            "--report-file",
            path(&report),
            "--json",
        ],
    );
    assert!(!failed.status.success());
    assert_eq!(result(&failed)["error"]["code"], "key-file-invalid");
    let recorded: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(recorded, result(&failed));
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn bare_relative_manifest_paths_allow_reports_outside_their_media_directory() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let (input, original) = source(root);
    let media = root.join("media");
    std::fs::create_dir(&media).unwrap();
    let text = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n";
    std::fs::write(media.join("master.m3u8"), text).unwrap();
    for (index, manifest) in ["master.m3u8", "./master.m3u8"].iter().enumerate() {
        for command in ["analyze", "verify"] {
            let name = root.join(format!("{command}-{index}.json"));
            let mode = if command == "analyze" {
                "--metadata-only"
            } else {
                "--manifest-only"
            };
            let output = cli(
                &media,
                &[
                    command,
                    manifest,
                    mode,
                    "--report-file",
                    path(&name),
                    "--json",
                ],
            );
            passed(&output);
            let recorded: Value = serde_json::from_slice(&std::fs::read(&name).unwrap()).unwrap();
            assert_eq!(recorded, result(&output));
        }
        let error_report = root.join(format!("key-error-{index}.json"));
        let missing_keys = root.join("missing-keys.json");
        let output = cli(
            &media,
            &[
                "verify",
                manifest,
                "--source",
                path(&input),
                "--keys-file",
                path(&missing_keys),
                "--report-file",
                path(&error_report),
                "--json",
            ],
        );
        assert_eq!(result(&output)["error"]["code"], "key-file-invalid");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(error_report).unwrap()).unwrap(),
            result(&output)
        );
        let refused = cli(
            &media,
            &[
                "verify",
                manifest,
                "--manifest-only",
                "--report-file",
                "missing.ts",
                "--json",
            ],
        );
        assert_eq!(
            result(&refused)["error"]["code"],
            "report-destination-overlap"
        );
        assert!(!media.join("missing.ts").exists());
    }
    let traversal = cli(
        &media,
        &[
            "verify",
            "master.m3u8",
            "--manifest-only",
            "--report-file",
            "../refused.json",
            "--json",
        ],
    );
    assert_eq!(
        result(&traversal)["error"]["code"],
        "report-destination-unsafe"
    );
    assert!(!root.join("refused.json").exists());
    assert_eq!(
        std::fs::read_to_string(media.join("master.m3u8")).unwrap(),
        text
    );
    assert_eq!(std::fs::read(&input).unwrap(), original);
}
