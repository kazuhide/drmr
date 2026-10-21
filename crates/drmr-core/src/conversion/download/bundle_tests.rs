use super::*;
use crate::{
    events::NoSink,
    toolrun::{Secrets, ToolRunner},
};

fn sealed_source(parent: &Path) -> (Converter, PathBuf, PathBuf, Vec<u8>) {
    let mut owned = OwnedDownload::reserve_source(&parent.join("download")).unwrap();
    owned.create_dir(Path::new("media")).unwrap();
    let manifest =
        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmedia/segment.ts\n#EXT-X-ENDLIST\n";
    let media = vec![0x47; 188];
    let mut expected = BTreeMap::new();
    for (name, bytes) in [
        ("input.m3u8", manifest.as_slice()),
        ("media/segment.ts", media.as_slice()),
    ] {
        let writer = write_private(&owned.path().join(name), bytes).unwrap();
        let identity = crate::native::FileIdentity::read(&writer).unwrap();
        drop(writer);
        owned.register_file(Path::new(name), identity).unwrap();
        expected.insert(PathBuf::from(name), bytes_digest(bytes));
    }
    let entrypoint = owned.path().join("input.m3u8");
    let segment = owned.path().join("media/segment.ts");
    owned.retain();
    let mut converter = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "unused",
        "unused",
    );
    converter.downloaded = Some(Arc::new(Seal {
        owned,
        files: expected,
        entrypoint: "input.m3u8",
    }));
    (converter, entrypoint, segment, media)
}

#[test]
fn downloaded_bundle_accepts_only_copied_bytes_from_the_creation_seal() {
    let root = crate::native::private_tempdir().unwrap();
    let (converter, entrypoint, _, _) = sealed_source(root.path());
    let frozen = super::super::FrozenSource::capture(&entrypoint, &Stop::new()).unwrap();
    converter
        .check_downloaded_inventory(frozen.inventory())
        .unwrap();
    let mut changed = frozen.inventory().clone();
    changed.files[0].sha256 = "0".repeat(64);
    assert_eq!(
        converter.check_downloaded_inventory(&changed).unwrap_err(),
        CHANGED
    );
    let mut wrong_root = frozen.inventory().clone();
    wrong_root.root = root.path().canonicalize().unwrap();
    assert_eq!(
        converter
            .check_downloaded_inventory(&wrong_root)
            .unwrap_err(),
        CHANGED
    );
}

#[test]
fn restoring_downloaded_names_after_a_substituted_copy_does_not_authorize_the_copy() {
    let root = crate::native::private_tempdir().unwrap();
    let (converter, entrypoint, segment, original) = sealed_source(root.path());
    converter.check_downloaded().unwrap();
    let substituted = vec![0x46; original.len()];
    std::fs::write(&segment, &substituted).unwrap();
    let frozen = super::super::FrozenSource::capture(&entrypoint, &Stop::new()).unwrap();
    std::fs::write(&segment, &original).unwrap();
    converter.check_downloaded().unwrap();
    assert_eq!(
        converter
            .check_downloaded_inventory(frozen.inventory())
            .unwrap_err(),
        CHANGED
    );
    assert_eq!(std::fs::read(segment).unwrap(), original);
}
