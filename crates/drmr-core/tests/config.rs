//! Synthetic asset discovery, rendition metadata, path confinement, and key validation.

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

#[path = "support/dash_fixture.rs"]
mod dash_fixture;

use dash_fixture::{
    RepSpec, audio_rep, build_synthetic_dash_asset, fake_widevine_pssh_hex, h264_rep, h265_rep,
    vp9_rep,
};
use drmr_core::config::{
    ConfigError, KeyMaterial, Rendition, enumerate_renditions, load_asset, load_key_material,
    segment_duration_seconds,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const VALID_KID: &str = "000102030405060708090a0b0c0d0e0f";
const VALID_KEY: &str = "10101010101010101010101010101010";
const VALID_IV: &str = "20202020202020202020202020202020";
const TITLE: &str = "title-abc123";

fn key() -> KeyMaterial {
    KeyMaterial::new(VALID_KID, VALID_KEY, VALID_IV).unwrap()
}

/// A temporary directory by its canonical path (macOS puts them behind /var → /private/var;
/// the enumerator resolves symbolic links, as Python's `Path.resolve` does).
fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn value_error(result: Result<impl std::fmt::Debug, ConfigError>) -> String {
    match result {
        Err(ConfigError::Value(message)) => message,
        other => panic!("expected a ValueError, got {other:?}"),
    }
}

// ---- : the full in-scope set, counted from the parse result -----------------------

#[test]
fn enumerate_renditions_matches_bbb_like_shape() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(
        &root,
        &[
            h264_rep("h264-lo"),
            h264_rep("h264-mid"),
            h264_rep("h264-hi"),
            h265_rep("h265-lo"),
            h265_rep("h265-mid"),
            h265_rep("h265-hi"),
            vp9_rep("vp9-lo"),
            vp9_rep("vp9-mid"),
            vp9_rep("vp9-hi"),
            audio_rep("audio-lo"),
            audio_rep("audio-hi"),
        ],
        TITLE,
    );
    let renditions = enumerate_renditions(&asset.mpd_path).unwrap();
    // 3 H.264 + 3 H.265 + 2 audio in scope; the VP9 three excluded (ENUM-02).
    assert_eq!(renditions.len(), 8);
    let mut codecs: Vec<&str> = renditions.iter().map(|r| r.codec.as_str()).collect();
    codecs.sort_unstable();
    let mut expected = [["avc1.42C00C"; 3], ["hvc1.1.6.L60.90"; 3]].concat();
    expected.extend(["mp4a.40.2"; 2]);
    expected.sort_unstable();
    assert_eq!(codecs, expected);
}

// ---- : VP9 and other out-of-scope tracks excluded ----------------------------------

#[test]
fn enumerate_renditions_excludes_vp9() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(
        &root,
        &[
            h264_rep("h264-a"),
            vp9_rep("vp9-a"),
            vp9_rep("vp9-b"),
            audio_rep("audio-a"),
        ],
        TITLE,
    );
    let renditions = enumerate_renditions(&asset.mpd_path).unwrap();
    let mut ids: Vec<&str> = renditions.iter().map(|r| r.rep_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["audio-a", "h264-a"]);
    assert!(renditions.iter().all(|r| r.codec != "vp9"));
}

#[test]
fn mixed_codec_adaptation_sets_are_rejected_before_loading_an_asset() {
    for codec in ["av01.0.04M.08", "hev1.1.6.L60.90"] {
        let (_dir, root) = tempdir();
        let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0")], TITLE);
        let text = std::fs::read_to_string(&asset.mpd_path).unwrap();
        std::fs::write(&asset.mpd_path, text.replace("</AdaptationSet>", &format!("<Representation id=\"unsupported\" codecs=\"{codec}\"><SegmentTemplate/></Representation></AdaptationSet>"))).unwrap();
        for message in [
            value_error(enumerate_renditions(&asset.mpd_path)),
            value_error(load_asset(
                &asset.mpd_path,
                key(),
                &root.join("output"),
                None,
            )),
        ] {
            assert_eq!(
                message,
                "mixed supported and unsupported codecs in one AdaptationSet"
            );
        }
        assert!(!root.join("output").exists());
    }
}

