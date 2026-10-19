//! Shaka Packager arguments, secret scrubbing, segment numbering, and failure handling.

#[path = "common/mod.rs"]
mod private_fixture;

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

use drmr_core::config::KeyMaterial;
use drmr_core::events::{Collector, NoSink, Record};
use drmr_core::packager::{
    Error, ISOLATING_SEGMENT_DURATION_S, ISOLATION_VIOLATION_RC, Packager, PackagerError,
    PackagerResult, STDERR_TAIL_CHARS, format_duration, merge_segments, renumber_to_source_origin,
    scrub,
};
use drmr_core::toolrun::{REDACTED, Secrets, ToolRunner};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VALID_KID: &str = "000102030405060708090a0b0c0d0e0f";
const VALID_KEY: &str = "10101010101010101010101010101010";
const VALID_IV: &str = "20202020202020202020202020202020";

fn key() -> KeyMaterial {
    KeyMaterial::new(VALID_KID, VALID_KEY, VALID_IV).unwrap()
}

/// Every spelling of the two secrets a scrub must catch. The KID is public: not here.
fn key_material_variants() -> Vec<String> {
    [VALID_KEY, VALID_IV]
        .iter()
        .flat_map(|secret| [secret.to_string(), secret.to_uppercase()])
        .collect()
}

fn assert_no_key_material(text: &str) {
    for variant in key_material_variants() {
        assert!(!text.contains(&variant), "key material in: {text}");
    }
}

fn tempdir() -> (private_fixture::PrivateTempDir, PathBuf) {
    let dir = private_fixture::private_tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

// ---- the stub packager ------------------------------------------------------------------------

/// A native fixture with per-invocation configuration; no process environment is changed.
fn stub_at(path: &Path, env: &[(&str, &str)]) -> String {
    let mut spec = drmr_test_support::ToolSpec {
        mode: drmr_test_support::Mode::Packager,
        ..Default::default()
    };
    for (name, value) in env {
        spec = spec.with(*name, *value);
    }
    drmr_test_support::install(path, &spec)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A stub wrapper in its own directory under `root`.
fn stub(root: &Path, env: &[(&str, &str)]) -> String {
    stub_at(&root.join("bin/packager-stub"), env)
}

fn runner() -> (Arc<ToolRunner>, Arc<Collector>) {
    let collector = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), collector.clone()));
    (runner, collector)
}

fn packager(bin: &str) -> (Packager, Arc<Collector>) {
    let (runner, collector) = runner();
    (Packager::new(runner).with_packager_bin(bin), collector)
}

fn recorded(argv_file: &Path) -> Vec<String> {
    std::fs::read_to_string(argv_file)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

fn encrypt(packager: &Packager, root: &Path, pssh: &[&str]) -> Result<PackagerResult, Error> {
    packager.encrypt_cbcs(
        &root.join("clear.mp4"),
        &root.join("cbcs/init.mp4"),
        &root.join("cbcs/seg_$Number$.m4s").to_string_lossy(),
        &key(),
        6.0,
        pssh,
    )
}

fn decrypt(packager: &Packager, root: &Path) -> Result<PackagerResult, Error> {
    packager.decrypt_cenc(&root.join("encrypted.mp4"), &root.join("clear.mp4"), &key())
}

fn after<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let index = argv.iter().position(|token| token == flag).unwrap();
    &argv[index + 1]
}

// ---- merge_segments:  /  ------------------------------------------------

#[test]
fn merge_segments_is_byte_exact_init_then_segments() {
    let (_dir, root) = tempdir();
    let init = root.join("init.mp4");
    std::fs::write(&init, b"INIT").unwrap();
    let segments: Vec<PathBuf> = [&b"AAA"[..], b"BB", b"CCCC"]
        .iter()
        .enumerate()
        .map(|(index, data)| {
            let path = root.join(format!("seg_{index}.m4s"));
            std::fs::write(&path, data).unwrap();
            path
        })
        .collect();
    let out = root.join("merged/encrypted.mp4"); // the nested parent is created
    let result = merge_segments(&init, &segments, &out).unwrap();
    assert_eq!(portable_path(&result), portable_path(&out));
    assert_eq!(std::fs::read(&out).unwrap(), b"INITAAABBCCCC");
    assert_eq!(std::fs::metadata(&out).unwrap().len(), 4 + 3 + 2 + 4);
}

#[test]
fn merge_segments_orders_numerically_not_lexically() {
    let (_dir, root) = tempdir();
    let init = root.join("init.mp4");
    std::fs::write(&init, b"I").unwrap();
    let (seg2, seg10) = (root.join("seg_2.m4s"), root.join("seg_10.m4s"));
    std::fs::write(&seg2, b"S2|").unwrap();
    std::fs::write(&seg10, b"S10|").unwrap();
    let out = root.join("m.mp4");
    merge_segments(&init, &[seg10, seg2], &out).unwrap(); // wrong order in, numeric out
    assert_eq!(std::fs::read(&out).unwrap(), b"IS2|S10|");
}

#[test]
fn merge_segments_sorts_shuffled_input_by_segment_number() {
    let (_dir, root) = tempdir();
    let init = root.join("init.mp4");
    std::fs::write(&init, b"INIT").unwrap();
    let data: BTreeMap<u32, &[u8]> = [
        (1, &b"a"[..]),
        (2, b"bb"),
        (3, b"ccc"),
        (5, b"eeee"),
        (10, b"fffff"),
    ]
    .into();
    let segments: Vec<PathBuf> = [10, 3, 1, 5, 2]
        .iter()
        .map(|number| {
            let path = root.join(format!("seg_{number}.m4s"));
            std::fs::write(&path, data[number]).unwrap();
            path
        })
        .collect();
    let out = root.join("m.mp4");
    merge_segments(&init, &segments, &out).unwrap();
    let mut expected = b"INIT".to_vec();
    for number in [1, 2, 3, 5, 10] {
        expected.extend(data[&number]);
    }
    assert_eq!(std::fs::read(&out).unwrap(), expected);
}

