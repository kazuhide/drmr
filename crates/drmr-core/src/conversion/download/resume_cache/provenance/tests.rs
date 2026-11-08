use super::*;
use std::io::Write as _;

#[test]
fn provenance_is_create_only_and_does_not_change_payload_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    let path = root.path().join("new-object");
    let mut file = crate::native::create_private_new(&path).unwrap();
    file.write_all(b"unchanged original bytes").unwrap();
    assert!(read(&file).is_err());
    assert!(read(&file).is_err());
    let nonce = create(&file).unwrap();
    assert_eq!(read(&file).unwrap(), nonce);
    assert!(create(&file).is_err());
    assert_eq!(read(&file).unwrap(), nonce);
    drop(file);
    assert_eq!(std::fs::read(path).unwrap(), b"unchanged original bytes");
}

#[test]
fn rename_preserves_provenance_and_an_independent_identical_object_has_none() {
    let root = crate::native::private_tempdir().unwrap();
    let original = root.path().join("original");
    let moved = root.path().join("moved");
    let replacement = root.path().join("replacement");
    let mut file = crate::native::create_private_new(&original).unwrap();
    file.write_all(b"identical bytes").unwrap();
    create(&file).unwrap();
    let native = FileIdentity::read(&file).unwrap();
    let expected = token(&file).unwrap();
    drop(file);
    crate::conversion::files::atomic_move(&original, &moved).unwrap();
    let current = crate::native::open_private_read(&moved).unwrap();
    assert_eq!(token(&current).unwrap(), expected);
    let mut separate = crate::native::create_private_new(&replacement).unwrap();
    separate.write_all(b"identical bytes").unwrap();
    // Both objects are alive, so this precondition cannot depend on inode reuse.
    assert!(FileIdentity::read(&separate).unwrap() != native);
    assert!(read(&separate).is_err());
}