// ---- : other shapes, no hardcoded count --------------------------------------------

#[test]
fn enumerate_renditions_handles_arbitrary_shapes() {
    for (video_count, audio_count) in [(2, 1), (7, 4), (1, 1)] {
        let (_dir, root) = tempdir();
        let mut reps: Vec<RepSpec> = (0..video_count)
            .map(|i| h264_rep(&format!("v{i}")))
            .collect();
        reps.extend((0..audio_count).map(|i| audio_rep(&format!("a{i}"))));
        let asset = build_synthetic_dash_asset(&root, &reps, TITLE);
        let renditions = enumerate_renditions(&asset.mpd_path).unwrap();
        assert_eq!(renditions.len(), video_count + audio_count);
        let count = |kind: &str| renditions.iter().filter(|r| r.content_type == kind).count();
        assert_eq!(count("video"), video_count);
        assert_eq!(count("audio"), audio_count);
    }
}

// ---- : segment_duration_seconds derives 6.0, never a constant ------------------------

#[test]
fn segment_duration_seconds_derived_from_source() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(
        &root,
        &[
            h264_rep("v0").timing(24_000, 144_000),
            audio_rep("a0").timing(48_000, 288_000),
        ],
        TITLE,
    );
    let renditions: HashMap<String, Rendition> = enumerate_renditions(&asset.mpd_path)
        .unwrap()
        .into_iter()
        .map(|r| (r.rep_id.clone(), r))
        .collect();
    assert_eq!(
        segment_duration_seconds(&renditions["v0"])
            .unwrap()
            .to_bits(),
        6.0_f64.to_bits()
    );
    assert_eq!(
        segment_duration_seconds(&renditions["a0"])
            .unwrap()
            .to_bits(),
        6.0_f64.to_bits()
    );
}

#[test]
fn segment_duration_seconds_is_not_hardcoded() {
    // A source with a different duration must not read back as 6.0.
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0").timing(12_000, 48_000)], TITLE);
    let rendition = &enumerate_renditions(&asset.mpd_path).unwrap()[0];
    assert_eq!(
        segment_duration_seconds(rendition).unwrap().to_bits(),
        4.0_f64.to_bits()
    );
}

#[test]
fn segment_duration_seconds_of_a_zero_timescale_is_python_zero_division() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0").timing(0, 48_000)], TITLE);
    let rendition = &enumerate_renditions(&asset.mpd_path).unwrap()[0];
    let error = segment_duration_seconds(rendition).unwrap_err();
    assert!(matches!(error, ConfigError::ZeroDivision));
    assert_eq!(error.to_string(), "division by zero");
}

// ---- segment / init path resolution (the download/<host><path> mirroring convention) ----------

#[test]
fn enumerate_renditions_resolves_paths_and_numeric_segment_order() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0").segments(12)], TITLE);
    let rendition = &enumerate_renditions(&asset.mpd_path).unwrap()[0];
    assert!(rendition.init_path.is_file());
    assert_eq!(rendition.init_path.file_name().unwrap(), "init.mp4");
    assert_eq!(rendition.segment_paths.len(), 12);
    // Numeric order: seg_2 before seg_10, not lexical.
    let names: Vec<String> = rendition
        .segment_paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    let expected: Vec<String> = (0..12).map(|i| format!("seg_{i}.m4s")).collect();
    assert_eq!(names, expected);
    assert!(rendition.segment_paths.iter().all(|path| path.is_file()));
    assert_eq!(rendition.rel_dir, Path::new("video/v0"));
}