fn snapshot(paths: &[&Path]) -> Vec<(Vec<u8>, std::time::SystemTime)> {
    paths
        .iter()
        .map(|path| {
            (
                std::fs::read(path).unwrap(),
                std::fs::metadata(path).unwrap().modified().unwrap(),
            )
        })
        .collect()
}

#[test]
fn merge_segments_leaves_inputs_byte_identical() {
    // HARD INVARIANT: inputs are read-only.
    let (_dir, root) = tempdir();
    let (init, segment) = (root.join("init.mp4"), root.join("seg_0.m4s"));
    std::fs::write(&init, b"INIT").unwrap();
    std::fs::write(&segment, b"SEG").unwrap();
    let before = snapshot(&[&init, &segment]);
    merge_segments(&init, std::slice::from_ref(&segment), &root.join("out.mp4")).unwrap();
    assert_eq!(snapshot(&[&init, &segment]), before);
}

#[test]
fn merge_segments_with_no_segments_is_just_init() {
    let (_dir, root) = tempdir();
    let init = root.join("init.mp4");
    std::fs::write(&init, b"INIT-ONLY").unwrap();
    let out = root.join("out.mp4");
    merge_segments(&init, &[], &out).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"INIT-ONLY");
}

#[test]
fn merge_segments_rejects_a_segment_without_a_number() {
    let (_dir, root) = tempdir();
    let init = root.join("init.mp4");
    std::fs::write(&init, b"I").unwrap();
    let error =
        merge_segments(&init, &[root.join("segment.m4s")], &root.join("o.mp4")).unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "segment file {} has no numeric index",
            portable_path(&root.join("segment.m4s"))
        )
    );
}

#[test]
fn merge_segments_reads_paths_as_pathlib_spells_them() {
    // `Path("init.mp4/")` is `init.mp4`: a trailing slash or a `.` part names the same file.
    let (_dir, root) = tempdir();
    std::fs::write(root.join("init.mp4"), b"I").unwrap();
    std::fs::write(root.join("seg_0.m4s"), b"S").unwrap();
    let init = PathBuf::from(format!("{}/init.mp4/", root.display()));
    let segment = PathBuf::from(format!("{}/./seg_0.m4s/", root.display()));
    let out = merge_segments(&init, &[segment], &root.join("out.mp4")).unwrap();
    assert_eq!(std::fs::read(out).unwrap(), b"IS");
}

// ---- decrypt_cenc argv shape -------------------------------------------------------------------

