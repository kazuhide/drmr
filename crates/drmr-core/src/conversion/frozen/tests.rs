use super::*;
use std::io::Write as _;

fn hls(root: &Path) -> PathBuf {
    std::fs::write(root.join("segment.ts"), b"original media").unwrap();
    let input = root.join("input.m3u8");
    std::fs::write(
        &input,
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    input
}

#[test]
#[cfg(unix)]
fn restored_original_cannot_hide_different_bytes_copied_during_execution() {
    use std::io::{Seek as _, SeekFrom};
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let original = vec![b'A'; 3 * 64 * 1024];
    std::fs::write(root.path().join("segment.ts"), &original).unwrap();
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let mut changed = false;
    let mut restored = false;
    let error =
        FrozenSource::from_inventory_observed(source.inventory(), &Stop::new(), |path, bytes| {
            if path != Path::new("segment.ts") {
                return;
            }
            if bytes == 64 * 1024 {
                let mut writer = std::fs::OpenOptions::new()
                    .write(true)
                    .open(root.path().join(path))
                    .unwrap();
                writer.seek(SeekFrom::Start(64 * 1024)).unwrap();
                writer.write_all(&vec![b'B'; 64 * 1024]).unwrap();
                writer.sync_all().unwrap();
                changed = true;
            } else if bytes == 128 * 1024 {
                std::fs::write(root.path().join(path), &original).unwrap();
                restored = true;
            }
        })
        .unwrap_err();
    assert!(changed && restored);
    assert_eq!(error, CHANGED);
    assert_eq!(
        std::fs::read(root.path().join("segment.ts")).unwrap(),
        original
    );
    source.check_original().unwrap();
}

#[test]
fn inventory_structure_limits_aliases_and_graph_closure_are_checked() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    for mutate in [0, 1, 2, 3, 4, 5, 6] {
        let mut inventory = source.inventory().clone();
        match mutate {
            0 => inventory.schema_version = 2,
            1 => inventory.files[0].path = PathBuf::from("../outside"),
            2 => inventory.files.reverse(),
            3 => inventory.files[0].bytes = MAX_GRAPH_BYTES + 1,
            4 => {
                let mut alias = inventory.files[1].clone();
                alias.path = PathBuf::from("SEGMENT.ts");
                inventory.files.push(alias);
                inventory.files.sort_by(|a, b| a.path.cmp(&b.path));
            }
            5 => inventory.files[0].sha256 = "A".repeat(64),
            6 => inventory.entrypoint = PathBuf::from("missing.m3u8"),
            _ => unreachable!(),
        }
        inventory.graph_digest = graph_digest(&inventory).unwrap();
        assert!(
            FrozenSource::from_inventory(&inventory, &Stop::new()).is_err(),
            "case {mutate}"
        );
    }
    let mut inventory = source.inventory().clone();
    std::fs::write(root.path().join("unrelated.txt"), b"unrelated").unwrap();
    inventory.files.push(SourceFile {
        path: "unrelated.txt".into(),
        bytes: 9,
        sha256: hex::encode(Sha256::digest(b"unrelated")),
    });
    inventory.graph_digest = graph_digest(&inventory).unwrap();
    assert!(FrozenSource::from_inventory(&inventory, &Stop::new()).is_err());
    source.check_original().unwrap();
}

#[test]
fn aggregate_byte_budget_is_checked_before_any_source_is_copied() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let mut inventory = source.inventory().clone();
    inventory.files[1].bytes = MAX_GRAPH_BYTES;
    inventory.graph_digest = graph_digest(&inventory).unwrap();
    assert_eq!(
        FrozenSource::from_inventory(&inventory, &Stop::new()).unwrap_err(),
        LIMIT
    );
    let stage = files::Stage::new(None).unwrap();
    let mut chunks = 0;
    let result = copy_object(
        &inventory.root,
        stage.path(),
        Path::new("segment.ts"),
        None,
        0,
        &Stop::new(),
        &mut |_, _| chunks += 1,
    );
    assert_eq!(result.err(), Some(LIMIT));
    assert_eq!(chunks, 0);
    assert!(!stage.path().join("segment.ts").exists());
    source.check_original().unwrap();
}

#[test]
fn same_bytes_replacement_invalidates_the_live_original_guard() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let file = root.path().join("segment.ts");
    std::fs::rename(&file, root.path().join("retained-old.ts")).unwrap();
    std::fs::write(&file, b"original media").unwrap();
    assert_eq!(source.check_original().unwrap_err(), CHANGED);
    assert_eq!(
        std::fs::read(source.entrypoint().parent().unwrap().join("segment.ts")).unwrap(),
        b"original media"
    );
    // A new process may reopen an identical logical source; it acquires no cleanup rights.
    FrozenSource::from_inventory(source.inventory(), &Stop::new()).unwrap();
    assert!(root.path().join("retained-old.ts").exists());
}

#[test]
fn references_are_taken_from_copied_manifests_and_keys_are_never_opened() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let text = std::fs::read_to_string(&input).unwrap().replace(
        "#EXTINF:2,",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.invalid/key\"\n#EXTINF:2,",
    );
    std::fs::write(&input, text).unwrap();
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    assert_eq!(source.inventory().files.len(), 2);
    let saved = source.inventory().clone();
    std::fs::write(&input, "#EXTM3U\n#EXTINF:2,\nforeign.ts\n#EXT-X-ENDLIST\n").unwrap();
    assert!(FrozenSource::from_inventory(&saved, &Stop::new()).is_err());
    assert!(!root.path().join("foreign.ts").exists());
}

#[test]
fn private_source_guard_survives_measurement_and_blocks_both_publication_forms() {
    use crate::{
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };
    for directory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let input = hls(root.path());
        let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
        let converter = super::super::Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "unused",
            "unused",
        )
        .using_frozen_source(&source);
        let stage = files::Stage::new(Some(root.path())).unwrap();
        let candidate = stage.path().join("candidate");
        files::create_new(&candidate)
            .unwrap()
            .write_all(b"verified candidate")
            .unwrap();
        std::fs::write(root.path().join("segment.ts"), b"changed media").unwrap();
        let output = root.path().join("output");
        let (result, _) = converter.measure(|command| {
            if directory {
                command.publish_directory(stage.path(), &output)
            } else {
                command.publish(&candidate, &output)
            }
        });
        assert_eq!(result.unwrap_err(), CHANGED);
        assert!(!output.exists());
        assert!(input.exists());
    }
}