#[test]
fn enumerate_renditions_extracts_widevine_pssh_hex() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0")], TITLE);
    let rendition = &enumerate_renditions(&asset.mpd_path).unwrap()[0];
    assert_eq!(
        rendition.widevine_pssh_hex.as_deref(),
        Some(fake_widevine_pssh_hex().as_str())
    );
}

#[test]
fn enumerate_renditions_widevine_pssh_none_when_absent() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0").without_widevine_pssh()], TITLE);
    let rendition = &enumerate_renditions(&asset.mpd_path).unwrap()[0];
    assert_eq!(rendition.widevine_pssh_hex, None);
}

// ---- KeyMaterial / load_key_material ------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn load_key_material_reads_scheme_invariant_env_vars() {
    let loaded = load_key_material(Some(&env(&[
        ("DRM_KID", VALID_KID),
        ("DRM_KEY", VALID_KEY),
        ("DRM_IV", VALID_IV),
    ])))
    .unwrap();
    assert_eq!(loaded, key());
    assert_eq!(
        (loaded.key_id_hex(), loaded.key_hex(), loaded.iv_hex()),
        (VALID_KID, VALID_KEY, VALID_IV)
    );
}

#[test]
fn load_key_material_raises_on_missing_env_var() {
    for missing in ["DRM_KID", "DRM_KEY", "DRM_IV"] {
        let mut values = env(&[
            ("DRM_KID", VALID_KID),
            ("DRM_KEY", VALID_KEY),
            ("DRM_IV", VALID_IV),
        ]);
        values.remove(missing);
        let message = value_error(load_key_material(Some(&values)));
        assert!(message.contains(missing), "{message}");
    }
}

#[test]
fn key_material_rejects_invalid_hex() {
    for (field, bad) in [
        ("key_id_hex", "not-hex"),
        ("key_id_hex", "aabb"),
        ("key_hex", "not-hex"),
        ("iv_hex", "aabbccdd"),
    ] {
        let (mut kid, mut key, mut iv) = (VALID_KID, VALID_KEY, VALID_IV);
        match field {
            "key_id_hex" => kid = bad,
            "key_hex" => key = bad,
            _ => iv = bad,
        }
        let message = value_error(KeyMaterial::new(kid, key, iv));
        assert!(message.starts_with(field), "{message}");
    }
}

#[test]
fn key_material_debug_prints_no_value() {
    // Debug formatting hides the key and IV (the KID stays: public); the debug formatting
    // prints no value at all (: KeyMaterial is never Debug-printed).
    let shown = format!("{:?} {:#?}", key(), key());
    for value in [VALID_KEY, VALID_IV, VALID_KID] {
        assert!(!shown.contains(value), "{shown}");
        assert!(!shown.contains(&value.to_uppercase()), "{shown}");
    }
    assert!(shown.starts_with("KeyMaterial"));
}

#[test]
fn key_material_and_load_key_material_match_python() {
    let oracle = oracle();
    for case in oracle["key_material"].as_array().unwrap() {
        let values: Vec<&str> = case[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let actual = KeyMaterial::new(values[0], values[1], values[2])
            .map(|material| material.key_id_hex().to_string())
            .map_err(|error| error.to_string());
        assert_eq!(actual, expected(&case[1]), "{case}");
    }
    for case in oracle["load_key_material"].as_array().unwrap() {
        let values: HashMap<String, String> = case[0]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().unwrap().to_string()))
            .collect();
        let actual = load_key_material(Some(&values))
            .map(|material| material.key_id_hex().to_string())
            .map_err(|error| error.to_string());
        assert_eq!(actual, expected(&case[1]), "{case}");
    }
}

/// Runs in a child process with the variables set (a test cannot set its own environment
/// safely): `load_key_material(None)` reads the process environment.
#[test]
#[ignore = "run by load_key_material_reads_the_process_environment in a child process"]
fn child_reads_the_process_environment() {
    let loaded = load_key_material(None).unwrap();
    assert_eq!(loaded, key());
}

