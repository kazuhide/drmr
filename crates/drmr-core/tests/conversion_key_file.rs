//! Protected key configuration keeps input and output roles distinct.
use drmr_core::conversion::key_file::read;
use std::path::Path;

fn config(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn key_file_has_strict_versioned_roles_and_redacted_debug() {
    let dir = tempfile::tempdir().unwrap();
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
fn malformed_unknown_and_duplicate_fields_fail_without_exposing_contents() {
    let dir = tempfile::tempdir().unwrap();
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
    let dir = tempfile::tempdir().unwrap();
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