#[test]
fn decrypt_cenc_argv_shape() {
    let (_dir, root) = tempdir();
    let argv_file = root.join("recorded_argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    decrypt(&packager, &root).unwrap();
    let argv = recorded(&argv_file);
    assert!(argv.contains(&"--single_threaded".to_string()));
    assert!(argv.contains(&"--io_cache_size=0".to_string()));
    assert!(argv.iter().any(|token| token.starts_with("input=")
        && token.contains("stream=0")
        && token.contains("output=")));
    assert!(argv.contains(&"--enable_raw_key_decryption".to_string()));
    assert_eq!(
        after(&argv, "--keys"),
        format!("label=:key_id={VALID_KID}:key={VALID_KEY}")
    );
    // CENC -> clear: no encryption, scheme or IV flags.
    for flag in [
        "--enable_raw_key_encryption",
        "--protection_scheme",
        "--iv",
        "--protection_systems",
    ] {
        assert!(!argv.contains(&flag.to_string()), "{flag}");
    }
}

// ---- encrypt_cbcs: the crypto surface -----------------------------------------------------------

#[test]
fn encrypt_cbcs_emits_source_derived_segment_duration() {
    // The duration is a required argument (the signature is checked by the
    // compile_fail example on `Packager::encrypt_cbcs`); 6 s from the source, never 4.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    let argv = recorded(&argv_file);
    assert_eq!(
        after(&argv, "--segment_duration")
            .parse::<f64>()
            .unwrap()
            .to_bits(),
        6.0_f64.to_bits()
    );
    assert!(!argv.contains(&"4".to_string()));
}

#[test]
fn encrypt_cbcs_emits_clear_lead_zero() {
    // Shaka's --clear_lead defaults to 5 s (< the 6 s segment): without 0 the whole first
    // segment of every rendition would stay in the clear.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    assert_eq!(after(&recorded(&argv_file), "--clear_lead"), "0");
}

#[test]
fn encrypt_cbcs_forwards_iv() {
    // : the explicit --iv is the configured 16-byte IV.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    assert_eq!(after(&recorded(&argv_file), "--iv"), VALID_IV);
}

#[test]
fn encrypt_cbcs_argv_is_deterministic_across_runs() {
    // At the argument layer: a fixed --iv makes two runs identical.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    let first = recorded(&argv_file);
    encrypt(&packager, &root, &[]).unwrap();
    assert_eq!(recorded(&argv_file), first);
}

#[test]
fn encrypt_cbcs_forwards_kid_and_uses_cbcs_scheme() {
    // : the KID unchanged; the scheme is cbcs, not cenc.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    let argv = recorded(&argv_file);
    assert!(after(&argv, "--keys").contains(&format!("key_id={VALID_KID}")));
    assert_eq!(after(&argv, "--protection_scheme"), "cbcs");
    assert!(!argv.contains(&"cenc".to_string()));
}

#[test]
fn encrypt_cbcs_forwards_pssh_concatenated() {
    // : the caller's boxes concatenated verbatim into one --pssh value;
    // --protection_systems never.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &["deadbeef", "cafe0001"]).unwrap();
    let argv = recorded(&argv_file);
    assert_eq!(after(&argv, "--pssh"), "deadbeefcafe0001");
    assert!(!argv.contains(&"--protection_systems".to_string()));
}

#[test]
fn encrypt_cbcs_omits_pssh_when_none_or_empty() {
    // Python's None and [] are both the empty slice here; [""] still emits an empty --pssh.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &[]).unwrap();
    let argv = recorded(&argv_file);
    assert!(!argv.contains(&"--pssh".to_string()));
    assert!(!argv.contains(&"--protection_systems".to_string()));
    encrypt(&packager, &root, &[""]).unwrap();
    assert_eq!(after(&recorded(&argv_file), "--pssh"), "");
}

#[test]
fn encrypt_cbcs_never_emits_protection_systems() {
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    encrypt(&packager, &root, &["dead"]).unwrap();
    assert!(!recorded(&argv_file).contains(&"--protection_systems".to_string()));
}

#[test]
fn media_arguments_match_the_reference_with_explicit_resource_limits() {
    // Every builder against the argv Python built for the same paths: path normalization
    // (`str(Path(...))` on the descriptors, the segment template as given), the duration text,
    // the pssh concatenation, the redaction positions.
    let (_dir, root) = tempdir();
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packager.oracle.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root_text = portable_path(&root);
    let argv_file = root.join("argv.txt");
    let env = [("STUB_PACKAGER_ARGV_FILE", argv_file.to_str().unwrap())];
    let cases: BTreeMap<String, Value> = oracle["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| (case[0].as_str().unwrap().to_string(), case[1].clone()))
        .collect();
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|token| token.as_str().unwrap().replace("{root}", &root_text))
            .collect()
    };
    let check = |name: &str, bin: &str, result: PackagerResult| {
        let (mut real, mut redacted) = (
            strings(&cases[name]["real"]),
            strings(&cases[name]["redacted"]),
        );
        for arguments in [&mut real, &mut redacted] {
            arguments.extend(["--single_threaded".into(), "--io_cache_size=0".into()]);
        }
        // Actual child arguments stay byte-for-byte equal to the reference;
        // retained diagnostics now hide the whole opaque signaling payload.
        if let Some(at) = redacted.iter().position(|argument| argument == "--pssh") {
            redacted[at + 1] = REDACTED.into();
        }
        assert_eq!(recorded(&argv_file), real[1..], "{name}: the child's argv");
        assert_eq!(result.argv_redacted[0], bin, "{name}");
        assert_eq!(
            result.argv_redacted[1..],
            redacted[1..],
            "{name}: the redacted argv"
        );
    };
    let bin = stub(&root, &env);
    let (packager, _) = packager(&bin);
    let result = packager
        .decrypt_cenc(
            &PathBuf::from(format!("{root_text}/in//enc.mp4")),
            &PathBuf::from(format!("{root_text}/out/./clear.mp4")),
            &key(),
        )
        .unwrap();
    check("decrypt", &bin, result);
    // A str packager_bin is passed as written (no path normalization).
    stub_at(&root.join("bin/packager"), &env);
    let as_written = format!("{root_text}/bin//packager");
    let (packager_as_written, _) = self::packager(&as_written);
    let result = packager_as_written
        .decrypt_cenc(&root.join("enc.mp4"), &root.join("clear.mp4"), &key())
        .unwrap();
    assert_eq!(strings(&cases["decrypt-bin"]["redacted"])[0], as_written);
    check("decrypt-bin", &as_written, result);
    let template = format!("{root_text}/cbcs//seg_$Number$.m4s");
    for (name, duration, pssh) in [
        ("encrypt-6", 6.0, &["dead", "beef"][..]),
        ("encrypt-none", 6.0, &[][..]),
        ("encrypt-empty", 6.0, &[][..]),
        ("encrypt-blank", 6.0, &[""][..]),
        ("encrypt-6006", 6.006, &["dead"][..]),
        ("encrypt-iso", 1_000_000.0, &["dead"][..]),
    ] {
        let result = packager
            .encrypt_cbcs(
                &root.join("clear.mp4"),
                &PathBuf::from(format!("{root_text}/cbcs/./init.mp4")),
                &template,
                &key(),
                duration,
                pssh,
            )
            .unwrap();
        check(name, &bin, result);
    }
}

// ---- renumber_to_source_origin:  ---------------------------------------------------

fn names_in(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn renumber_maps_shaka_1_origin_to_source_0_origin() {
    // Shaka writes seg_1..seg_5 ($Number$ from 1); the source is zero-based. Preserve
    // changing the URL structure, so the OUTPUT is renamed back to the source origin.
    let (_dir, root) = tempdir();
    let staging = root.join("staging");
    std::fs::create_dir(&staging).unwrap();
    for number in 1..=5 {
        std::fs::write(
            staging.join(format!("seg_{number}.m4s")),
            format!("S{number}"),
        )
        .unwrap();
    }
    let mapping = renumber_to_source_origin(&staging, 0).unwrap();
    assert_eq!(
        mapping,
        BTreeMap::from([(1, 0), (2, 1), (3, 2), (4, 3), (5, 4)])
    );
    let expected: Vec<String> = (0..5).map(|i| format!("seg_{i}.m4s")).collect();
    assert_eq!(names_in(&staging), expected);
    assert_eq!(std::fs::read(staging.join("seg_0.m4s")).unwrap(), b"S1");
    assert_eq!(std::fs::read(staging.join("seg_4.m4s")).unwrap(), b"S5");
}

#[test]
fn renumber_output_count_equals_source_count() {
    let (_dir, root) = tempdir();
    for number in 1..=3 {
        std::fs::write(root.join(format!("seg_{number}.m4s")), b"x").unwrap();
    }
    let mapping = renumber_to_source_origin(&root, 0).unwrap();
    assert_eq!(mapping.len(), 3);
    assert_eq!(names_in(&root).len(), 3);
}

#[test]
fn renumber_is_identity_when_source_is_1_origin() {
    let (_dir, root) = tempdir();
    for number in 1..=3 {
        std::fs::write(root.join(format!("seg_{number}.m4s")), number.to_string()).unwrap();
    }
    let mapping = renumber_to_source_origin(&root, 1).unwrap();
    assert_eq!(mapping, BTreeMap::from([(1, 1), (2, 2), (3, 3)]));
    assert_eq!(std::fs::read(root.join("seg_1.m4s")).unwrap(), b"1");
}

#[test]
fn renumber_rejects_noncontiguous_staging() {
    let (_dir, root) = tempdir();
    std::fs::write(root.join("seg_1.m4s"), b"x").unwrap();
    std::fs::write(root.join("seg_3.m4s"), b"x").unwrap(); // a gap: Shaka never does this
    assert!(matches!(
        renumber_to_source_origin(&root, 0),
        Err(Error::Value(_))
    ));
}

#[test]
fn renumber_matches_python_on_every_case() {
    // The error text is what the reimplementation's `6-1.number-origin` FAIL detail carries.
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packager.oracle.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for case in oracle["renumber"].as_array().unwrap() {
        let (_dir, root) = tempdir();
        for name in case["names"].as_array().unwrap() {
            let name = name.as_str().unwrap();
            match name.strip_prefix("dir:") {
                Some(directory) => std::fs::create_dir(root.join(directory)).unwrap(),
                None => std::fs::write(root.join(name), name).unwrap(),
            }
        }
        let start = case["start"].as_i64().unwrap();
        match (renumber_to_source_origin(&root, start), case.get("ok")) {
            (Ok(mapping), Some(expected)) => {
                let expected: BTreeMap<i64, i64> = expected
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(from, to)| (from.parse().unwrap(), to.as_i64().unwrap()))
                    .collect();
                assert_eq!(mapping, expected, "{case}");
            }
            (Err(Error::Value(message)), None) => {
                assert_eq!(message, case["error"].as_str().unwrap(), "{case}");
            }
            (outcome, _) => panic!("{outcome:?} against {case}"),
        }
        let mut after: Vec<(String, String)> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let content = if path.is_file() {
                    std::fs::read_to_string(&path).unwrap()
                } else {
                    "<dir>".to_string()
                };
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    content,
                )
            })
            .collect();
        after.sort();
        let expected: Vec<(String, String)> = case["after"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_string(),
                    pair[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(after, expected, "{case}");
    }
}

