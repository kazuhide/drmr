//! Process-level argument checks, help, exit codes, and read-only manifest planning.
//! Packaging requires external media tools; these tests use synthetic key material only.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";

fn drmr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .env("DRM_KID", KID)
        .env("DRM_KEY", KEY)
        .env("DRM_IV", IV)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Neither stream carries the key or the IV, in any case.
fn assert_clean(output: &Output) {
    for stream in [&output.stdout, &output.stderr] {
        let lowered = text(stream).to_lowercase();
        assert!(!lowered.contains(KEY) && !lowered.contains(IV), "{lowered}");
    }
}

fn usage_error(output: &Output, message: &str) {
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert_eq!(text(&output.stderr), format!("error: {message}\n"));
    assert_eq!(text(&output.stdout), "");
    assert_clean(output);
}

fn existing_file(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, "<MPD/>").unwrap();
    path.display().to_string()
}

#[test]
fn run_without_a_required_flag_is_a_usage_error() {
    let output = drmr(&["run", "--mpd", "x.mpd"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--output"));
    assert_eq!(text(&output.stdout), "");
    assert_clean(&output);
}

#[test]
fn run_with_an_invalid_log_format_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let mpd = existing_file(dir.path(), "m.mpd");
    let output = drmr(&[
        "run",
        "--mpd",
        &mpd,
        "--output",
        "out",
        "--no-manifests",
        "--log-format",
        "xml",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("xml"));
    assert_clean(&output);
}

#[test]
fn run_and_plan_check_their_arguments_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let mpd = existing_file(dir.path(), "m.mpd");
    let missing = dir.path().join("nope.mpd").display().to_string();
    let missing_master = dir.path().join("nope.m3u8").display().to_string();
    let wrong_bin = existing_file(dir.path(), "shaka");

    usage_error(
        &drmr(&["run", "--mpd", &missing, "--output", "out"]),
        &format!("--mpd not found: {missing}"),
    );
    usage_error(
        &drmr(&["plan", "--mpd", &missing]),
        &format!("--mpd not found: {missing}"),
    );
    usage_error(
        &drmr(&[
            "run",
            "--mpd",
            &mpd,
            "--output",
            "out",
            "--limit-renditions",
            "0",
        ]),
        "--limit-renditions must be >= 1",
    );
    usage_error(
        &drmr(&[
            "run",
            "--mpd",
            &mpd,
            "--output",
            "out",
            "--max-workers",
            "0",
        ]),
        "--max-workers must be >= 1",
    );
    usage_error(
        &drmr(&[
            "run",
            "--mpd",
            &mpd,
            "--output",
            "out",
            "--packager-bin",
            &wrong_bin,
        ]),
        &format!(
            "--packager-bin must be a directory containing 'packager', or a file named \
             'packager' (got {wrong_bin})"
        ),
    );
    usage_error(
        &drmr(&["run", "--mpd", &mpd, "--output", "out"]),
        "--hls-master is required unless --no-manifests is given",
    );
    usage_error(
        &drmr(&[
            "run",
            "--mpd",
            &mpd,
            "--output",
            "out",
            "--no-manifests",
            "--hls-master",
            &missing_master,
        ]),
        &format!("--hls-master not found: {missing_master}"),
    );
}

#[test]
fn run_help_lists_options_and_no_secret() {
    let output = drmr(&["run", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let help = text(&output.stdout);
    for line in [
        "repackage every in-scope rendition of one asset",
        "--mpd <MPD>",
        "path to the source DASH manifest",
        "--output <OUTPUT>",
        "--asset-id <ASSET_ID>",
        "--packager-bin <PACKAGER_BIN>",
        "--max-workers <MAX_WORKERS>",
        "--no-round-trip",
        "--limit-renditions <N>",
        "--hls-master <HLS_MASTER>",
        "--no-manifests",
        "--manifest-local-url-base <MANIFEST_LOCAL_URL_BASE>",
        "--log-format <LOG_FORMAT>",
        "[default: text]",
    ] {
        assert!(help.contains(line), "{line} missing:\n{help}");
    }
    assert_clean(&output);

    let output = drmr(&["plan", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("enumerate renditions only; writes nothing (read-only)"));

    let output = drmr(&["--help"]);
    let help = text(&output.stdout);
    assert!(help.contains("run ") && help.contains("plan "), "{help}");
}

#[test]
fn plan_reports_a_manifest_config_cannot_read_as_a_usage_error() {
    // A manifest without a <BaseURL> is a configuration error: exit 2.
    let dir = tempfile::tempdir().unwrap();
    let mpd = existing_file(dir.path(), "m.mpd");
    let output = drmr(&["plan", "--mpd", &mpd]);
    usage_error(
        &output,
        &format!("{mpd}: no <BaseURL> found (required to resolve segment paths)"),
    );
}

/// Synthetic tree: `download/<manifest host>/<title>/dash/manifest.mpd` and,
/// per rendition, `init.mp4` and `seg_<i>.m4s` under `download/<cdn host>/<title>/<type>/<id>/`.
fn synthetic_asset(root: &Path, reps: &[(&str, &str, &str)], segments: usize) -> String {
    let download = root.join("download");
    let mpd = download.join("manifest.example.test/title-abc123/dash/manifest.mpd");
    std::fs::create_dir_all(mpd.parent().unwrap()).unwrap();
    let mut body = String::new();
    for (id, content_type, codec) in reps {
        write!(
            body,
            "<AdaptationSet mimeType=\"{content_type}/mp4\" contentType=\"{content_type}\">\
             <Representation id=\"{id}\" codecs=\"{codec}\" bandwidth=\"1\"><SegmentTemplate \
             timescale=\"24000\" startNumber=\"0\" duration=\"144000\" \
             media=\"{content_type}/{id}/seg_$Number$.m4s\" \
             initialization=\"{content_type}/{id}/init.mp4\"/></Representation></AdaptationSet>"
        )
        .unwrap();
        let dir = download.join(format!("cdn.example.test/title-abc123/{content_type}/{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("init.mp4"), format!("INIT:{id}")).unwrap();
        // Out of numeric order on purpose: config sorts seg_2 before seg_10.
        for index in (0..segments).rev() {
            std::fs::write(dir.join(format!("seg_{index}.m4s")), "SEG").unwrap();
        }
    }
    std::fs::write(
        &mpd,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD \
             xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\"><BaseURL>\
             https://cdn.example.test/title-abc123/</BaseURL><Period id=\"p0\">{body}</Period>\
             </MPD>\n"
        ),
    )
    .unwrap();
    mpd.display().to_string()
}

#[test]
fn plan_lists_the_renditions_config_enumerates_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mpd = synthetic_asset(
        dir.path(),
        &[
            ("v0", "video", "avc1.42C00C"),
            ("vp9", "video", "vp09.00.10.08"),
            ("a0", "audio", "mp4a.40.2"),
        ],
        12,
    );
    let before = std::fs::read_dir(dir.path()).unwrap().count();
    let output = drmr(&["plan", "--mpd", &mpd]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(
        text(&output.stdout),
        "asset_id: title-abc123\nrenditions: 2\n  v0  video  avc1.42C00C           12 \
         segments\n  a0  audio  mp4a.40.2             12 segments\n"
    );
    assert_eq!(text(&output.stderr), "");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
    let output = drmr(&["plan", "--mpd", &mpd, "--asset-id", "override"]);
    assert!(text(&output.stdout).starts_with("asset_id: override\n"));
    assert_clean(&output);
}
