//! Catalog command parsing, exit codes, readable summaries, and scrubbed JSON Lines.
//! The pipeline dependencies are fakes and all keys are synthetic.

#[path = "common/mod.rs"]
mod private_fixture;

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

mod pipeline_fakes;

use base64::Engine as _;
use drmr_core::cli::{self, EXIT_ALL_PASS, EXIT_RENDITION_FAIL, EXIT_USAGE_ERROR, Env, Modules};
use drmr_core::pipeline_deps::PyError;
use pipeline_fakes::*;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One CLI process: the fakes, a scratch directory, and `main(argv)`.
struct Cli {
    dir: private_fixture::PrivateTempDir,
    config: FakeConfig,
    playready: FakePlayReady,
    fakes: Fakes,
}

struct Ran {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Cli {
    fn new() -> Cli {
        Cli {
            dir: private_fixture::private_tempdir().unwrap(),
            config: FakeConfig::default(),
            playready: FakePlayReady::default(),
            fakes: Fakes::new(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn path(&self, relative: &str) -> String {
        self.root().join(relative).display().to_string()
    }

    fn modules(&self) -> Modules<'_> {
        Modules {
            config: &self.config,
            packager: &self.fakes.packager,
            media_inspect: &self.fakes.media,
            manifests: &self.fakes.manifests,
            playready: &self.playready,
        }
    }

    /// `build_synthetic_dash_asset`, registered with the fake config.
    fn asset(&self, reps: &[RepSpec], title: &str) -> SyntheticAsset {
        let asset = build_synthetic_dash_asset(self.root(), reps, title);
        self.config.register(&asset);
        asset
    }

    /// The `asset_mpd` fixture: one H.264 rendition, three segments, `cli-asset`.
    fn asset_mpd(&self) -> String {
        self.asset(&[h264_rep("v0").segments(3)], "cli-asset")
            .mpd_path
            .display()
            .to_string()
    }

    fn main_with(&self, argv: &[&str], env: &Env, stdout: impl Write + Send + 'static) -> u8 {
        let argv: Vec<String> = argv.iter().map(ToString::to_string).collect();
        let mut stderr = SharedBuf::default();
        let code = cli::main(&argv, env, &self.modules(), stdout, &mut stderr);
        eprint!("{}", stderr.text());
        code
    }

    fn main(&self, argv: &[&str], env: &Env) -> Ran {
        let argv: Vec<String> = argv.iter().map(ToString::to_string).collect();
        let stdout = SharedBuf::default();
        let mut stderr = SharedBuf::default();
        let code = cli::main(&argv, env, &self.modules(), stdout.clone(), &mut stderr);
        Ran {
            code,
            stdout: stdout.text(),
            stderr: stderr.text(),
        }
    }

    fn report(&self, asset_id: &str) -> Value {
        let path = self
            .root()
            .join("out")
            .join(asset_id)
            .join("reports/phase1-summary.json");
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
}

/// The `real_key_env` fixture (plus an empty `PATH`: nothing resolves there).
fn real_key_env() -> Env {
    Env::from([
        ("DRM_KID".to_string(), KID.to_string()),
        ("DRM_KEY".to_string(), KEY.to_string()),
        ("DRM_IV".to_string(), IV.to_string()),
        ("PATH".to_string(), String::new()),
    ])
}

/// Every form of the key and the IV a tool or a record could carry them in.
fn secret_forms() -> Vec<String> {
    let bracketed = |hex: &str| {
        let pairs: Vec<&str> = (0..hex.len()).step_by(2).map(|i| &hex[i..i + 2]).collect();
        format!("[{}]", pairs.join(" "))
    };
    let base64 =
        |hex: &str| base64::engine::general_purpose::STANDARD.encode(hex::decode(hex).unwrap());
    vec![
        KEY.into(),
        KEY.to_uppercase(),
        IV.into(),
        IV.to_uppercase(),
        bracketed(IV),
        bracketed(KEY),
        base64(KEY),
        base64(IV),
    ]
}

fn assert_no_secrets(text: &str) {
    for form in secret_forms() {
        assert!(!text.contains(&form), "{form} leaked: {text}");
    }
}

// ---- plan: read-only, no key material needed ----

#[test]
fn plan_enumerates_and_prints_without_key_material() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(&["plan", "--mpd", &mpd], &Env::new());

    assert_eq!(ran.code, EXIT_ALL_PASS, "{}", ran.stderr);
    assert_eq!(
        ran.stdout,
        "asset_id: cli-asset\nrenditions: 1\n  v0  video  avc1.42C00C           3 segments\n"
    );
    // The placeholder key (all zeros) and the `.` output root reach load_asset.
    let loads = cli.config.loads.lock().unwrap().clone();
    assert_eq!(loads.len(), 1);
    assert_eq!(loads[0].1, "0".repeat(32));
    assert_eq!(loads[0].2, PathBuf::from("."));
    assert_eq!(loads[0].3, None);
}

#[test]
fn plan_honours_the_asset_id_override() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(&["plan", "--mpd", &mpd, "--asset-id", "other"], &Env::new());

    assert_eq!(ran.code, EXIT_ALL_PASS);
    assert!(
        ran.stdout.starts_with("asset_id: other\n"),
        "{}",
        ran.stdout
    );
}

#[test]
fn plan_writes_nothing() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();
    let before = tree_state(cli.root());