// ---- : secret hygiene ---------------------------------------------------------------------

#[test]
fn argv_redacted_hides_key_and_iv() {
    for mode in ["decrypt", "encrypt"] {
        let (_dir, root) = tempdir();
        let (packager, _) = packager(&stub(&root, &[]));
        let result = if mode == "decrypt" {
            decrypt(&packager, &root)
        } else {
            encrypt(&packager, &root, &["dead"])
        }
        .unwrap();
        let joined = result.argv_redacted.join(" ");
        assert!(
            !joined.contains(VALID_KEY) && !joined.contains(VALID_IV),
            "{mode}"
        );
        assert!(joined.contains(REDACTED), "{mode}");
        assert!(
            joined.contains(VALID_KID),
            "{mode}: the KID is public, kept visible"
        );
        assert!(result.duration_s >= 0.0);
    }
}

#[test]
fn stderr_tail_is_scrubbed_of_key_and_iv() {
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_STDERR", "echo_argv")]));
    let result = encrypt(&packager, &root, &[]).unwrap();
    assert!(!result.stderr_tail.contains(VALID_KEY));
    assert!(!result.stderr_tail.contains(VALID_IV));
    assert!(result.stderr_tail.contains("args were:"));
}

#[test]
fn nonzero_returncode_is_returned_not_raised_and_scrubbed() {
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_EXIT", "3"),
            ("STUB_PACKAGER_STDERR", "echo_argv"),
        ],
    ));
    let result = decrypt(&packager, &root).unwrap(); // must not raise on a non-zero exit
    assert_eq!(result.returncode, 3);
    assert!(!result.stderr_tail.contains(VALID_KEY));
    assert!(!result.stderr_tail.contains(VALID_IV));
}

#[test]
fn stderr_tail_is_the_last_4000_characters_after_the_scrub() {
    let (_dir, root) = tempdir();
    // A secret straddling the tail boundary must not survive: the scrub runs first.
    let noise = format!("{}{VALID_KEY}{}END", "x".repeat(5000), "y".repeat(3990));
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_STDERR", &noise)]));
    let result = decrypt(&packager, &root).unwrap();
    let scrubbed = format!("{}{REDACTED}{}END\n", "x".repeat(5000), "y".repeat(3990));
    let expected: String = scrubbed
        .chars()
        .skip(scrubbed.chars().count() - STDERR_TAIL_CHARS)
        .collect();
    assert_eq!(STDERR_TAIL_CHARS, 4000);
    assert_eq!(result.stderr_tail, expected);
    assert_eq!(result.stderr_tail.chars().count(), 4000);
}

fn assert_secret_free_error(error: &PackagerError) {
    let shown = format!("{error} {error:?} {}", error.argv_redacted().join(" "));
    assert_no_key_material(&shown);
    assert!(
        std::error::Error::source(error).is_none(),
        "no chained cause"
    );
}

#[test]
fn timeout_raises_error_with_no_secret_in_message_or_debug() {
    // (b): the timeout is a scrubbed PackagerError with no cause behind it.
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_HANG", "1")]));
    let packager = packager.with_timeout(Some(0.5));
    let start = Instant::now();
    let Err(Error::Packager(error)) = encrypt(&packager, &root, &[]) else {
        panic!("expected a PackagerError");
    };
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(error.to_string(), "Shaka Packager timed out after 0.5s");
    assert_secret_free_error(&error);
}

#[test]
fn spawn_failure_raises_scrubbed_error() {
    // (a): a missing binary leaks no key or IV through the error.
    let (_dir, root) = tempdir();
    let missing = root.join("nonexistent-packager");
    let (packager, _) = packager(&missing.to_string_lossy());
    let Err(Error::Packager(error)) = decrypt(&packager, &root) else {
        panic!("expected a PackagerError");
    };
    assert_eq!(error.to_string(), "could not run Shaka Packager");
    assert_secret_free_error(&error);
    assert!(
        error
            .argv_redacted()
            .iter()
            .any(|token| token.starts_with("label=:key_id="))
    );
}