#[test]
fn load_key_material_reads_the_process_environment() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "child_reads_the_process_environment",
            "--quiet",
        ])
        .env("DRM_KID", VALID_KID)
        .env("DRM_KEY", VALID_KEY)
        .env("DRM_IV", VALID_IV)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "child_reads_the_process_environment",
            "--quiet",
        ])
        .env_remove("DRM_KID")
        .env("DRM_KEY", VALID_KEY)
        .env("DRM_IV", VALID_IV)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "a missing DRM_KID fails the load");
}

// ---- Asset / load_asset (: asset_id = the outer title id) ---------------------------------

#[test]
fn load_asset_derives_asset_id_from_base_url_title_segment() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0")], "the-title-id");
    let loaded = load_asset(&asset.mpd_path, key(), &root.join("output"), None).unwrap();
    assert_eq!(loaded.asset_id, "the-title-id");
    assert_eq!(
        portable_path(&loaded.output_dir),
        portable_path(&root.join("output/the-title-id"))
    );
    assert_eq!(loaded.renditions.len(), 1);
}

#[test]
fn load_asset_asset_id_override() {
    let (_dir, root) = tempdir();
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("v0")], "the-title-id");
    let loaded = load_asset(
        &asset.mpd_path,
        key(),
        &root.join("output"),
        Some("override-id"),
    )
    .unwrap();
    assert_eq!(loaded.asset_id, "override-id");
}

// ---- security: the manifest is untrusted input (traversal / DoS hardening) --------------------

/// A hand-crafted (possibly malicious) MPD written directly, bypassing the fixture builder.
fn write_raw_mpd(root: &Path, xml: &str) -> PathBuf {
    let mpd_path = root.join("download/manifest.example.test/manifest.mpd");
    std::fs::create_dir_all(mpd_path.parent().unwrap()).unwrap();
    std::fs::write(&mpd_path, xml).unwrap();
    mpd_path
}

fn minimal_mpd(base_url: &str, adaptation_sets: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" \
         mediaPresentationDuration=\"PT6.0S\" minBufferTime=\"PT2S\" \
         profiles=\"urn:mpeg:dash:profile:isoff-live:2011\">\
         <BaseURL>{base_url}</BaseURL><Period id=\"p0\">{adaptation_sets}</Period></MPD>\n"
    )
}

fn video_set(rep_id: &str, media: &str, init: &str) -> String {
    format!(
        "<AdaptationSet mimeType=\"video/mp4\" contentType=\"video\">\
         <Representation id=\"{rep_id}\" codecs=\"avc1.42C00C\" bandwidth=\"1\">\
         <SegmentTemplate timescale=\"24000\" startNumber=\"0\" duration=\"144000\" \
         media=\"{media}\" initialization=\"{init}\"/></Representation></AdaptationSet>"
    )
}

#[test]
fn enumerate_renditions_rejects_path_traversal_in_representation_id() {
    let (_dir, root) = tempdir();
    let mpd_path = write_raw_mpd(
        &root,
        &minimal_mpd(
            "https://cdn.example.test/title/",
            &video_set(
                "../../../ESCAPED_MARKER_A",
                "video/v0/seg_$Number$.m4s",
                "video/v0/init.mp4",
            ),
        ),
    );
    assert!(value_error(enumerate_renditions(&mpd_path)).contains("unsafe value"));
}

#[test]
fn enumerate_renditions_rejects_path_traversal_in_media_template() {
    let (_dir, root) = tempdir();
    let mpd_path = write_raw_mpd(
        &root,
        &minimal_mpd(
            "https://cdn.example.test/title/",
            &video_set(
                "v0",
                "../../../ESCAPED_MARKER_B/work/seg_$Number$.m4s",
                "../../../ESCAPED_MARKER_B/work/init.mp4",
            ),
        ),
    );
    let message = value_error(enumerate_renditions(&mpd_path));
    assert!(
        if cfg!(windows) {
            message == "unsafe native media path"
        } else {
            message.contains("unsafe path segments")
                || message.contains("escapes the download root")
        },
        "{message}"
    );
    assert!(!root.join("ESCAPED_MARKER_B").exists());
}