    cli.main(&["plan", "--mpd", &mpd], &Env::new());

    assert_eq!(tree_state(cli.root()), before);
}

#[test]
fn source_metadata_and_mixed_codecs_fail_before_packaging_or_output() {
    use drmr_core::pipeline_deps::Ports;
    use drmr_core::playready::{build_playready_v43, build_pro};

    for case in ["custom", "ds-id", "mixed-av1", "mixed-hev1"] {
        let cli = Cli::new();
        let asset = cli.asset(&[h264_rep("v0")], "unsupported-source");
        let master = write_minimal_hls_master(cli.root(), "unsupported-source", 3);
        let mut text = fs::read_to_string(&asset.mpd_path).unwrap();
        if case.starts_with("mixed") {
            let codec = if case == "mixed-av1" {
                "av01.0.04M.08"
            } else {
                "hev1.1.6.L60.90"
            };
            text = text.replace("</AdaptationSet>", &format!("<Representation id=\"unsupported\" codecs=\"{codec}\"><SegmentTemplate/></Representation></AdaptationSet>"));
        } else {
            let pr = build_playready_v43(KID, "https://license.example.test/").unwrap();
            let metadata = if case == "custom" {
                "<CUSTOMATTRIBUTES><VALUE>synthetic-private-value</VALUE></CUSTOMATTRIBUTES>"
            } else {
                "<DS_ID>synthetic-private-value</DS_ID>"
            };
            let header = pr
                .wrmheader_xml
                .replace("</DATA>", &format!("{metadata}</DATA>"));
            let pro = base64::engine::general_purpose::STANDARD
                .encode(build_pro(header.as_str()).unwrap());
            let protection = format!(
                "<ContentProtection schemeIdUri=\"urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95\"><mspr:pro xmlns:mspr=\"urn:microsoft:playready\">{pro}</mspr:pro></ContentProtection><Representation"
            );
            text = text.replacen("<Representation", &protection, 1);
        }
        fs::write(&asset.mpd_path, text).unwrap();
        let before = tree_state(cli.root());
        let modules = Modules {
            config: &Ports,
            manifests: &Ports,
            packager: &cli.fakes.packager,
            media_inspect: &cli.fakes.media,
            playready: &cli.playready,
        };
        let expected = if case.starts_with("mixed") {
            "mixed supported and unsupported codecs in one AdaptationSet"
        } else {
            "unsupported PlayReady custom attributes or DS_ID"
        };
        let mpd = asset.mpd_path.to_str().unwrap();
        let output = cli.path("output");
        let mut commands = vec![vec![
            "run",
            "--mpd",
            mpd,
            "--hls-master",
            master.to_str().unwrap(),
            "--output",
            &output,
        ]];
        if case.starts_with("mixed") {
            commands.push(vec!["plan", "--mpd", mpd]);
        }
        for argv in commands {
            let argv: Vec<String> = argv.into_iter().map(ToString::to_string).collect();
            let stdout = SharedBuf::default();
            let mut stderr = SharedBuf::default();
            let code = cli::main(
                &argv,
                &real_key_env(),
                &modules,
                stdout.clone(),
                &mut stderr,
            );
            assert_eq!(code, EXIT_USAGE_ERROR, "{case}: {}", stderr.text());
            assert_eq!(stderr.text(), format!("error: {expected}\n"));
            assert_eq!(stdout.text(), "");
            assert_no_secrets(&stderr.text());
            assert!(!stderr.text().contains("synthetic-private-value"));
        }
        assert_eq!(tree_state(cli.root()), before);
        assert_eq!(cli.fakes.packager.calls(), [] as [PackagerCall; 0]);
        assert!(cli.playready.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn plan_missing_mpd_is_usage_error() {
    let cli = Cli::new();
    let missing = cli.path("nope.mpd");

    let ran = cli.main(&["plan", "--mpd", &missing], &Env::new());

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(
        ran.stderr,
        format!(
            "error: --mpd not found: {}\n",
            portable_path(Path::new(&missing))
        )
    );
    assert_eq!(ran.stdout, "");
}

// ---- run: exit codes  ----

#[test]
fn run_without_key_material_is_usage_error() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &Env::new(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(
        ran.stderr,
        "error: missing required env var(s): DRM_KID, DRM_KEY, DRM_IV\n"
    );
}

#[test]
fn run_missing_mpd_is_usage_error() {
    let cli = Cli::new();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &cli.path("nope.mpd"),
            "--output",
            &cli.path("out"),
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert!(
        ran.stderr.starts_with("error: --mpd not found: "),
        "{}",
        ran.stderr
    );
}

// ---- run: --hls-master / --no-manifests ----

#[test]
fn run_without_hls_master_or_no_manifests_is_usage_error() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &["run", "--mpd", &mpd, "--output", &cli.path("out")],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(
        ran.stderr,
        "error: --hls-master is required unless --no-manifests is given\n"
    );
}

#[test]
fn run_missing_hls_master_file_is_usage_error() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();
    let missing = cli.path("nope.m3u8");

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--hls-master",
            &missing,
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(
        ran.stderr,
        format!(
            "error: --hls-master not found: {}\n",
            portable_path(Path::new(&missing))
        )
    );
}

#[test]
fn run_no_manifests_leaves_manifests_none() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    let report = cli.report("cli-asset");
    assert!(
        [EXIT_ALL_PASS, EXIT_RENDITION_FAIL].contains(&ran.code),
        "{}",
        ran.stderr
    );
    assert_eq!(report["manifests"], Value::Null);
    assert_eq!(
        report["full_enumeration"], false,
        "--no-manifests extends the full-run flag"
    );
}

#[test]
fn run_with_hls_master_populates_manifests() {
    let cli = Cli::new();
    let asset = cli.asset(&[h264_rep("v0").segments(3)], "hls-master-test");
    let master = write_minimal_hls_master(cli.root(), "hls-master-test", 3);

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--hls-master",
            &master.display().to_string(),
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL, "{}", ran.stderr);
    let report = cli.report("hls-master-test");
    assert!(
        report["manifests"]["checks"]
            .get("6-1.pssh-agreement")
            .is_some()
    );
    // The PlayReady objects come from the KID and the manifest's own LA_URL.
    assert_eq!(
        cli.playready.calls.lock().unwrap().clone(),
        [(KID.to_string(), Some(LA_URL.to_string()))]
    );
}

