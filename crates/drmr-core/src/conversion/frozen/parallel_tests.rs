use super::*;
use execution::Execution;
use std::sync::{
    Barrier, Mutex,
    atomic::{AtomicBool, Ordering},
};

const MIB: u64 = 1024 * 1024;

fn presentation(root: &Path) -> PathBuf {
    std::fs::create_dir(root.join("variant")).unwrap();
    for index in 0..4 {
        std::fs::write(
            root.join(format!("variant/segment-{index}.ts")),
            vec![b'A'; 3 * 64 * 1024],
        )
        .unwrap();
    }
    let input = root.join("input.m3u8");
    std::fs::write(&input,
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nvariant/segment-3.ts\n#EXTINF:2,\nvariant/segment-2.ts\n#EXTINF:2,\nvariant/segment-1.ts\n#EXTINF:2,\nvariant/segment-0.ts\n#EXTINF:2,\nvariant/segment-3.ts\n#EXT-X-ENDLIST\n"
    ).unwrap();
    input
}

#[test]
fn parallel_capture_and_saved_reopen_keep_exact_sorted_inventory_and_private_bytes() {
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let serial = FrozenSource::capture(&input, &Stop::new()).unwrap();
    for maximum in [Some(1), Some(2), Some(4), None] {
        let workers = maximum.unwrap_or(4);
        let entered = Mutex::new(std::collections::HashSet::new());
        let start = Barrier::new(workers);
        let observer = |path: &Path, bytes| {
            if path.extension().is_some_and(|extension| extension == "ts") && bytes == 64 * 1024 {
                entered.lock().unwrap().insert(std::thread::current().id());
                start.wait();
            }
        };
        let captured = capture::capture(
            &input,
            &Stop::new(),
            Execution::for_test(maximum, 4, 512 * MIB),
            &observer,
        )
        .unwrap();
        assert_eq!(entered.lock().unwrap().len(), workers);
        assert_eq!(captured.inventory(), serial.inventory());
        let reopened = capture::from_inventory(
            serial.inventory(),
            &Stop::new(),
            Execution::for_test(maximum, 4, 512 * MIB),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(reopened.inventory(), serial.inventory());
        for file in &serial.inventory().files {
            let original = std::fs::read(root.path().join(&file.path)).unwrap();
            assert_eq!(
                std::fs::read(captured.entrypoint().parent().unwrap().join(&file.path)).unwrap(),
                original
            );
            assert_eq!(
                std::fs::read(reopened.entrypoint().parent().unwrap().join(&file.path)).unwrap(),
                original
            );
        }
        captured.check_original().unwrap();
        reopened.check_original().unwrap();
    }
}

#[test]
#[cfg(unix)]
fn parallel_reopen_rejects_changed_copied_bytes_even_after_the_original_is_restored() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let original = vec![b'A'; 3 * 64 * 1024];
    let serial = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let start = Barrier::new(4);
    let changed = AtomicBool::new(false);
    let restored = AtomicBool::new(false);
    let observer = |path: &Path, bytes| {
        if path.extension().is_some_and(|extension| extension == "ts") && bytes == 64 * 1024 {
            start.wait();
        }
        if path != Path::new("variant/segment-3.ts") {
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
            changed.store(true, Ordering::SeqCst);
        } else if bytes == 128 * 1024 {
            std::fs::write(root.path().join(path), &original).unwrap();
            restored.store(true, Ordering::SeqCst);
        }
    };
    let stop = Stop::new();
    let error = capture::from_inventory(
        serial.inventory(),
        &stop,
        Execution::for_test(Some(4), 4, 512 * MIB),
        &observer,
    )
    .unwrap_err();
    assert_eq!(error, CHANGED);
    assert!(changed.load(Ordering::SeqCst) && restored.load(Ordering::SeqCst));
    assert!(!stop.is_set());
    assert_eq!(
        std::fs::read(root.path().join("variant/segment-3.ts")).unwrap(),
        original
    );
    serial.check_original().unwrap();
}

#[test]
fn cancellation_after_real_parallel_copy_chunks_drains_and_preserves_originals() {
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let serial = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let start = Barrier::new(4);
    let stop = Stop::new();
    let entered = Mutex::new(std::collections::HashSet::new());
    let observer = |path: &Path, bytes| {
        if path.extension().is_some_and(|extension| extension == "ts") && bytes == 64 * 1024 {
            entered.lock().unwrap().insert(std::thread::current().id());
            start.wait();
            stop.set();
        }
    };
    let error = capture::capture(
        &input,
        &stop,
        Execution::for_test(Some(4), 4, 512 * MIB),
        &observer,
    )
    .unwrap_err();
    assert_eq!(error, ConversionError("cancelled"));
    assert_eq!(entered.lock().unwrap().len(), 4);
    serial.check_original().unwrap();
    for file in &serial.inventory().files {
        assert!(root.path().join(&file.path).is_file());
    }
}

#[test]
fn parallel_live_guard_rejects_a_same_bytes_replacement_and_keeps_the_command_usable() {
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let stop = Stop::new();
    let execution = Execution::for_test(Some(4), 4, 512 * MIB);
    let captured = capture::capture(&input, &stop, execution.clone(), &|_, _| {}).unwrap();
    let path = root.path().join("variant/segment-3.ts");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::rename(&path, root.path().join("retained.ts")).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(captured.check_original().unwrap_err(), CHANGED);
    assert!(!stop.is_set());
    capture::from_inventory(captured.inventory(), &stop, execution, &|_, _| {}).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("retained.ts")).unwrap(),
        bytes
    );
}

#[test]
fn insufficient_snapshot_memory_is_refused_before_any_metadata_or_media_chunk() {
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let observed = AtomicBool::new(false);
    let stop = Stop::new();
    let error = capture::capture(
        &input,
        &stop,
        Execution::for_test(Some(4), 4, 1),
        &|_, _| {
            observed.store(true, Ordering::SeqCst);
        },
    )
    .unwrap_err();
    assert_eq!(error, ConversionError("resource-budget"));
    assert!(!observed.load(Ordering::SeqCst));
    assert!(!stop.is_set());
    FrozenSource::capture(&input, &stop).unwrap();
}

#[test]
#[cfg(unix)]
fn a_parent_symlink_inserted_during_parallel_copy_is_refused_after_every_worker_drains() {
    let root = tempfile::tempdir().unwrap();
    let input = presentation(root.path());
    let start = Barrier::new(4);
    let replaced = AtomicBool::new(false);
    let observer = |path: &Path, bytes| {
        if path.extension().is_some_and(|extension| extension == "ts") && bytes == 64 * 1024 {
            start.wait();
            if path == Path::new("variant/segment-3.ts") {
                std::fs::rename(root.path().join("variant"), root.path().join("retained")).unwrap();
                std::os::unix::fs::symlink(
                    root.path().join("retained"),
                    root.path().join("variant"),
                )
                .unwrap();
                replaced.store(true, Ordering::SeqCst);
            }
        }
    };
    let stop = Stop::new();
    let error = capture::capture(
        &input,
        &stop,
        Execution::for_test(Some(4), 4, 512 * MIB),
        &observer,
    )
    .unwrap_err();
    assert_eq!(error, CHANGED);
    assert!(replaced.load(Ordering::SeqCst));
    assert!(!stop.is_set());
    for index in 0..4 {
        assert_eq!(
            std::fs::read(root.path().join(format!("retained/segment-{index}.ts"))).unwrap(),
            vec![b'A'; 3 * 64 * 1024]
        );
    }
}