#[test]
fn argv_reaches_the_child_verbatim_without_a_shell() {
    // (c): the argv is a list handed to the tool, never to a shell -- shell syntax in
    // a path stays literal text.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let (packager, _) = packager(&stub(
        &root,
        &[("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy())],
    ));
    let odd = root.join("in $(touch pwned) `id`; x.mp4");
    packager
        .decrypt_cenc(&odd, &root.join("clear.mp4"), &key())
        .unwrap();
    assert!(recorded(&argv_file)[0].contains("$(touch pwned) `id`; x.mp4"));
    assert!(!root.join("pwned").exists() && !Path::new("pwned").exists());
}

// ---- transcrypt_cbcs_per_segment:, the boundary-preserving path ---------------------

fn make_source(root: &Path, count: usize) -> (PathBuf, Vec<PathBuf>) {
    let source = root.join("source");
    std::fs::create_dir_all(&source).unwrap();
    let init = source.join("init.mp4");
    std::fs::write(&init, b"SRCINIT").unwrap();
    let segments = (0..count)
        .map(|index| {
            let path = source.join(format!("seg_{index}.m4s"));
            std::fs::write(&path, format!("SRCSEG{index}")).unwrap();
            path
        })
        .collect();
    (init, segments)
}

fn transcrypt(
    packager: &Packager,
    root: &Path,
    init: &Path,
    segments: &[PathBuf],
    pssh: &[&str],
) -> Result<PackagerResult, Error> {
    packager.transcrypt_cbcs_per_segment(
        init,
        segments,
        &root.join("cbcs/init.mp4"),
        &root.join("cbcs/seg_$Number$.m4s").to_string_lossy(),
        &key(),
        pssh,
        Some(&root.join("work")),
    )
}

fn calls(argv_dir: &Path) -> Vec<Vec<String>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(argv_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    files.iter().map(|file| recorded(file)).collect()
}

#[test]
fn transcrypt_produces_one_origin_staging_series_and_single_init() {
    // Source segment i -> staging seg_{i+1} (Shaka's 1-origin, as encrypt_cbcs) + ONE init,
    // so renumber_to_source_origin reconciles the output with no change.
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_SEGMENT_COUNT", "1")]));
    let (init, segments) = make_source(&root, 4);
    let result = transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    assert_eq!(result.returncode, 0);
    let cbcs = root.join("cbcs");
    let series: Vec<String> = names_in(&cbcs)
        .into_iter()
        .filter(|name| name.starts_with("seg_"))
        .collect();
    assert_eq!(series, ["seg_1.m4s", "seg_2.m4s", "seg_3.m4s", "seg_4.m4s"]);
    assert!(cbcs.join("init.mp4").exists());
    let mapping = renumber_to_source_origin(&cbcs, 0).unwrap();
    assert_eq!(mapping, BTreeMap::from([(1, 0), (2, 1), (3, 2), (4, 3)]));
}

#[test]
fn transcrypt_runs_decrypt_then_encrypt_per_segment() {
    let (_dir, root) = tempdir();
    let argv_dir = root.join("argv");
    let (packager, _) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "1"),
            ("STUB_PACKAGER_ARGV_DIR", &argv_dir.to_string_lossy()),
        ],
    ));
    let (init, segments) = make_source(&root, 3);
    transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    let all = calls(&argv_dir);
    let decrypts = all
        .iter()
        .filter(|argv| argv.contains(&"--enable_raw_key_decryption".to_string()))
        .count();
    let encrypts = all
        .iter()
        .filter(|argv| argv.contains(&"--enable_raw_key_encryption".to_string()))
        .count();
    assert_eq!(
        (decrypts, encrypts),
        (3, 3),
        "one decrypt + one encrypt per segment"
    );
}

#[test]
fn transcrypt_encrypt_uses_isolating_duration_not_six_or_four() {
    // The isolating --segment_duration (>> any asset) forces Shaka's floor grid to 0, so it
    // emits ONE segment == the source boundary. Never 6 (re-decided boundaries), never 4.
    let (_dir, root) = tempdir();
    let argv_dir = root.join("argv");
    let (packager, _) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "1"),
            ("STUB_PACKAGER_ARGV_DIR", &argv_dir.to_string_lossy()),
        ],
    ));
    let (init, segments) = make_source(&root, 2);
    transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    let encrypts: Vec<Vec<String>> = calls(&argv_dir)
        .into_iter()
        .filter(|argv| argv.contains(&"--enable_raw_key_encryption".to_string()))
        .collect();
    assert_ne!(encrypts.len(), 0);
    for argv in &encrypts {
        let duration = after(argv, "--segment_duration");
        assert_eq!(
            duration.parse::<f64>().unwrap().to_bits(),
            ISOLATING_SEGMENT_DURATION_S.to_bits()
        );
        assert!(!["6", "6.0", "4", "4.0"].contains(&duration));
        assert_eq!(after(argv, "--protection_scheme"), "cbcs");
        assert_eq!(after(argv, "--clear_lead"), "0");
        assert_eq!(after(argv, "--iv"), VALID_IV);
    }
}

#[test]
fn transcrypt_forwards_pssh_to_each_segment() {
    let (_dir, root) = tempdir();
    let argv_dir = root.join("argv");
    let (packager, _) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "1"),
            ("STUB_PACKAGER_ARGV_DIR", &argv_dir.to_string_lossy()),
        ],
    ));
    let (init, segments) = make_source(&root, 2);
    transcrypt(
        &packager,
        &root,
        &init,
        &segments,
        &["deadbeef", "cafe0001"],
    )
    .unwrap();
    let encrypts: Vec<Vec<String>> = calls(&argv_dir)
        .into_iter()
        .filter(|argv| argv.contains(&"--enable_raw_key_encryption".to_string()))
        .collect();
    assert_ne!(encrypts.len(), 0);
    for argv in &encrypts {
        assert_eq!(after(argv, "--pssh"), "deadbeefcafe0001");
        assert!(!argv.contains(&"--protection_systems".to_string()));
    }
}