#[test]
fn run_limit_renditions_zero_is_usage_error() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--limit-renditions",
            "0",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: --limit-renditions must be >= 1\n");
}

#[test]
fn run_negative_bounds_are_usage_errors_of_their_own() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--max-workers",
            "-2",
        ],
        &real_key_env(),
    );
    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: --max-workers must be >= 1\n");

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--limit-renditions",
            "-1",
        ],
        &real_key_env(),
    );
    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: --limit-renditions must be >= 1\n");
}

#[test]
fn run_argparse_usage_error_exits_2() {
    let cli = Cli::new();

    let ran = cli.main(&["run", "--mpd", &cli.path("x.mpd")], &real_key_env());

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert!(ran.stderr.contains("--output"), "{}", ran.stderr);
    assert_eq!(ran.stdout, "");
}

#[test]
fn run_reports_rendition_fail_as_exit_1_not_0_or_2() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL, "{}", ran.stderr);
}

// ---- run: --limit-renditions sets full_enumeration=False ----

#[test]
fn run_limit_renditions_marks_partial_run() {
    let cli = Cli::new();
    let asset = cli.asset(
        &[h264_rep("v0"), h264_rep("v1"), h264_rep("v2")],
        "limited-asset",
    );

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--limit-renditions",
            "1",
            "--no-manifests",
        ],
        &real_key_env(),
    );

    let report = cli.report("limited-asset");
    assert_eq!(report["full_enumeration"], false);
    assert_eq!(report["renditions"].as_array().unwrap().len(), 1);
    assert_eq!(
        ran.stderr,
        "WARNING: partial run (--limit-renditions or --no-manifests) -- not a valid \
         completion-gate result\n"
    );
}

