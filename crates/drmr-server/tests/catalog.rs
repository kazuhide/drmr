mod support;

use drmr_core::config::KeyMaterial;
use drmr_server::catalog::{
    CatalogEntry, catalog_entry_public_dict, discover_catalog, get_catalog_entry,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use support::{B_IV, B_KEY, B_KID, IV, KEY, KID};

/// The catalog test's own `_stage_entry`: placeholder manifest and playlist, a `keys.env`, an
/// `asset.json`; every knob drives one negative case.
struct Entry<'a> {
    asset_id: Value,
    label: &'a str,
    kid: &'a str,
    key: &'a str,
    iv: &'a str,
    mpd_rel: &'a str,
    hls_rel: &'a str,
    keys_file: &'a str,
    descriptor: Option<Value>,
    keys_env_text: Option<&'a str>,
    /// The one file left out: `mpd`, `hls`, `keys` or `asset.json`.
    omit: Option<&'a str>,
}

impl Default for Entry<'_> {
    fn default() -> Self {
        Entry {
            asset_id: json!("asset"),
            label: "A label",
            kid: KID,
            key: KEY,
            iv: IV,
            mpd_rel: "download/host/title/dash/manifest.mpd",
            hls_rel: "download/host/title/hls/manifest.m3u8",
            keys_file: "keys.env",
            descriptor: None,
            keys_env_text: None,
            omit: None,
        }
    }
}

fn stage(catalog_root: &Path, dir_name: &str, entry: Entry<'_>) -> PathBuf {
    let dir = catalog_root.join(dir_name);
    drmr_core::native::create_private_dir_all(&dir).unwrap();
    if entry.omit != Some("mpd") {
        let path = dir.join(entry.mpd_rel);
        drmr_core::native::create_private_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "<MPD/>").unwrap();
    }
    if entry.omit != Some("hls") {
        let path = dir.join(entry.hls_rel);
        drmr_core::native::create_private_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#EXTM3U\n").unwrap();
    }
    if entry.omit != Some("keys") {
        let text = entry.keys_env_text.map_or_else(
            || {
                format!(
                    "# staged key material\nDRM_KID={}\nDRM_KEY={}\nDRM_IV={}\n",
                    entry.kid, entry.key, entry.iv
                )
            },
            str::to_string,
        );
        support::write_private(&dir.join(entry.keys_file), text).unwrap();
    }
    if entry.omit != Some("asset.json") {
        let descriptor = entry.descriptor.unwrap_or_else(|| {
            json!({"asset_id": entry.asset_id, "label": entry.label, "mpd_path": entry.mpd_rel,
                   "hls_master_path": entry.hls_rel, "keys_file": entry.keys_file})
        });
        std::fs::write(
            dir.join("asset.json"),
            serde_json::to_string(&descriptor).unwrap(),
        )
        .unwrap();
    }
    dir
}

fn id(asset_id: &str) -> Entry<'static> {
    Entry {
        asset_id: json!(asset_id),
        ..Entry::default()
    }
}

fn ids(entries: &[CatalogEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.asset_id.clone()).collect()
}

fn discover(root: &Path) -> Vec<CatalogEntry> {
    discover_catalog(root, None).unwrap()
}

fn skips(root: &Path) -> Vec<(PathBuf, String)> {
    let mut skipped = Vec::new();
    let mut hook = |dir: &Path, reason: &str| skipped.push((dir.to_path_buf(), reason.to_string()));
    discover_catalog(root, Some(&mut hook)).unwrap();
    skipped
}

fn temp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("catalog");
    drmr_core::native::create_private_dir_all(&root).unwrap();
    (dir, root)
}

#[test]
fn discover_returns_all_valid_entries_sorted_by_asset_id() {
    let (_dir, root) = temp();
    stage(
        &root,
        "zzz-dir",
        Entry {
            label: "Alpha",
            ..id("alpha")
        },
    );
    stage(
        &root,
        "aaa-dir",
        Entry {
            label: "Charlie",
            ..id("charlie")
        },
    );
    stage(
        &root,
        "mmm-dir",
        Entry {
            label: "Bravo",
            ..id("bravo")
        },
    );
    let entries = discover(&root);
    assert_eq!(ids(&entries), ["alpha", "bravo", "charlie"]);
    let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["Alpha", "Bravo", "Charlie"]);
}