#[test]
fn transcrypt_result_hides_key_and_iv() {
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_SEGMENT_COUNT", "1")]));
    let (init, segments) = make_source(&root, 2);
    let result = transcrypt(&packager, &root, &init, &segments, &["dead"]).unwrap();
    let joined = result.argv_redacted.join(" ");
    assert!(!joined.contains(VALID_KEY) && !joined.contains(VALID_IV));
    assert!(joined.contains(REDACTED));
}

#[test]
fn transcrypt_returns_nonzero_on_subprocess_failure_scrubbed() {
    // A per-segment non-zero exit is RETURNED (not raised) and scrubbed.
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "1"),
            ("STUB_PACKAGER_EXIT", "5"),
            ("STUB_PACKAGER_STDERR", "echo_argv"),
        ],
    ));
    let (init, segments) = make_source(&root, 3);
    let result = transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    assert_ne!(result.returncode, 0);
    assert!(!result.stderr_tail.contains(VALID_KEY) && !result.stderr_tail.contains(VALID_IV));
}

#[test]
fn transcrypt_refuses_multi_segment_output_from_one_source_segment() {
    // If the isolating duration ever fails to isolate, the source boundary would be lost: the
    // call refuses with a scrubbed non-zero result instead of keeping one and dropping data.
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_SEGMENT_COUNT", "2")]));
    let (init, segments) = make_source(&root, 2);
    let result = transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    assert_eq!(result.returncode, ISOLATION_VIOLATION_RC);
    assert_eq!(ISOLATION_VIOLATION_RC, 251);
    assert_eq!(
        result.stderr_tail,
        "per-segment isolation produced 2 segments for source segment index 0 (expected exactly 1)"
    );
    assert!(!result.stderr_tail.contains(VALID_KEY) && !result.stderr_tail.contains(VALID_IV));
}

#[test]
fn transcrypt_leaves_source_inputs_byte_identical() {
    // HARD INVARIANT: the source init and segments are read-only.
    let (_dir, root) = tempdir();
    let (packager, _) = packager(&stub(&root, &[("STUB_PACKAGER_SEGMENT_COUNT", "1")]));
    let (init, segments) = make_source(&root, 3);
    let paths: Vec<&Path> = std::iter::once(init.as_path())
        .chain(segments.iter().map(PathBuf::as_path))
        .collect();
    let before = snapshot(&paths);
    transcrypt(&packager, &root, &init, &segments, &[]).unwrap();
    assert_eq!(snapshot(&paths), before);
}

#[test]
fn transcrypt_with_no_segments_is_an_empty_success() {
    let (_dir, root) = tempdir();
    let (packager, collector) = packager(&root.join("never-run").to_string_lossy());
    let (init, _) = make_source(&root, 0);
    let result = transcrypt(&packager, &root, &init, &[], &[]).unwrap();
    assert_eq!(
        (
            result.returncode,
            result.argv_redacted.len(),
            result.duration_s,
            result.stderr_tail.as_str()
        ),
        (0, 0, 0.0, "")
    );
    assert_eq!(collector.records().len(), 0);
}

/// Runs in a child process whose working directory the parent chose (a test cannot change its
/// own safely): the per-segment path with relative paths.
#[test]
#[ignore = "run by transcrypt_relative_paths_are_spelled_as_pathlib_spells_them in a child"]
fn child_transcrypts_with_relative_paths() {
    let bin = std::env::var("DRMR_TEST_PACKAGER_STUB").unwrap();
    let (packager, _) = packager(&bin);
    let init = Path::new("source/init.mp4");
    let segments = [PathBuf::from("source/seg_0.m4s")];
    let expected_tail =
        "per-segment isolation produced 0 segments for source segment index 0 (expected exactly 1)";
    let bare = packager
        .transcrypt_cbcs_per_segment(
            init,
            &segments,
            Path::new("init.mp4"),
            "seg_$Number$.m4s",
            &key(),
            &[],
            None,
        )
        .unwrap();
    assert_eq!(
        (bare.returncode, bare.stderr_tail.as_str()),
        (ISOLATION_VIOLATION_RC, expected_tail)
    );
    let dot = packager
        .transcrypt_cbcs_per_segment(
            init,
            &segments,
            Path::new("out/init.mp4"),
            "out/seg_$Number$.m4s",
            &key(),
            &[],
            Some(Path::new(".")),
        )
        .unwrap();
    assert_eq!(dot.returncode, ISOLATION_VIOLATION_RC);
}

#[test]
fn transcrypt_relative_paths_are_spelled_as_pathlib_spells_them() {
    // `Path(".") / "x"` is `x`: the work directory under a bare init name is
    // `_persegment_work`, and `work_dir="."` puts the segment directories at `seg_<i>` — the
    // argv Python builds (no `./` prefix).
    let (_dir, root) = tempdir();
    // The child uses its current directory as a media workspace. Prepare its
    // inheritable owner-only policy before native helpers or inputs are created.
    #[cfg(windows)]
    drmr_core::native::create_media_dir_all(&root).unwrap();
    std::fs::create_dir(root.join("source")).unwrap();
    std::fs::write(root.join("source/init.mp4"), b"I").unwrap();
    std::fs::write(root.join("source/seg_0.m4s"), b"S").unwrap();
    let argv_dir = root.join("argv");
    let bin = stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "0"),
            ("STUB_PACKAGER_ARGV_DIR", &argv_dir.to_string_lossy()),
        ],
    );
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "child_transcrypts_with_relative_paths",
            "--quiet",
        ])
        .current_dir(&root)
        .env("DRMR_TEST_PACKAGER_STUB", &bin)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "relative-path child failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packager.oracle.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut expected: Vec<Vec<String>> =
        serde_json::from_value(oracle["transcrypt_relative"]["calls"].clone()).unwrap();
    for argv in &mut expected {
        argv.extend(["--single_threaded".into(), "--io_cache_size=0".into()]);
    }
    let redacted: Vec<Vec<String>> = calls(&argv_dir)
        .into_iter()
        .map(|argv| {
            argv.into_iter()
                .map(|token| {
                    token
                        .replace(VALID_KEY, REDACTED)
                        .replace(VALID_IV, REDACTED)
                })
                .collect()
        })
        .collect();
    assert_eq!(redacted, expected);
}