#[test]
fn run_without_limit_is_full_enumeration() {
    let cli = Cli::new();
    let asset = cli.asset(&[h264_rep("v0")], "full-asset");
    let master = write_minimal_hls_master(cli.root(), "full-asset", 3);

    cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--hls-master",
            &master.display().to_string(),
        ],
        &real_key_env(),
    );

    let report = cli.report("full-asset");
    assert_eq!(report["full_enumeration"], true);
    assert_eq!(report["renditions"].as_array().unwrap().len(), 1);
    assert!(!report["manifests"].is_null());
}

// ---- run: --no-round-trip reaches the per-rendition check ----

#[test]
fn run_no_round_trip_skips_framemd5_check() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-round-trip",
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(
        cli.report("cli-asset")["renditions"][0]["checks"]["6-4.framemd5"],
        "SKIP: verify_round_trip=False"
    );
}

#[test]
fn run_options_take_the_last_occurrence_and_unique_prefixes_like_argparse() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &cli.path("first.mpd"),
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-round",
            "--no-man",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL, "{}", ran.stderr);
    assert_eq!(
        cli.report("cli-asset")["renditions"][0]["checks"]["6-4.framemd5"],
        "SKIP: verify_round_trip=False"
    );
}

// ---- run: --packager-bin override ----

#[test]
fn run_packager_bin_override_is_used() {
    // No packager on PATH at all: only --packager-bin's directory, prepended to the job's PATH
    // as Python prepends it to os.environ["PATH"], can find the stub.
    let mut cli = Cli::new();
    cli.fakes.packager.on_path = None;
    let custom = cli.root().join("custom-bin");
    let stub = write_stub_packager(&custom, 0, None, None);
    let asset = cli.asset(&[h264_rep("v0").segments(3)], "custom-bin-asset");

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--packager-bin",
            &stub.display().to_string(),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_ne!(ran.code, EXIT_USAGE_ERROR, "{}", ran.stderr);
    // The pipeline runs the bare name (so the records show `packager`); the
    // job's PATH resolved it to the stub.
    let bins: BTreeSet<String> = cli
        .fakes
        .packager
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            PackagerCall::Decrypt { packager_bin, .. }
            | PackagerCall::Encrypt { packager_bin, .. } => Some(packager_bin),
            _ => None,
        })
        .collect();
    assert_eq!(bins, BTreeSet::from(["packager".to_string()]));
    let resolved = cli.fakes.packager.resolved.lock().unwrap().clone();
    assert_ne!(resolved.len(), 0);
    assert!(
        resolved
            .iter()
            .all(|program| program.as_ref() == Some(&stub))
    );
}