#[test]
fn discover_resolves_paths_absolute_and_loads_key_material() {
    let (_dir, root) = temp();
    stage(&root, "e1", id("asset-1"));
    let entries = discover(&root);
    let [entry] = entries.as_slice() else {
        panic!("{entries:?}")
    };
    assert!(entry.mpd_path.is_absolute() && entry.mpd_path.is_file());
    assert!(entry.hls_master_path.is_absolute() && entry.hls_master_path.is_file());
    assert_eq!(entry.mpd_path.file_name().unwrap(), "manifest.mpd");
    assert_eq!(entry.key_material, KeyMaterial::new(KID, KEY, IV).unwrap());
}

#[test]
fn discover_rejects_keys_in_served_download_tree() {
    for through_symlink in [false, true] {
        let (_dir, root) = temp();
        let entry = stage(&root, "bad", id("bad-asset"));
        let keys = entry.join("download").join("secret.env");
        std::fs::rename(entry.join("keys.env"), &keys).unwrap();
        let mut descriptor: Value =
            serde_json::from_slice(&std::fs::read(entry.join("asset.json")).unwrap()).unwrap();
        if through_symlink {
            support::file_link(&keys, &entry.join("keys.env"));
            descriptor["keys_file"] = json!("keys.env");
        } else {
            descriptor["keys_file"] = json!("download/secret.env");
        }
        std::fs::write(entry.join("asset.json"), descriptor.to_string()).unwrap();
        stage(&root, "good", id("good-asset"));
        let skipped = skips(&root);
        assert_eq!(ids(&discover(&root)), ["good-asset"]);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].1.contains("keys_file"), "{}", skipped[0].1);
        assert!(!skipped[0].1.contains(KEY) && !skipped[0].1.contains(IV));
    }
}

#[test]
fn discover_ignores_non_directory_and_dirs_without_asset_json() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    std::fs::write(root.join("stray-file.txt"), "not an entry").unwrap();
    std::fs::create_dir(root.join("empty-dir")).unwrap();
    assert_eq!(ids(&discover(&root)), ["good-asset"]);
}

#[test]
fn discover_returns_empty_for_missing_or_non_directory_root() {
    let (dir, _root) = temp();
    assert_eq!(
        discover(&dir.path().join("does-not-exist")),
        Vec::<CatalogEntry>::new()
    );
    let file = dir.path().join("a-file");
    std::fs::write(&file, "x").unwrap();
    assert_eq!(discover(&file), Vec::<CatalogEntry>::new());
}

#[test]
fn discover_skips_malformed_json_but_keeps_valid_siblings() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    std::fs::create_dir(root.join("bad")).unwrap();
    std::fs::write(root.join("bad/asset.json"), "{not valid json").unwrap();
    assert_eq!(ids(&discover(&root)), ["good-asset"]);
}

#[test]
fn discover_skips_entry_missing_a_required_field() {
    for missing in [
        "asset_id",
        "label",
        "mpd_path",
        "hls_master_path",
        "keys_file",
    ] {
        let (_dir, root) = temp();
        stage(&root, "good", id("good-asset"));
        let mut descriptor = json!({"asset_id": "broken-asset", "label": "Broken",
            "mpd_path": "download/host/title/dash/manifest.mpd",
            "hls_master_path": "download/host/title/hls/manifest.m3u8", "keys_file": "keys.env"});
        descriptor.as_object_mut().unwrap().remove(missing);
        stage(
            &root,
            "broken",
            Entry {
                descriptor: Some(descriptor),
                ..id("broken-asset")
            },
        );
        assert_eq!(ids(&discover(&root)), ["good-asset"], "{missing}");
    }
}

