use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

#[test]
fn checked_cleanup_does_not_adopt_a_foreign_outer_replacement() {
    let root = tempfile::tempdir().unwrap();
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(root.path())
        .unwrap();
    let outer = UnixOuter::new(directory).unwrap();
    let path = outer.path().to_path_buf();
    let original = root.path().join("original");
    std::fs::create_dir_all(path.join("nested/deep")).unwrap();
    std::fs::write(path.join("nested/deep/owned"), b"owned cleanup bytes").unwrap();
    let result = outer.cleanup_observed(|| {
        std::fs::rename(&path, &original).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(path.join("foreign-sentinel"), b"must remain").unwrap();
    });
    drop(outer);
    assert_eq!(
        result,
        Err(CLEANUP_ERROR),
        "foreign sentinel preserved: {}",
        path.join("foreign-sentinel").exists()
    );
    assert_eq!(
        std::fs::read(path.join("foreign-sentinel")).unwrap(),
        b"must remain"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o777
    );
    assert!(original.is_dir());
    assert_eq!(std::fs::read_dir(original).unwrap().count(), 0);
}

#[test]
fn checked_cleanup_does_not_follow_a_replaced_ancestor() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("work");
    std::fs::create_dir(&parent).unwrap();
    let stage = Stage::new(Some(&parent)).unwrap();
    let leaf = stage.private_root().file_name().unwrap().to_owned();
    let original_parent = root.path().join("original-work");
    std::fs::write(stage.path().join("owned"), b"owned cleanup bytes").unwrap();
    let result = stage.outer.cleanup_observed(|| {
        std::fs::rename(&parent, &original_parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        let foreign = parent.join(&leaf);
        std::fs::create_dir(&foreign).unwrap();
        std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(foreign.join("foreign-sentinel"), b"must remain").unwrap();
    });
    drop(stage);
    assert_eq!(
        result,
        Err(CLEANUP_ERROR),
        "foreign sentinel preserved: {}",
        parent.join(&leaf).join("foreign-sentinel").exists()
    );
    assert_eq!(
        std::fs::read(parent.join(&leaf).join("foreign-sentinel")).unwrap(),
        b"must remain"
    );
    assert_eq!(
        std::fs::read_dir(original_parent.join(leaf))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn stage_refuses_a_non_sticky_writable_parent_before_creating_a_directory() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("shared");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
    let result = Stage::new(Some(&parent));
    assert_eq!(
        result.err().map(|error| error.0),
        Some("temporary-workspace-parent-unsafe")
    );
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
}

#[test]
fn a_trusted_sticky_parent_allows_cleanup_without_following_links() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("sticky");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("sentinel"), b"outside target").unwrap();
    let stage = Stage::new(Some(&parent)).unwrap();
    let path = stage.private_root().to_path_buf();
    std::fs::create_dir_all(stage.path().join("nested/deep")).unwrap();
    std::fs::write(stage.path().join("nested/deep/owned"), b"owned bytes").unwrap();
    symlink(&foreign, stage.path().join("link")).unwrap();
    stage.outer.cleanup().unwrap();
    drop(stage);
    assert!(!path.exists());
    assert_eq!(
        std::fs::read(foreign.join("sentinel")).unwrap(),
        b"outside target"
    );
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
}

#[test]
fn parent_privacy_loss_during_cleanup_retains_the_root_and_reports_cleanup_uncertainty() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("work");
    std::fs::create_dir(&parent).unwrap();
    let stage = Stage::new(Some(&parent)).unwrap();
    let path = stage.private_root().to_path_buf();
    std::fs::write(stage.path().join("owned"), b"owned bytes").unwrap();
    let result = stage.outer.cleanup_observed(|| {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(parent.join("sentinel"), b"outside stage").unwrap();
    });
    drop(stage);
    assert_eq!(result, Err(CLEANUP_ERROR));
    assert!(path.is_dir());
    assert_eq!(std::fs::read_dir(path).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(parent.join("sentinel")).unwrap(),
        b"outside stage"
    );
    assert_eq!(
        std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
        0o777
    );
}

#[test]
fn a_trusted_parent_alias_is_resolved_before_stage_creation_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("sticky");
    let alias = root.path().join("alias");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o1777)).unwrap();
    symlink(&parent, &alias).unwrap();
    let stage = Stage::new(Some(&alias)).unwrap();
    let path = stage.private_root().to_path_buf();
    assert_eq!(path.parent().unwrap(), parent.canonicalize().unwrap());
    std::fs::write(stage.path().join("owned"), b"owned cleanup bytes").unwrap();
    stage.outer.cleanup().unwrap();
    drop(stage);
    assert!(!path.exists());
    assert!(std::fs::symlink_metadata(alias).unwrap().is_symlink());
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
}

#[test]
#[cfg(target_os = "macos")]
fn the_macos_system_tmp_alias_is_a_supported_workspace_parent() {
    let stage = Stage::new(Some(Path::new("/tmp"))).unwrap();
    let path = stage.private_root().to_path_buf();
    assert_eq!(
        path.parent().unwrap(),
        Path::new("/tmp").canonicalize().unwrap()
    );
    std::fs::write(stage.path().join("owned"), b"owned cleanup bytes").unwrap();
    stage.outer.cleanup().unwrap();
    drop(stage);
    assert!(!path.exists());
}