#[test]
fn run_packager_bin_directory_resolves_its_packager() {
    let mut cli = Cli::new();
    cli.fakes.packager.on_path = None;
    let custom = cli.root().join("custom-dir");
    let stub = write_stub_packager(&custom, 0, None, None);
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--packager-bin",
            &custom.display().to_string(),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL, "{}", ran.stderr);
    assert!(cli.fakes.packager.calls().contains(&PackagerCall::Decrypt {
        input: cli.root().join("out/cli-asset/_work/v0/source_merged.mp4"),
        output: cli.root().join("out/cli-asset/clear/video/v0/clear.mp4"),
        packager_bin: "packager".to_string(),
    }));
    let resolved = cli.fakes.packager.resolved.lock().unwrap().clone();
    assert!(
        resolved
            .iter()
            .all(|program| program.as_ref() == Some(&stub))
    );
}

#[test]
fn run_packager_bin_file_must_be_named_packager() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();
    let wrong = cli.root().join("shaka");
    fs::write(&wrong, "#!/bin/sh\n").unwrap();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--packager-bin",
            &wrong.display().to_string(),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(
        ran.stderr,
        format!(
            "error: --packager-bin must be a directory containing 'packager{}', or a file named \
             'packager{}' (got {})\n",
            std::env::consts::EXE_SUFFIX,
            std::env::consts::EXE_SUFFIX,
            portable_path(&wrong)
        )
    );
}

// ---- : no secret ever printed to stdout/stderr ----

#[test]
fn run_never_prints_key_or_iv() {
    let cli = Cli::new();
    let asset = cli.asset(&[h264_rep("v0").segments(3)], "secrets");
    let master = write_minimal_hls_master(cli.root(), "secrets", 3);

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--hls-master",
            &master.display().to_string(),
        ],
        &real_key_env(),
    );

    assert_no_secrets(&ran.stdout);
    assert_no_secrets(&ran.stderr);
}

// ---- run: --log-format {text,jsonl} ----

#[test]
fn run_log_format_defaults_to_text_unchanged() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(
        ran.stdout,
        "asset cli-asset: FAIL (1 renditions)\n  v0: FAIL (6-1.boundary-agreement, \
         6-1.iv-agreement)\n"
    );
    for line in ran.stdout.lines() {
        assert!(serde_json::from_str::<Value>(line).is_err());
    }
}

#[test]
fn run_text_summary_lists_the_manifests_verdict() {
    let cli = Cli::new();
    let asset = cli.asset(&[h264_rep("v0").segments(3)], "summary");
    let master = write_minimal_hls_master(cli.root(), "summary", 3);

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--hls-master",
            &master.display().to_string(),
        ],
        &real_key_env(),
    );

    assert_eq!(
        ran.stdout,
        "asset summary: FAIL (1 renditions)\n  v0: FAIL (6-1.boundary-agreement, \
         6-1.iv-agreement)\n  manifests: FAIL (6-1.pssh-agreement)\n"
    );
    assert!(
        ran.stderr.is_empty(),
        "a full run warns about nothing: {}",
        ran.stderr
    );
}

const BASE_KEYS: [&str; 7] = [
    "ts",
    "stage",
    "level",
    "message",
    "rendition_id",
    "check_id",
    "duration_s",
];
const TOOL_KEYS: [&str; 8] = [
    "event",
    "tool",
    "call_id",
    "argv",
    "returncode",
    "stdout",
    "stderr",
    "truncated",
];

#[test]
fn run_log_format_jsonl_emits_one_json_object_per_line() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
            "--log-format",
            "jsonl",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL);
    let lines: Vec<&str> = ran.stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_ne!(lines.len(), 0);
    let mut tools = 0;
    for line in &lines {
        let record: serde_json::Map<String, Value> = serde_json::from_str(line).unwrap();
        let tool = record["stage"] == "tool";
        tools += usize::from(tool);
        for key in record.keys() {
            assert!(
                BASE_KEYS.contains(&key.as_str()) || (tool && TOOL_KEYS.contains(&key.as_str())),
                "{key} in {line}"
            );
        }
        assert!(
            [
                "merge", "decrypt", "encrypt", "manifest", "report", "raw", "tool"
            ]
            .contains(&record["stage"].as_str().unwrap())
        );
        assert!(line.is_ascii(), "json.dumps escapes non-ASCII");
    }
    assert!(tools > 0, "the packager's tool records join the stream");
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("asset cli-asset:"))
    );
    assert!(
        lines
            .last()
            .unwrap()
            .contains("\"message\": \"asset cli-asset: FAIL\"")
    );
}