// ---- : full tool logs for the operator, under  ------------------------------------

fn events(records: &[Record]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["event"].as_str().unwrap())
        .collect()
}

#[test]
fn each_call_emits_one_redacted_command_and_one_tool_output_record() {
    // The stub echoes its REAL command line (key + IV) to stderr: the operator still sees
    // what Shaka printed, with only the secrets scrubbed out.
    for mode in ["decrypt", "encrypt"] {
        let (_dir, root) = tempdir();
        let (packager, collector) =
            packager(&stub(&root, &[("STUB_PACKAGER_STDERR", "echo_argv")]));
        let result = if mode == "decrypt" {
            decrypt(&packager, &root)
        } else {
            encrypt(&packager, &root, &["dead"])
        }
        .unwrap();
        let records = collector.records();
        assert_eq!(events(&records), ["command", "tool-output"], "{mode}");
        let (command, output) = (&records[0], &records[1]);
        assert_eq!(command["tool"], "packager");
        assert_eq!(output["tool"], "packager");
        let argv: Vec<String> = serde_json::from_value(command["argv"].clone()).unwrap();
        assert_eq!(
            argv, result.argv_redacted,
            "{mode}: exactly the redacted argv"
        );
        assert!(argv.contains(&format!("label=:key_id={VALID_KID}:key={REDACTED}")));
        if mode == "encrypt" {
            assert_eq!(after(&argv, "--iv"), REDACTED);
        }
        assert_eq!(output["returncode"], 0);
        assert_eq!(result.returncode, 0);
        let stderr = output["stderr"].as_str().unwrap();
        assert!(
            stderr.contains("args were:") && stderr.contains(VALID_KID),
            "{mode}"
        );
        assert_no_key_material(&serde_json::to_string(&records).unwrap());
    }
}

#[test]
fn records_never_carry_key_or_iv_even_when_the_tool_prints_them() {
    // A failing run whose output carries both secrets in lower AND upper case: the non-zero
    // exit is returned (not raised) and nothing reaches the log unscrubbed.
    let leaked = format!("leaked: {}", key_material_variants().join(" "));
    for mode in ["decrypt", "encrypt"] {
        let (_dir, root) = tempdir();
        let (packager, collector) = packager(&stub(
            &root,
            &[
                ("STUB_PACKAGER_STDERR", &leaked),
                ("STUB_PACKAGER_EXIT", "3"),
            ],
        ));
        let result = if mode == "decrypt" {
            decrypt(&packager, &root)
        } else {
            encrypt(&packager, &root, &["dead"])
        }
        .unwrap();
        assert_eq!(result.returncode, 3);
        let records = collector.records();
        let output = records.last().unwrap();
        assert_eq!(output["event"], "tool-output");
        assert_eq!(output["returncode"], 3);
        assert!(output["stderr"].as_str().unwrap().starts_with("leaked: "));
        assert_no_key_material(&serde_json::to_string(&records).unwrap());
        assert_no_key_material(&result.stderr_tail);
    }
}

#[test]
fn every_per_segment_invocation_is_logged_redacted() {
    let (_dir, root) = tempdir();
    let (packager, collector) = packager(&stub(
        &root,
        &[
            ("STUB_PACKAGER_SEGMENT_COUNT", "1"),
            ("STUB_PACKAGER_STDERR", "echo_argv"),
        ],
    ));
    let (init, segments) = make_source(&root, 2);
    let result = transcrypt(&packager, &root, &init, &segments, &["dead"]).unwrap();
    assert_eq!(result.returncode, 0);
    let records = collector.records();
    assert_eq!(events(&records), ["command", "tool-output"].repeat(2 * 2));
    let decrypting: Vec<bool> = records
        .iter()
        .filter(|record| record["event"] == "command")
        .map(|record| {
            record["argv"]
                .as_array()
                .unwrap()
                .iter()
                .any(|token| token == "--enable_raw_key_decryption")
        })
        .collect();
    assert_eq!(decrypting, [true, false, true, false]);
    assert_no_key_material(&serde_json::to_string(&records).unwrap());
}

#[test]
fn timeout_is_logged_and_raised_with_no_key_material() {
    let (_dir, root) = tempdir();
    let (packager, collector) = packager(&stub(&root, &[("STUB_PACKAGER_HANG", "1")]));
    let packager = packager.with_timeout(Some(0.5));
    let Err(Error::Packager(error)) = encrypt(&packager, &root, &[]) else {
        panic!("expected a PackagerError");
    };
    assert_eq!(error.to_string(), "Shaka Packager timed out after 0.5s");
    assert_secret_free_error(&error);
    let records = collector.records();
    assert_eq!(events(&records), ["command", "tool-output"]);
    let argv: Vec<String> = serde_json::from_value(records[0]["argv"].clone()).unwrap();
    assert_eq!(argv, error.argv_redacted());
    assert_eq!(records[1]["returncode"], Value::Null);
    assert_no_key_material(&serde_json::to_string(&records).unwrap());
}

#[test]
fn spawn_failure_is_logged_and_raised() {
    let (_dir, root) = tempdir();
    let (packager, collector) = packager(&root.join("nonexistent-packager").to_string_lossy());
    let Err(Error::Packager(error)) = decrypt(&packager, &root) else {
        panic!("expected a PackagerError");
    };
    assert_eq!(error.to_string(), "could not run Shaka Packager");
    assert_secret_free_error(&error);
    let records = collector.records();
    assert_eq!(events(&records), ["command", "tool-output"]);
    let argv: Vec<String> = serde_json::from_value(records[0]["argv"].clone()).unwrap();
    assert_eq!(argv, error.argv_redacted());
    assert_eq!(records[1]["returncode"], Value::Null);
    assert!(
        records[1]["stderr"]
            .as_str()
            .unwrap()
            .contains("could not run")
    );
    assert_no_key_material(&format!(
        "{error} {error:?} {}",
        serde_json::to_string(&records).unwrap()
    ));
}