#[test]
fn enumerate_renditions_rejects_authority_traversal_in_media_template() {
    // urljoin neutralizes a plain relative "../" in the URL path, not a ".." inside the
    // authority: the confinement catches this case too.
    let (_dir, root) = tempdir();
    let mpd_path = write_raw_mpd(
        &root,
        &minimal_mpd(
            "https://cdn.example.test/title/",
            &video_set(
                "v0",
                "https://../../../ESCAPED_MARKER_C/work/seg_$Number$.m4s",
                "https://../../../ESCAPED_MARKER_C/work/init.mp4",
            ),
        ),
    );
    assert!(value_error(enumerate_renditions(&mpd_path)).contains("escapes the download root"));
}

#[test]
fn load_asset_rejects_path_traversal_in_base_url_derived_asset_id() {
    let (_dir, root) = tempdir();
    let mpd_path = write_raw_mpd(
        &root,
        &minimal_mpd("https://cdn.example.test/../../../../ESCAPED_ASSET_ID/", ""),
    );
    let message = value_error(load_asset(&mpd_path, key(), &root.join("output"), None));
    assert!(message.contains("unsafe value"), "{message}");
}

#[test]
fn enumerate_renditions_rejects_doctype() {
    let (_dir, root) = tempdir();
    let xml = format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE MPD [<!ENTITY x \"lol\">]>\n{}",
        minimal_mpd("https://cdn.example.test/title/", "")
    );
    let mpd_path = write_raw_mpd(&root, &xml);
    assert!(value_error(enumerate_renditions(&mpd_path)).contains("DOCTYPE"));
}

#[test]
fn enumerate_renditions_rejects_doctype_past_a_large_leading_comment() {
    // An earlier Python version scanned only the first 4096 bytes for "<!DOCTYPE".
    let (_dir, root) = tempdir();
    let xml = format!(
        "<?xml version=\"1.0\"?>\n<!-- {} -->\n<!DOCTYPE MPD [<!ENTITY x \"lol\">]>\n{}",
        "x".repeat(8192),
        minimal_mpd("https://cdn.example.test/title/", "")
    );
    let mpd_path = write_raw_mpd(&root, &xml);
    assert!(std::fs::metadata(&mpd_path).unwrap().len() > 4096);
    assert!(value_error(enumerate_renditions(&mpd_path)).contains("DOCTYPE"));
}

#[test]
fn enumerate_renditions_excludes_out_of_scope_codec_in_mp4_container() {
    // mimeType video/mp4 alone is not the scope (: H.264/H.265): AV1-in-mp4 is excluded.
    let (_dir, root) = tempdir();
    let av1 = RepSpec::new("av1-0", "av01.0.04M.08", "video", "video/mp4");
    let asset = build_synthetic_dash_asset(&root, &[h264_rep("h264-0"), av1], TITLE);
    let renditions = enumerate_renditions(&asset.mpd_path).unwrap();
    let ids: Vec<&str> = renditions.iter().map(|r| r.rep_id.as_str()).collect();
    assert_eq!(ids, ["h264-0"]);
}

// ---- the reference cases: renditions, assets and messages as Python produced them -----------------

fn oracle() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config.oracle.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn expected(outcome: &Value) -> Result<String, String> {
    match outcome.get("ok") {
        Some(Value::String(value)) => Ok(value.clone()),
        Some(other) => Ok(other.to_string()),
        None => Err(outcome["error"].as_str().unwrap().to_string()),
    }
}