#[test]
fn run_log_format_jsonl_never_leaks_key_or_iv() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
            "--log-format",
            "jsonl",
        ],
        &real_key_env(),
    );

    assert!(
        ran.stdout.contains("***REDACTED***"),
        "the packager argv is redacted"
    );
    assert_no_secrets(&ran.stdout);
    assert_no_secrets(&ran.stderr);
}

#[test]
fn run_log_format_jsonl_scrubs_tool_output_in_every_encoding() {
    // mp4dump prints the cbcs tenc constant IV as `[20 20 …]`, and a tool may echo the key in
    // upper case or base64: `run` registers the job's key and IV with the tool runner, which
    // scrubs every encoding from every record. The stub prints what Bento4 prints.
    let mut cli = Cli::new();
    let bracketed = |hex: &str| {
        let pairs: Vec<&str> = (0..hex.len()).step_by(2).map(|i| &hex[i..i + 2]).collect();
        format!("[{}]", pairs.join(" "))
    };
    let key_b64 = base64::engine::general_purpose::STANDARD.encode(hex::decode(KEY).unwrap());
    let stdout = format!(
        "[{{\"name\":\"moov\",\"children\":[{{\"name\":\"tenc\",\"version\":1,\"default_isProtected\":1,\"default_Per_Sample_IV_Size\":0,\"default_KID\":\"{}\",\"default_constant_IV_size\":16,\"default_constant_IV\":\"{}\"}}]}}]\n",
        bracketed(KID),
        bracketed(IV)
    );
    let spec = drmr_test_support::ToolSpec {
        stdout: stdout.into_bytes(),
        stderr: format!("key {key_b64} {}\n", KEY.to_uppercase()).into_bytes(),
        ..Default::default()
    };
    let stub = drmr_test_support::install(&cli.root().join("mp4dump"), &spec).unwrap();
    cli.fakes.media.tenc_iv = Box::new(move |runner, path| mp4dump_tenc_iv(runner, &stub, path));
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
            "--log-format",
            "jsonl",
        ],
        &real_key_env(),
    );

    let outputs: Vec<Value> = ran
        .stdout
        .lines()
        .filter(|line| line.contains("\"mp4dump\""))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["event"] == "tool-output")
        .collect();
    assert!(!outputs.is_empty(), "mp4dump ran through the tool runner");
    for form in [
        IV.to_string(),
        IV.to_uppercase(),
        bracketed(IV),
        KEY.to_string(),
        KEY.to_uppercase(),
        key_b64.clone(),
    ] {
        assert!(!ran.stdout.contains(&form), "{form}");
    }
    for record in &outputs {
        assert!(
            record["stdout"]
                .as_str()
                .unwrap()
                .contains("[***REDACTED***]"),
            "{record}"
        );
        assert!(
            record["stderr"]
                .as_str()
                .unwrap()
                .starts_with("key ***REDACTED***"),
            "{record}"
        );
        assert!(
            record["stdout"].as_str().unwrap().contains("[00 01 02 03"),
            "the KID is public signaling"
        );
    }
    // The pipeline compared the real IV: the stub's tenc IV is the configured one.
    assert_eq!(
        cli.report("cli-asset")["renditions"][0]["checks"]["6-1.iv-agreement"],
        "PASS"
    );
}

#[test]
fn run_log_format_invalid_choice_is_usage_error() {
    let cli = Cli::new();
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
            "--log-format",
            "xml",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert!(ran.stderr.contains("xml"), "{}", ran.stderr);
}

/// A log stream whose reader went away.
struct ClosedPipe;

impl Write for ClosedPipe {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn run_log_format_jsonl_into_a_closed_pipe_exits_1() {
    // Python's print raises BrokenPipeError (an uncaught OSError: exit 1).
    let cli = Cli::new();
    let mpd = cli.asset_mpd();
    let argv = [
        "run",
        "--mpd",
        &mpd,
        "--output",
        &cli.path("out"),
        "--no-manifests",
        "--log-format",
        "jsonl",
    ];

