//! Protected key configuration keeps input and output roles distinct.
mod common;
use drmr_core::conversion::key_file::read;
use std::path::Path;

fn config(path: &Path, text: &str) {
    use std::io::Write as _;
    drmr_core::native::create_private_or_truncate(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

#[test]
fn key_file_has_strict_versioned_roles_and_redacted_debug() {
    let dir = common::private_tempdir().unwrap();
    let file = dir.path().join("keys.json");
    config(
        &file,
        r#"{"schema_version":1,"input":{"keys":[{"kid":"00000000000000000000000000000001","key":"11111111111111111111111111111111"}]},"output":{"kid":"00000000000000000000000000000002","key":"22222222222222222222222222222222"}}"#,
    );
    let keys = read(&file).unwrap();
    let shown = format!("{keys:?}");
    assert!(!shown.contains("111111") && !shown.contains("222222"));
    assert!(keys.output.is_some());
}

#[test]
fn segment_keys_need_no_mp4_identifier_and_keep_uris_private() {
    let dir = common::private_tempdir().unwrap();
    let file = dir.path().join("keys.json");
    config(
        &file,
        r#"{"schema_version":1,"input":{"hls_keys":[{"uri":"https://keys.example.test/credential-sentinel","key":"11111111111111111111111111111111"}]},"output":{"key":"22222222222222222222222222222222","hls_key_uri":"keys/output-sentinel"}}"#,
    );
    let keys = read(&file).unwrap();
    assert!(keys.output.is_none());
    assert!(keys.hls_output.is_some());
    assert!(keys.output_hls_verification_keys().is_ok());
    assert!(keys.output_verification_keys().is_ok());
    let shown = format!("{keys:?}");
    assert!(!shown.contains("sentinel") && !shown.contains("111111") && !shown.contains("222222"));
}

#[test]
fn sample_encryption_can_supply_an_explicit_hls_key_location_without_changing_key_type() {
    let dir = common::private_tempdir().unwrap();
    let file = dir.path().join("keys.json");
    config(
        &file,
        r#"{"schema_version":1,"output":{"kid":"00000000000000000000000000000002","key":"22222222222222222222222222222222","hls_key_uri":"keys/credential-sentinel"}}"#,
    );
    let keys = read(&file).unwrap();
    assert!(keys.output.is_some());
    assert!(keys.hls_output.is_none());
    assert_eq!(keys.hls_output_uri(), Some("keys/credential-sentinel"));
    assert!(!format!("{keys:?}").contains("sentinel"));
}

#[test]
fn malformed_unknown_and_duplicate_fields_fail_without_exposing_contents() {
    let dir = common::private_tempdir().unwrap();
    let file = dir.path().join("credential-sentinel.json");
    for text in [
        r#"{"schema_version":1,"unexpected":"secret-sentinel"}"#,
        r#"{"schema_version":1,"schema_version":1}"#,
        r#"{"schema_version":2}"#,
        r#"{"schema_version":1,"output":{"kid":"secret-sentinel","key":"secret-sentinel"}}"#,
    ] {
        config(&file, text);
        let error = read(&file).expect_err("invalid configuration must fail");
        assert_eq!(error.to_string(), "key-file-invalid");
        assert!(!format!("{error:?}").contains("sentinel"));
    }
}

#[cfg(unix)]
#[test]
fn shared_permissions_links_and_oversized_keys_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = common::private_tempdir().unwrap();
    let file = dir.path().join("keys.json");
    config(&file, r#"{"schema_version":1}"#);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let alias = dir.path().join("alias.json");
    symlink(&file, &alias).unwrap();
    assert!(read(&alias).is_err());
    std::fs::remove_file(&alias).unwrap();
    std::fs::hard_link(&file, &alias).unwrap();
    assert!(read(&file).is_err());
    std::fs::remove_file(&alias).unwrap();
    config(&file, &" ".repeat(65_537));
    assert!(read(&file).is_err());
}

#[cfg(windows)]
#[test]
fn inherited_acl_hard_links_and_oversized_key_files_are_refused_without_exposure() {
    let dir = common::private_tempdir().unwrap();
    let ordinary = tempfile::tempdir().unwrap();
    let file = dir.path().join("keys.json");
    config(&file, r#"{"schema_version":1}"#);
    assert!(read(&file).is_ok());
    let alias = ordinary.path().join("alias.json");
    std::fs::hard_link(&file, &alias).unwrap();
    assert!(read(&file).is_err());
    assert!(read(&alias).is_err());
    std::fs::remove_file(alias).unwrap();
    assert!(read(&file).is_ok());
    config(&file, &" ".repeat(65_537));
    assert!(read(&file).is_err());
    let inherited = ordinary.path().join("keys.json");
    std::fs::write(&inherited, r#"{"schema_version":1}"#).unwrap();
    assert!(read(&inherited).is_err());
}