fn rendition_json(rendition: &Rendition, root: &str) -> Value {
    let shown = |path: &Path| portable_path(path).replace(root, "{root}");
    json!({
        "rep_id": rendition.rep_id,
        "content_type": rendition.content_type,
        "codec": rendition.codec,
        "timescale": rendition.timescale,
        "segment_duration_ticks": rendition.segment_duration_ticks,
        "start_number": rendition.start_number,
        "init_path": shown(&rendition.init_path),
        "segment_paths": rendition.segment_paths.iter().map(|path| shown(path)).collect::<Vec<_>>(),
        "rel_dir": portable_path(&rendition.rel_dir),
        "widevine_pssh_hex": rendition.widevine_pssh_hex,
        "width": rendition.width,
        "height": rendition.height,
        "frame_rate": rendition.frame_rate,
        "lang": rendition.lang,
    })
}

/// Python's exception class name for each error kind.
fn kind(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::Value(_) => "ValueError",
        ConfigError::Xml(_) => "ParseError",
        ConfigError::Runtime(_) => "RuntimeError",
        ConfigError::ZeroDivision => "ZeroDivisionError",
        ConfigError::Io(_) => "OSError",
    }
}

fn run_case(case: &Value, base: &Path) -> Result<Value, (String, &'static str)> {
    let name = case["name"].as_str().unwrap();
    let root = base.join(name);
    let root_text = portable_path(&root);
    let mpd_path = root
        .join(case["where"].as_str().unwrap())
        .join("manifest.mpd");
    std::fs::create_dir_all(mpd_path.parent().unwrap()).unwrap();
    std::fs::write(&mpd_path, case["mpd"].as_str().unwrap()).unwrap();
    for file in case["files"].as_array().unwrap() {
        let file = file.as_str().unwrap();
        let target = root.join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, file).unwrap();
    }
    let error = |error: ConfigError| {
        (
            error.to_string().replace(&root_text, "{root}"),
            kind(&error),
        )
    };
    if case["call"] == "load_asset" {
        let placeholder = KeyMaterial::new(VALID_KID, "0".repeat(32), "0".repeat(32)).unwrap();
        let asset =
            load_asset(&mpd_path, placeholder, &root.join("output"), None).map_err(error)?;
        let shown = |path: &Path| portable_path(path).replace(&root_text, "{root}");
        return Ok(json!({
            "asset_id": asset.asset_id,
            "mpd_path": shown(&asset.mpd_path),
            "input_base_dir": shown(&asset.input_base_dir),
            "output_dir": shown(&asset.output_dir),
            "renditions": asset.renditions.iter().map(|r| rendition_json(r, &root_text)).collect::<Vec<_>>(),
        }));
    }
    let renditions = enumerate_renditions(&mpd_path).map_err(error)?;
    Ok(Value::Array(
        renditions
            .iter()
            .map(|r| rendition_json(r, &root_text))
            .collect(),
    ))
}

#[test]
fn enumerate_renditions_and_load_asset_match_python() {
    let (_dir, base) = tempdir();
    let oracle = oracle();
    for case in oracle["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        match (run_case(case, &base), case.get("ok")) {
            (Ok(actual), Some(expected)) => assert_eq!(&actual, expected, "{name}"),
            (Err((message, kind)), None) => {
                let python_kind = case["type"].as_str().unwrap();
                // binascii.Error is a ValueError.
                let python_kind = if python_kind == "Error" {
                    "ValueError"
                } else {
                    python_kind
                };
                assert_eq!(kind, python_kind, "{name}");
                let expected = if cfg!(windows)
                    && matches!(
                        name,
                        "media-without-directory"
                            | "absolute-media-directory"
                            | "traversal-media-template"
                    ) {
                    "unsafe native media path"
                } else {
                    case["error"].as_str().unwrap()
                };
                assert_eq!(message, expected, "{name}");
            }
            (outcome, _) => panic!("{name}: {outcome:?} against {case}"),
        }
    }
}