#[test]
fn without_a_sink_nothing_is_emitted_and_behaviour_is_unchanged() {
    // Logged or not, the child gets the same real argv and the caller the same result.
    let (_dir, root) = tempdir();
    let argv_file = root.join("argv.txt");
    let bin = stub(
        &root,
        &[
            ("STUB_PACKAGER_ARGV_FILE", &argv_file.to_string_lossy()),
            ("STUB_PACKAGER_STDERR", "echo_argv"),
            ("STUB_PACKAGER_EXIT", "3"),
        ],
    );
    let (logging, collector) = packager(&bin);
    let logged = encrypt(&logging, &root, &[]).unwrap();
    let logged_child_argv = recorded(&argv_file);
    assert_eq!(collector.records().len(), 2);
    let silent = Packager::new(Arc::new(ToolRunner::new(
        Arc::new(Secrets::new()),
        Arc::new(NoSink),
    )))
    .with_packager_bin(&bin);
    let unlogged = encrypt(&silent, &root, &[]).unwrap();
    assert_eq!(recorded(&argv_file), logged_child_argv);
    assert_eq!(
        (
            &unlogged.argv_redacted,
            unlogged.returncode,
            &unlogged.stderr_tail
        ),
        (
            &logged.argv_redacted,
            logged.returncode,
            &logged.stderr_tail
        )
    );
    assert_eq!(unlogged.returncode, 3);
    assert_no_key_material(&unlogged.stderr_tail);
    assert_eq!(
        collector.records().len(),
        2,
        "the other runner's sink received nothing"
    );
}

// ---- the duration text and the plain scrub, against Python ---------------------------------------

#[test]
fn format_duration_matches_python() {
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packager.oracle.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for case in oracle["format_duration"].as_array().unwrap() {
        let seconds: f64 = case[0].as_str().unwrap().parse().unwrap();
        let actual = format_duration(seconds).map_err(|error| error.to_string());
        let expected = match case[1].get("ok") {
            Some(text) => Ok(text.as_str().unwrap().to_string()),
            None => Err(case[1]["error"].as_str().unwrap().to_string()),
        };
        assert_eq!(actual, expected, "{case}");
    }
    assert_eq!(
        format_duration(ISOLATING_SEGMENT_DURATION_S).unwrap(),
        "1000000"
    );
}

#[test]
fn scrub_matches_python() {
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packager.oracle.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for case in oracle["scrub"].as_array().unwrap() {
        let secrets: Vec<&str> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|secret| secret.as_str().unwrap())
            .collect();
        assert_eq!(
            scrub(case[0].as_str().unwrap(), &secrets),
            case[2].as_str().unwrap(),
            "{case}"
        );
    }
}

#[test]
fn debug_output_never_carries_key_material_or_the_argv() {
    let error = PackagerError::new(
        "could not run Shaka Packager",
        vec!["--pssh".into(), "abcdef".into()],
    );
    let shown = format!("{error:?}");
    assert!(!shown.contains("abcdef"), "{shown}");
    let result = PackagerResult {
        argv_redacted: vec!["--pssh".into(), "abcdef".into()],
        returncode: 0,
        duration_s: 0.0,
        stderr_tail: "abcdef".into(),
    };
    assert!(!format!("{result:?}").contains("abcdef"));
}

#[test]
fn retained_packager_results_and_errors_hide_signaling_and_encoded_keys() {
    use base64::Engine as _;
    let (_dir, root) = tempdir();
    let url = "https://license.example.invalid/rights?token=synthetic-private-token&x=1";
    let objects = drmr_core::playready::build_playready_v43(VALID_KID, url).unwrap();
    let encoded_key =
        base64::engine::general_purpose::STANDARD.encode(hex::decode(VALID_KEY).unwrap());
    let diagnostic = format!(
        "{url} {} {} {} {} {encoded_key}",
        objects.wrmheader_xml, objects.pro_b64, objects.pssh_b64, objects.pssh_hex
    );
    let argv_file = root.join("actual-argv.txt");
    let bin = stub(
        &root,
        &[
            ("STUB_PACKAGER_STDERR", &diagnostic),
            ("STUB_PACKAGER_ARGV_FILE", argv_file.to_str().unwrap()),
        ],
    );
    let collector = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(
        Arc::new(Secrets::with([
            url,
            objects.wrmheader_xml.as_str(),
            objects.pro_b64.as_str(),
            objects.pssh_b64.as_str(),
        ])),
        collector.clone(),
    ));
    let packager = Packager::new(runner.clone()).with_packager_bin(bin);
    let result = encrypt(&packager, &root, &[&objects.pssh_hex]).unwrap();
    assert_eq!(after(&recorded(&argv_file), "--pssh"), objects.pssh_hex);
    assert_eq!(after(&result.argv_redacted, "--pssh"), REDACTED);
    let shown = format!(
        "{} {} {result:?} {}",
        result.argv_redacted.join(" "),
        result.stderr_tail,
        serde_json::to_string(&collector.records()).unwrap()
    );
    for value in [
        url,
        objects.wrmheader_xml.as_str(),
        objects.pro_b64.as_str(),
        objects.pssh_b64.as_str(),
        objects.pssh_hex.as_str(),
        encoded_key.as_str(),
    ] {
        assert!(!shown.contains(value), "retained secret diagnostic");
    }
    let missing = Packager::new(runner).with_packager_bin(root.join("missing").to_string_lossy());
    let Err(Error::Packager(error)) = encrypt(&missing, &root, &[&objects.pssh_hex]) else {
        panic!("expected a missing-tool failure");
    };
    assert_eq!(after(error.argv_redacted(), "--pssh"), REDACTED);
    assert_secret_free_error(&error);
    assert!(
        !format!("{error} {error:?} {}", error.argv_redacted().join(" "))
            .contains(&objects.pssh_hex)
    );
}