#[test]
fn discover_skips_unsafe_asset_id_but_keeps_valid_siblings() {
    for bad in [
        "../../escape",
        "..",
        "foo/bar",
        "foo\\bar",
        "",
        ".",
        ".hidden",
        "a\u{0}b",
    ] {
        let (_dir, root) = temp();
        stage(&root, "good", id("good-asset"));
        stage(&root, "evil", id(bad));
        assert_eq!(ids(&discover(&root)), ["good-asset"], "{bad:?}");
    }
}

#[test]
fn discover_skips_entry_with_a_missing_referenced_file() {
    for missing in ["mpd", "hls", "keys"] {
        let (_dir, root) = temp();
        stage(&root, "good", id("good-asset"));
        stage(
            &root,
            "broken",
            Entry {
                omit: Some(missing),
                ..id("broken-asset")
            },
        );
        assert_eq!(ids(&discover(&root)), ["good-asset"], "{missing}");
    }
}

#[test]
fn discover_skips_entry_whose_referenced_path_escapes_the_entry_dir() {
    let (dir, root) = temp();
    std::fs::write(
        std::fs::canonicalize(dir.path())
            .unwrap()
            .join("outside.env"),
        format!("DRM_KID={KID}\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
    )
    .unwrap();
    stage(&root, "good", id("good-asset"));
    stage(
        &root,
        "escaper",
        Entry {
            keys_file: "../../outside.env",
            omit: Some("keys"),
            ..id("escaper-asset")
        },
    );
    assert_eq!(ids(&discover(&root)), ["good-asset"]);
}

#[test]
fn keys_env_parses_with_comments_and_blank_lines() {
    let (_dir, root) = temp();
    let text = format!("# a comment\n\nDRM_KID={KID}\n   \nDRM_KEY={KEY}\nDRM_IV={IV}\n");
    stage(
        &root,
        "e1",
        Entry {
            keys_env_text: Some(&text),
            ..id("asset-1")
        },
    );
    assert_eq!(discover(&root)[0].key_material.key_id_hex(), KID);
}

#[test]
fn discover_skips_entry_with_malformed_key_material() {
    for text in [
        format!("DRM_KID=not-hex\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
        format!("DRM_KID=aabb\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
        format!("DRM_KEY={KEY}\nDRM_IV={IV}\n"),
        String::new(),
    ] {
        let (_dir, root) = temp();
        stage(&root, "good", id("good-asset"));
        stage(
            &root,
            "badkeys",
            Entry {
                keys_env_text: Some(&text),
                ..id("badkeys-asset")
            },
        );
        assert_eq!(ids(&discover(&root)), ["good-asset"]);
    }
}

#[test]
fn entry_dir_is_the_discovered_directory_not_derived_from_asset_id() {
    let (_dir, root) = temp();
    stage(&root, "zzz-dir", id("alpha"));
    let entries = discover(&root);
    let entry = &entries[0];
    assert_eq!(entry.asset_id, "alpha");
    assert_eq!(entry.entry_dir, root.join("zzz-dir"));
    assert!(entry.entry_dir.is_absolute() && entry.entry_dir.join("download").is_dir());
    assert!(entry.mpd_path.starts_with(&entry.entry_dir));
}

#[test]
fn entry_dir_of_a_symlinked_entry_is_the_real_directory() {
    let (dir, root) = temp();
    let real = std::fs::canonicalize(dir.path()).unwrap().join("elsewhere");
    stage(&real, "real-dir", id("linked-asset"));
    support::directory_link(&real.join("real-dir"), &root.join("link"));
    let entries = discover(&root);
    assert_eq!(entries[0].entry_dir, real.join("real-dir"));
    assert!(entries[0].mpd_path.starts_with(&entries[0].entry_dir));
}

#[test]
fn get_catalog_entry_carries_entry_dir_too() {
    let (_dir, root) = temp();
    stage(&root, "zzz-dir", id("alpha"));
    let entry = get_catalog_entry(&root, "alpha").unwrap().unwrap();
    assert_eq!(entry.entry_dir, root.join("zzz-dir"));
    assert!(
        get_catalog_entry(&root, "zzz-dir").unwrap().is_none(),
        "a directory name is not an asset id"
    );
}

#[test]
fn duplicate_asset_id_keeps_the_first_directory_and_skips_the_rest() {
    let (_dir, root) = temp();
    stage(
        &root,
        "a-first",
        Entry {
            label: "First",
            ..id("same")
        },
    );
    stage(
        &root,
        "b-copy",
        Entry {
            label: "Copy",
            ..id("same")
        },
    );
    let skipped = skips(&root);
    let entries = discover(&root);
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.asset_id.as_str(), e.label.as_str()))
            .collect::<Vec<_>>(),
        [("same", "First")]
    );
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0.file_name().unwrap(), "b-copy");
    assert!(skipped[0].1.contains("duplicate asset_id"));
    assert!(skipped[0].1.contains("same") && skipped[0].1.contains("a-first"));
}