    assert_eq!(
        cli.main_with(&argv, &real_key_env(), ClosedPipe),
        EXIT_RENDITION_FAIL
    );
    assert_eq!(
        cli.main_with(&argv[..6], &real_key_env(), ClosedPipe),
        EXIT_RENDITION_FAIL
    );
}

// ---- run: a missing/unspawnable packager binary is a clean usage error ----

#[test]
fn run_missing_packager_binary_is_usage_error_not_a_traceback() {
    let mut cli = Cli::new();
    cli.fakes.packager.on_path = None; // nothing named "packager" on PATH
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: could not run Shaka Packager\n");
    assert!(
        !cli.root()
            .join("out/cli-asset/reports/phase1-summary.json")
            .exists()
    );
}

#[test]
fn run_packager_timeout_is_a_usage_error() {
    // external packager timeout: the runner gives up on a hung packager, the wrapper raises
    // PackagerError, and main maps it to exit 2 (an environmental fault, not a FAIL).
    let mut cli = Cli::new();
    let stub = write_stub_packager(&cli.root().join("hang"), 0, None, Some(5));
    cli.fakes.packager.on_path = Some(stub);
    cli.fakes.packager.timeout = Some(Duration::from_millis(200));
    let mpd = cli.asset_mpd();
    let started = Instant::now();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the timeout bounded the run"
    );
    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: Shaka Packager timed out after 0.2s\n");
}

#[test]
fn run_value_errors_from_the_modules_are_usage_errors() {
    // A manifest PRO without an LA_URL: PlayReadyError (a ValueError) → exit 2.
    let mut cli = Cli::new();
    cli.fakes.manifests.la_url = None;
    let asset = cli.asset(&[h264_rep("v0")], "no-la-url");
    let master = write_minimal_hls_master(cli.root(), "no-la-url", 3);

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &asset.mpd_path.display().to_string(),
            "--output",
            &cli.path("out"),
            "--hls-master",
            &master.display().to_string(),
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_USAGE_ERROR);
    assert_eq!(ran.stderr, "error: LA_URL must be non-empty\n");
}

#[test]
fn run_uncaught_errors_exit_1_like_a_python_traceback() {
    let mut cli = Cli::new();
    cli.fakes.media.tenc_iv = Box::new(|_, _| {
        Err(PyError::Os(
            "[Errno 13] Permission denied: 'init.mp4'".into(),
        ))
    });
    let mpd = cli.asset_mpd();

    let ran = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
        ],
        &real_key_env(),
    );

    assert_eq!(ran.code, EXIT_RENDITION_FAIL);
    assert_eq!(
        ran.stderr,
        "OSError: [Errno 13] Permission denied: 'init.mp4'\n"
    );
    assert!(
        !cli.root()
            .join("out/cli-asset/reports/phase1-summary.json")
            .exists()
    );
}

#[test]
fn run_and_plan_help_explain_their_inputs_and_output() {
    let cli = Cli::new();

    let ran = cli.main(&["run", "--help"], &real_key_env());

    assert_eq!(ran.code, EXIT_ALL_PASS);
    for text in [
        "repackage every in-scope rendition of one asset",
        "path to the source DASH manifest",
        "output root (output/<asset_id>/ is created under it)",
        "Name the output asset (default: the source title directory name)",
        "Shaka Packager binary/path override (default: `packager` on PATH)",
        "maximum rendition workers for this asset (default: CPU bound)",
        "skip the 6-4 framemd5 round-trip check",
        "Process at most N renditions for debugging; the result is incomplete",
        "--log-format",
    ] {
        assert!(
            ran.stdout.contains(text),
            "{text} missing from:\n{}",
            ran.stdout
        );
    }
    assert_no_secrets(&ran.stdout);
    let ran = cli.main(&["plan", "--help"], &real_key_env());
    assert_eq!(ran.code, EXIT_ALL_PASS);
    assert!(
        ran.stdout
            .contains("enumerate renditions only; writes nothing (read-only)")
    );
}

