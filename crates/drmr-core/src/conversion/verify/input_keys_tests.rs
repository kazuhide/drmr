use super::*;
use crate::conversion::{InputKeys, keys::ProbeKeys};

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn keys() -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap()
}

#[test]
fn hls_input_uses_its_uri_without_losing_the_native_metadata_dictionary() {
    let root = crate::native::private_tempdir().unwrap();
    let metadata = fixture("cenc-iv8.mp4");
    let keys = keys();
    let dictionary = &keys;
    for (index, prefix) in [b"#EXTM3U\n".as_slice(), b"\xef\xbb\xbf \r\n#EXTM3U\n"]
        .into_iter()
        .enumerate()
    {
        let input = root.path().join(format!("reference-{index}.mp4"));
        std::fs::write(&input, prefix).unwrap();
        let view = StreamView::new(&input, 0, None)
            .with_metadata(&metadata)
            .with_all_keys(dictionary);
        assert!(input_key(view).unwrap().is_none());
        assert!(matches!(view.key, Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary)));
        assert!(
            matches!(metadata_key(view), Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary))
        );
    }
}

#[test]
fn raw_mp4_retains_every_key_even_when_metadata_is_clear_or_the_name_is_misleading() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("container.m3u8");
    std::fs::copy(fixture("cenc-iv8.mp4"), &input).unwrap();
    let metadata = fixture("clip.mp4");
    let keys = keys();
    let dictionary = &keys;
    let view = StreamView::new(&input, 0, None)
        .with_metadata(&metadata)
        .with_all_keys(dictionary);
    assert!(
        matches!(input_key(view).unwrap(), Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary))
    );
    assert!(metadata_key(view).is_none());
    assert!(matches!(view.key, Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary)));
}

#[test]
fn missing_decoder_input_is_refused_instead_of_reusing_metadata_keys() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("missing.m3u8");
    let metadata = fixture("cenc-iv8.mp4");
    let keys = keys();
    let view = StreamView::new(&input, 0, None)
        .with_metadata(&metadata)
        .with_all_keys(&keys);
    assert_eq!(input_key(view).err().unwrap().0, "input-unreadable");
}