#[test]
fn on_skip_reports_each_skipped_directory_once_with_a_reason() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    std::fs::create_dir(root.join("no-descriptor")).unwrap();
    stage(
        &root,
        "broken-json",
        Entry {
            omit: Some("asset.json"),
            ..id("x")
        },
    );
    std::fs::write(root.join("broken-json/asset.json"), "{not valid json").unwrap();
    stage(
        &root,
        "missing-label",
        Entry {
            descriptor: Some(
                json!({"asset_id": "missing-label-asset", "mpd_path": "download/host/title/dash/manifest.mpd",
                "hls_master_path": "download/host/title/hls/manifest.m3u8", "keys_file": "keys.env"}),
            ),
            ..id("x")
        },
    );
    stage(&root, "unsafe-id", id("../../escape"));
    stage(
        &root,
        "no-mpd",
        Entry {
            omit: Some("mpd"),
            ..id("no-mpd-asset")
        },
    );
    std::fs::write(root.join("plain-file.txt"), "not a directory").unwrap();
    let skipped: std::collections::HashMap<String, String> = skips(&root)
        .into_iter()
        .map(|(dir, reason)| {
            (
                dir.file_name().unwrap().to_string_lossy().into_owned(),
                reason,
            )
        })
        .collect();
    let mut names: Vec<&String> = skipped.keys().collect();
    names.sort();
    assert_eq!(
        names,
        [
            "broken-json",
            "missing-label",
            "no-descriptor",
            "no-mpd",
            "unsafe-id"
        ]
    );
    assert!(skipped["no-descriptor"].contains("no asset.json"));
    assert!(skipped["broken-json"].contains("invalid JSON"));
    assert!(skipped["missing-label"].contains("missing required field(s): label"));
    assert!(skipped["unsafe-id"].contains("unsafe asset_id"));
    assert!(skipped["no-mpd"].contains("mpd_path: referenced file not found"));
}

#[test]
fn on_skip_names_which_referenced_file_is_missing() {
    for (missing, field) in [
        ("mpd", "mpd_path"),
        ("hls", "hls_master_path"),
        ("keys", "keys_file"),
    ] {
        let (_dir, root) = temp();
        stage(
            &root,
            "broken",
            Entry {
                omit: Some(missing),
                ..id("broken-asset")
            },
        );
        let skipped = skips(&root);
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0]
                .1
                .contains(&format!("{field}: referenced file not found")),
            "{}",
            skipped[0].1
        );
    }
}

#[test]
fn on_skip_reports_an_escaping_reference_by_field() {
    let (dir, root) = temp();
    std::fs::write(
        std::fs::canonicalize(dir.path())
            .unwrap()
            .join("outside.env"),
        format!("DRM_KID={KID}\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
    )
    .unwrap();
    stage(
        &root,
        "escaper",
        Entry {
            keys_file: "../../outside.env",
            omit: Some("keys"),
            ..id("escaper-asset")
        },
    );
    let skipped = skips(&root);
    assert!(
        skipped[0]
            .1
            .contains("keys_file: path escapes the entry directory"),
        "{}",
        skipped[0].1
    );
}

#[test]
fn on_skip_is_diagnostics_only_and_never_changes_the_result() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    stage(
        &root,
        "bad",
        Entry {
            omit: Some("keys"),
            ..id("bad-asset")
        },
    );
    let mut hook = |_: &Path, _: &str| {};
    let with_hook = discover_catalog(&root, Some(&mut hook)).unwrap();
    assert_eq!(with_hook, discover(&root));
    assert_eq!(ids(&with_hook), ["good-asset"]);
}

