//! HLS command help, configuration validation, and snapshot failures without media tools.
//! These tests use synthetic key material only.

use drmr_core::canonical::Dumps;
use drmr_core::snapshot::{analysis_id, snapshot_digest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";

/// Runs drmr with exactly `env` as its `DRM_*` environment.
fn drmr(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command.args(args);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("DRM_") {
            command.env_remove(name);
        }
    }
    command.envs(env.iter().copied()).output().unwrap()
}

fn keys() -> [(&'static str, &'static str); 3] {
    [("DRM_KID", KID), ("DRM_KEY", KEY), ("DRM_IV", IV)]
}

/// Every stdout line as a record; the tool records (none on these paths) left out.
fn records(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .filter(|record: &Value| record["stage"] != "tool")
        .collect()
}

/// The run ends with exactly this one record and this exit code, and prints nothing else.
fn assert_ends(output: &Output, stage: &str, message: &str, code: i32) {
    assert_eq!(
        records(output),
        [json!({"stage": stage, "level": "error", "message": message})],
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(output.status.code(), Some(code));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A private configuration file, protected before its synthetic payload is written.
fn private(path: &Path, value: &Value) -> PathBuf {
    #[cfg(unix)]
    {
        std::fs::write(path, value.to_string()).unwrap();
        set_mode(path, 0o600);
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        let mut file = std::fs::File::create(path).unwrap();
        private_fs::harden(&file).unwrap();
        file.write_all(value.to_string().as_bytes()).unwrap();
    }
    path.to_path_buf()
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    {
        // The deliberately ordinary file keeps its inherited, non-secret ACL.
        assert_eq!(mode, 0o644);
        assert!(private_fs::verify(&std::fs::File::open(path).unwrap()).is_err());
    }
}

/// An admitted snapshot without tracks: `read_analysis` accepts it (its digest is the
/// record's), `package_hls` then has no audio or video to package.
fn empty_snapshot(root: &Path) -> PathBuf {
    let directory = root.join("synth/clear/source");
    std::fs::create_dir_all(&directory).unwrap();
    let mut record =
        json!({"schema_version": "1", "asset_id": "synth", "resources": [], "tracks": []});
    let digest = snapshot_digest(&record);
    record["analysis_id"] = Value::from(analysis_id(&digest));
    record["snapshot_digest"] = Value::from(digest);
    let path = directory.join("analysis.json");
    std::fs::write(&path, Dumps::SORTED.dumps(&record)).unwrap();
    path
}

fn drm_config() -> Value {
    json!({
        "widevine_pssh": format!("1210{KID}"),
        "playready_la_url": "https://license.example.invalid/rightsmanager.asmx",
        "fairplay_uri": "skd://sample-content-id",
    })
}

#[test]
fn help_lists_both_commands_and_packaging_options() {
    let output = drmr(&["--help"], &[]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("analyze-hls      Save and inspect an unencrypted, on-demand HLS stream"),
        "{text}"
    );
    assert!(
        text.contains("package-hls      package clear HLS for Widevine, PlayReady and FairPlay"),
        "{text}"
    );
    let output = drmr(&["package-hls", "--help"], &[]);
    let text = String::from_utf8_lossy(&output.stdout).replace(char::is_whitespace, "");
    assert!(
        text.contains("segmentspackagedatonceacrossalltracks,1-64(default:theCPUstheprocessmayrunon,atmost32;1=oneatatimeonthecaller'sthread)"),
        "{text}"
    );
    // no key flag exists: the key material is the environment's
    assert!(!text.contains("--key") && !text.contains("--iv"), "{text}");
}

#[test]
fn analyze_hls_reports_configuration_errors_as_source_config_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    let run = |config: &Path| {
        drmr(
            &[
                "analyze-hls",
                "--source-config",
                config.to_str().unwrap(),
                "--asset-id",
                "synth",
                "--output",
                out.to_str().unwrap(),
            ],
            &[],
        )
    };
    let missing = temp.path().join("missing.json");
    assert_ends(&run(&missing), "analysis", "source-config-invalid", 2);
    let open = private(&temp.path().join("open.json"), &json!({"url": "x"}));
    set_mode(&open, 0o644);
    assert_ends(&run(&open), "analysis", "source-config-invalid", 2);
    for (name, config) in [
        ("url-not-text", json!({"url": 5})),
        (
            "unknown-policy-key",
            json!({"url": "x", "policy": {"nope": 1}}),
        ),
        (
            "connections-65",
            json!({"url": "x", "policy": {"connections": 65}}),
        ),
        ("null-policy", json!({"url": "x", "policy": null})),
        ("not-an-object", json!([1, 2])),
    ] {
        let path = private(&temp.path().join(format!("{name}.json")), &config);
        assert_ends(&run(&path), "analysis", "source-config-invalid", 2);
    }
    // An empty policy uses defaults, which allow no local root: analysis starts and
    // refuses the source.
    let empty = private(
        &temp.path().join("empty-policy.json"),
        &json!({"url": temp.path().join("master.m3u8").display().to_string(), "policy": ""}),
    );
    let output = run(&empty);
    assert_eq!(
        records(&output),
        [
            json!({"stage": "hls-source", "level": "info", "message": "analysis-started"}),
            json!({"stage": "analysis", "level": "error", "message": "source-not-allowed"}),
        ]
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn analyze_hls_reports_source_errors_with_their_token() {
    let temp = tempfile::tempdir().unwrap();
    let out = temp.path().join("out");
    let config = private(
        &temp.path().join("source.json"),
        &json!({"url": temp.path().join("master.m3u8").display().to_string(),
                "policy": {"allowed_local_roots": [temp.path().display().to_string()]}}),
    );
    let run = |asset: &str| {
        drmr(
            &[
                "analyze-hls",
                "--source-config",
                config.to_str().unwrap(),
                "--asset-id",
                asset,
                "--output",
                out.to_str().unwrap(),
            ],
            &[],
        )
    };
    assert_ends(&run("../x"), "analysis", "invalid-asset-id", 1);
    std::fs::create_dir_all(out.join("synth/clear/source")).unwrap();
    assert_ends(&run("synth"), "analysis", "source-snapshot-exists", 1);
}

#[test]
fn package_hls_reports_snapshot_errors_with_their_token() {
    let temp = tempfile::tempdir().unwrap();
    let drm = private(&temp.path().join("drm.json"), &drm_config());
    let run = |analysis: &Path| {
        drmr(
            &[
                "package-hls",
                "--analysis",
                analysis.to_str().unwrap(),
                "--drm-config",
                drm.to_str().unwrap(),
            ],
            &keys(),
        )
    };
    let missing = temp.path().join("missing/analysis.json");
    assert_ends(&run(&missing), "package-hls", "snapshot-invalid", 1);
    let analysis = empty_snapshot(&temp.path().join("out"));
    let text = std::fs::read_to_string(&analysis).unwrap();
    std::fs::write(
        &analysis,
        text.replace("\"asset_id\": \"synth\"", "\"asset_id\": \"other\""),
    )
    .unwrap();
    assert_ends(&run(&analysis), "package-hls", "snapshot-changed", 1);
}

#[test]
fn package_hls_reports_configuration_errors_as_package_config_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let analysis = empty_snapshot(&temp.path().join("out"));
    let drm = private(&temp.path().join("drm.json"), &drm_config());
    let run = |extra: &[&str], drm: &Path, env: &[(&str, &str)]| {
        let mut args = vec![
            "package-hls",
            "--analysis",
            analysis.to_str().unwrap(),
            "--drm-config",
            drm.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        drmr(&args, env)
    };
    let config_invalid = |output: &Output| {
        assert_ends(output, "package-hls", "package-config-invalid", 2);
        // the key and the IV never reach a record, nor stderr
        for secret in [KEY, IV] {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
        }
    };
    config_invalid(&run(&[], &temp.path().join("missing.json"), &keys()));
    config_invalid(&run(&[], &drm, &[("DRM_KID", KID)]));
    // the key material is the environment's, never the configuration's
    let in_config = private(
        &temp.path().join("in-config.json"),
        &json!({"kid": KID, "key": KEY, "iv": IV, "widevine_pssh": format!("1210{KID}"),
                "playready_la_url": "https://license.example.invalid/x", "fairplay_uri": "skd://x"}),
    );
    config_invalid(&run(&[], &in_config, &[]));
    for workers in ["0", "-1", "65"] {
        config_invalid(&run(&["--max-workers", workers], &drm, &keys()));
    }
    // valid parameters, an admitted snapshot without audio or video: `source-no-av-tracks`
    config_invalid(&run(&[], &drm, &keys()));
    // a selection naming another analysis, or an audio choice that is not an id
    for selection in [
        json!({"analysis_id": 5}),
        json!({"audio_source_id": ["a000"]}),
        json!([1]),
    ] {
        let path = private(&temp.path().join("selection.json"), &selection);
        config_invalid(&run(
            &["--selection", path.to_str().unwrap()],
            &drm,
            &keys(),
        ));
    }
}