#[test]
fn run_jsonl_registers_source_and_generated_signaling_before_tool_records() {
    // Exercise cmd_run, the pipeline and ToolRunner with the real PlayReady
    // builder; existing fake media ports keep this a bounded synthetic test.
    let mut cli = Cli::new();
    let url = "https://license.example.invalid/rights?token=synthetic-license-token&x=1";
    cli.fakes.manifests.la_url = Some(url.into());
    let asset = cli.asset(&[h264_rep("v0")], "private-signaling");
    let master = write_minimal_hls_master(cli.root(), "private-signaling", 3);
    let objects = drmr_core::playready::build_playready_v43(KID, url).unwrap();
    let probe = drmr_test_support::ToolSpec {
        stderr: format!(
            "{}\n{}\n{}\n{}\n{}\n",
            url, objects.wrmheader_xml, objects.pro_b64, objects.pssh_b64, objects.pssh_hex
        )
        .into_bytes(),
        stdout: b"[]".to_vec(),
        ..Default::default()
    };
    let stub = drmr_test_support::install(&cli.root().join("metadata-probe"), &probe).unwrap();
    cli.fakes.media.tenc_iv = Box::new(move |runner, path| mp4dump_tenc_iv(runner, &stub, path));
    let ports = drmr_core::pipeline_deps::Ports;
    let modules = Modules {
        playready: &ports,
        ..cli.modules()
    };
    let argv = vec![
        "run".into(),
        "--mpd".into(),
        asset.mpd_path.display().to_string(),
        "--output".into(),
        cli.path("out"),
        "--hls-master".into(),
        master.display().to_string(),
        "--log-format".into(),
        "jsonl".into(),
    ];
    let stdout = SharedBuf::default();
    let mut stderr = SharedBuf::default();
    let code = cli::main(
        &argv,
        &real_key_env(),
        &modules,
        stdout.clone(),
        &mut stderr,
    );
    assert_eq!(code, EXIT_RENDITION_FAIL);
    let log = stdout.text();
    assert!(log.contains("\"command\""));
    assert!(log.contains("metadata-probe") || log.contains("mp4dump"));
    for secret in [
        url,
        objects.wrmheader_xml.as_str(),
        objects.pro_b64.as_str(),
        objects.pssh_b64.as_str(),
        objects.pssh_hex.as_str(),
    ] {
        assert!(!log.contains(secret), "signaling in JSONL");
        assert!(!stderr.text().contains(secret), "signaling in stderr");
    }
    // Redaction must not alter the signaling submitted to encryption.
    assert!(cli.fakes.packager.calls().iter().any(|call| matches!(call,
        PackagerCall::Encrypt { pssh_hex: Some(boxes), .. } if boxes.contains(&objects.pssh_hex))));
}

#[test]
fn run_jsonl_scrubs_compact_tool_output_for_accepted_spaced_key_material() {
    let mut cli = Cli::new();
    let spec = drmr_test_support::ToolSpec {
        stdout: b"[]".to_vec(),
        stderr: format!(
            "{} {}",
            KEY,
            base64::engine::general_purpose::STANDARD.encode(hex::decode(KEY).unwrap())
        )
        .into_bytes(),
        ..Default::default()
    };
    let stub = drmr_test_support::install(&cli.root().join("spaced-key-probe"), &spec).unwrap();
    cli.fakes.media.tenc_iv = Box::new(move |runner, path| mp4dump_tenc_iv(runner, &stub, path));
    let mpd = cli.asset_mpd();
    let mut env = real_key_env();
    let spaced = KEY
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    env.insert("DRM_KEY".into(), spaced);
    let result = cli.main(
        &[
            "run",
            "--mpd",
            &mpd,
            "--output",
            &cli.path("out"),
            "--no-manifests",
            "--log-format",
            "jsonl",
        ],
        &env,
    );
    assert_eq!(result.code, EXIT_RENDITION_FAIL);
    assert!(result.stdout.contains("mp4dump"));
    assert_no_secrets(&result.stdout);
    assert_no_secrets(&result.stderr);
}