#[test]
fn on_skip_reason_for_malformed_keys_env_never_contains_a_value_from_the_file() {
    let cases = [
        (
            format!("DRM_KID=0badc0de\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
            vec!["0badc0de", KEY, IV],
        ),
        (
            format!("DRM_KID={KID}\nDRM_KEY=not-hex-but-distinctive-zq7\nDRM_IV={IV}\n"),
            vec!["distinctive", "zq7", IV],
        ),
        (
            format!("DRM_KID={KID}\nDRM_KEY={KEY}\nDRM_IV=00112233445566\n"),
            vec!["00112233445566", KEY],
        ),
        (format!("DRM_KID={KID}\nDRM_KEY={KEY}\n"), vec![KEY]),
    ];
    for (text, secrets) in cases {
        let (_dir, root) = temp();
        stage(
            &root,
            "badkeys",
            Entry {
                keys_env_text: Some(&text),
                ..id("badkeys-asset")
            },
        );
        let skipped = skips(&root);
        let (dir, reason) = &skipped[0];
        assert_eq!(dir.file_name().unwrap(), "badkeys");
        assert!(
            reason.contains("keys.env") && reason.contains("invalid key material"),
            "{reason}"
        );
        for secret in secrets {
            assert!(
                !reason.contains(secret),
                "skip reason echoed {secret:?}: {reason:?}"
            );
        }
        assert!(!reason.contains(KEY) && !reason.contains(IV));
    }
}

#[test]
fn on_skip_reason_names_the_offending_field() {
    let (_dir, root) = temp();
    let text = format!("DRM_KID={KID}\nDRM_KEY=abcd\nDRM_IV={IV}\n");
    stage(
        &root,
        "badkeys",
        Entry {
            keys_env_text: Some(&text),
            ..id("badkeys-asset")
        },
    );
    let skipped = skips(&root);
    assert!(
        skipped[0]
            .1
            .contains("key_hex must decode to 16 bytes, got 2"),
        "{}",
        skipped[0].1
    );
}

#[test]
fn keys_file_that_is_not_utf8_is_skipped_and_its_bytes_are_never_echoed() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    let bad = stage(&root, "binary-keys", id("binary-asset"));
    let mut bytes = b"DRM_KEY=\xff\xfe\xfd\x80".to_vec();
    bytes.extend_from_slice(KEY.as_bytes());
    bytes.push(b'\n');
    std::fs::write(bad.join("keys.env"), bytes).unwrap();
    let skipped = skips(&root);
    assert_eq!(ids(&discover(&root)), ["good-asset"]);
    let [(dir, reason)] = skipped.as_slice() else {
        panic!("{skipped:?}")
    };
    assert_eq!(dir.file_name().unwrap(), "binary-keys");
    assert!(reason.contains("keys.env"));
    for echoed in [
        "0xff",
        "\\xff",
        "invalid start byte",
        "codec",
        "position",
        KEY,
    ] {
        assert!(!reason.contains(echoed), "{echoed} {reason}");
    }
}

#[test]
fn unreadable_keys_file_is_skipped_never_raised() {
    if support::is_root() {
        return; // a permission denial is not enforced for root
    }
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    let locked = stage(&root, "locked", id("locked-asset"));
    let keys = locked.join("keys.env");
    support::set_mode(&keys, 0o000);
    let skipped = skips(&root);
    let entries = discover(&root);
    support::set_mode(&keys, 0o600);
    assert_eq!(ids(&entries), ["good-asset"]);
    let [(dir, reason)] = skipped.as_slice() else {
        panic!("{skipped:?}")
    };
    assert_eq!(dir.file_name().unwrap(), "locked");
    assert!(reason.contains("keys.env") && !reason.contains(KEY));
}

#[test]
fn a_keys_file_with_an_additional_link_is_not_admitted() {
    let (_dir, root) = temp();
    stage(&root, "good", id("good-asset"));
    let rejected = stage(&root, "linked", id("linked-asset"));
    let keys = rejected.join("keys.env");
    let before = std::fs::read(&keys).unwrap();
    std::fs::hard_link(&keys, root.parent().unwrap().join("key-alias")).unwrap();
    assert_eq!(ids(&discover(&root)), ["good-asset"]);
    assert_eq!(std::fs::read(keys).unwrap(), before);
}

fn sample_entry() -> CatalogEntry {
    CatalogEntry {
        asset_id: "asset-1".into(),
        label: "Asset One".into(),
        mpd_path: PathBuf::from("/abs/download/host/title/dash/manifest.mpd"),
        hls_master_path: PathBuf::from("/abs/download/host/title/hls/manifest.m3u8"),
        key_material: KeyMaterial::new(KID, KEY, IV).unwrap(),
        entry_dir: PathBuf::from("/abs"),
    }
}

#[test]
fn public_dict_has_exactly_the_allowlisted_keys() {
    for converted in [true, false] {
        let entry = sample_entry();
        let public = catalog_entry_public_dict(&entry, converted);
        let mut keys: Vec<&String> = public.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["asset_id", "converted", "entry", "label", "paths"]);
        assert_eq!(
            (public["asset_id"].as_str(), public["label"].as_str()),
            (Some("asset-1"), Some("Asset One"))
        );
        assert_eq!(public["converted"], json!(converted));
        assert_eq!(public["entry"], json!("abs"));
        let paths = public["paths"].as_object().unwrap();
        assert_eq!(paths.len(), 2);
        for value in paths.values() {
            let value = value.as_str().unwrap();
            assert!(!value.is_empty() && !value.starts_with('/') && !value.contains(".."));
        }
        assert!(!public.to_string().contains("keys"));
    }
}

#[test]
fn public_dict_never_contains_key_material_under_any_key_or_value() {
    let serialized = catalog_entry_public_dict(&sample_entry(), true).to_string();
    assert!(!serialized.contains(KEY) && !serialized.contains(IV));
    assert!(
        !serialized.contains(KID),
        "the KID is public but not part of the listing"
    );
}

#[test]
fn debug_of_an_entry_never_prints_the_key_or_iv() {
    // The Rust counterpart of `test_public_dict_is_not_dataclasses_asdict_which_would_leak`:
    // no structural dump of an entry (its `Debug`) carries the key or the IV.
    let printed = format!("{:?}", sample_entry());
    assert!(!printed.contains(KEY) && !printed.contains(IV));
    assert!(!printed.contains(KID), "{printed}");
    let public = catalog_entry_public_dict(&sample_entry(), false);
    assert!(public.get("key_material").is_none());
}

#[test]
fn get_catalog_entry_returns_the_matching_entry() {
    let (_dir, root) = temp();
    stage(
        &root,
        "e1",
        Entry {
            label: "One",
            ..id("asset-1")
        },
    );
    stage(
        &root,
        "e2",
        Entry {
            label: "Two",
            kid: B_KID,
            key: B_KEY,
            iv: B_IV,
            ..id("asset-2")
        },
    );
    let entry = get_catalog_entry(&root, "asset-2").unwrap().unwrap();
    assert_eq!(
        (entry.asset_id.as_str(), entry.label.as_str()),
        ("asset-2", "Two")
    );
    assert_eq!(entry.key_material.key_id_hex(), B_KID);
}

#[test]
fn get_catalog_entry_unknown_id_returns_none() {
    let (_dir, root) = temp();
    stage(&root, "e1", id("asset-1"));
    assert!(get_catalog_entry(&root, "no-such-asset").unwrap().is_none());
}

#[test]
fn get_catalog_entry_validates_asset_id_itself_independent_of_caller() {
    let (_dir, root) = temp();
    stage(&root, "e1", id("asset-1"));
    for unsafe_id in [
        "../../etc/passwd",
        "..",
        "foo/bar",
        "",
        ".",
        ".hidden",
        "a\u{0}b",
        "foo\\bar",
    ] {
        assert!(
            get_catalog_entry(&root, unsafe_id).unwrap().is_none(),
            "{unsafe_id:?}"
        );
    }
}
