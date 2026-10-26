use super::*;
use crate::{
    concurrency::Stop,
    events::{Collector, NoSink, Record, Sink},
    resources::Limiter,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
mod sizing_tests;

fn converter(limiter: Arc<Limiter>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(limiter)),
        "unused-packager",
        "unused-ffmpeg",
    )
    .with_timeout(Duration::from_secs(10))
}
fn media() -> &'static str {
    "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"absent-init.mp4\"\n#EXTINF:2.0,\nabsent-segment.m4s\n#EXT-X-ENDLIST\n"
}
fn input(root: &Path) -> std::path::PathBuf {
    let input = root.join("media.m3u8");
    std::fs::write(&input, media()).unwrap();
    input
}

#[test]
fn captured_raw_manifest_identity_and_bytes_remain_bound_after_native_checks() {
    let root = crate::native::private_tempdir().unwrap();
    let input = input(root.path());
    let converter = converter(Arc::new(Limiter::new(2, 512 * 1024 * 1024)));
    let checked = converter
        .checked_manifest(&input, OutputFormat::Hls)
        .unwrap();
    assert!(checked.report.is_clean_for_scope());
    checked.check(&converter).unwrap();
    let changed = media().replace("VERSION:6", "VERSION:5");
    std::fs::write(&input, changed).unwrap();
    assert_eq!(checked.check(&converter).unwrap_err(), CHANGED);
    std::fs::write(&input, media()).unwrap();
    checked.check(&converter).unwrap();
    let replacement = root.path().join("replacement.m3u8");
    std::fs::write(&replacement, media()).unwrap();
    std::fs::remove_file(&input).unwrap();
    std::fs::rename(replacement, &input).unwrap();
    assert_eq!(checked.check(&converter).unwrap_err(), CHANGED);
}

#[test]
fn missing_and_unsafe_child_playlists_are_incomplete_without_reading_them() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("master.m3u8");
    let converter = converter(Arc::new(Limiter::new(2, 512 * 1024 * 1024)));
    for child in [
        "absent.m3u8",
        "../outside.m3u8",
        "https://confidential-native-sentinel.invalid/child.m3u8",
    ] {
        std::fs::write(
            &input,
            format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000\n{child}\n"),
        )
        .unwrap();
        let report = converter.lint_manifest(&input, OutputFormat::Hls).unwrap();
        assert!(!report.is_clean_for_scope());
        assert!(report.incomplete);
        assert_eq!(report.document_count, 1);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("confidential-native-sentinel")
        );
    }
}

#[test]
fn manifest_admission_uses_the_same_resource_pool_and_preserves_cancel_codes() {
    struct Started(mpsc::SyncSender<()>);
    impl Sink for Started {
        fn emit(&self, record: &Record) {
            if record["event"] == "activity"
                && record["activity"] == "verification"
                && record["state"] == "started"
            {
                let _ = self.0.try_send(());
            }
        }
    }
    let root = crate::native::private_tempdir().unwrap();
    let input = input(root.path());
    let limiter = Arc::new(Limiter::new(1, 512 * 1024 * 1024));
    let held = limiter
        .acquire(
            Claim {
                cpu_threads: 1,
                memory_bytes: 0,
            },
            None,
            None,
        )
        .unwrap();
    let blocked = self::converter(limiter.clone()).with_timeout(Duration::from_millis(30));
    assert_eq!(
        blocked
            .lint_manifest(&input, OutputFormat::Hls)
            .unwrap_err(),
        ConversionError("resource-budget")
    );
    let stop = Stop::new();
    let (sent, received) = mpsc::sync_channel(1);
    let converter = converter(limiter.clone())
        .with_stop(stop.clone())
        .with_activity_sink(Arc::new(Started(sent)));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| converter.lint_manifest(&input, OutputFormat::Hls));
        received.recv_timeout(Duration::from_secs(10)).unwrap();
        stop.set();
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            ConversionError("cancelled")
        );
    });
    drop(held);
    let report = self::converter(limiter)
        .lint_manifest(&input, OutputFormat::Hls)
        .unwrap();
    assert!(report.is_clean_for_scope());
    assert_eq!(report.workers_used, 1);
}

#[test]
fn insufficient_memory_is_not_reported_as_a_clean_or_incomplete_manifest() {
    let root = crate::native::private_tempdir().unwrap();
    let input = input(root.path());
    let small = converter(Arc::new(Limiter::new(1, 64 * 1024)));
    assert_eq!(
        small.lint_manifest(&input, OutputFormat::Hls).unwrap_err(),
        ConversionError("resource-budget")
    );
    let stopped = Stop::new();
    stopped.set();
    assert_eq!(
        small
            .with_stop(stopped)
            .lint_manifest(Path::new("not-read.m3u8"), OutputFormat::Hls)
            .unwrap_err(),
        ConversionError("cancelled")
    );
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_supply_the_entrypoint_or_child_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    let actual = input(root.path());
    let linked = root.path().join("linked.m3u8");
    std::os::unix::fs::symlink(&actual, &linked).unwrap();
    let converter = converter(Arc::new(Limiter::new(1, 512 * 1024 * 1024)));
    assert_eq!(
        converter
            .lint_manifest(&linked, OutputFormat::Hls)
            .unwrap_err()
            .0,
        "manifest-unreadable"
    );
    let master = root.path().join("master.m3u8");
    std::fs::write(
        &master,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nlinked.m3u8\n",
    )
    .unwrap();
    let report = converter.lint_manifest(&master, OutputFormat::Hls).unwrap();
    assert!(report.incomplete);
    assert_eq!(report.document_count, 1);
}

#[test]
fn native_reports_and_activity_do_not_expose_input_bytes_or_paths() {
    let root = crate::native::private_tempdir().unwrap();
    let input = input(root.path());
    let sink = Arc::new(Collector::default());
    let converter =
        converter(Arc::new(Limiter::new(1, 512 * 1024 * 1024))).with_activity_sink(sink.clone());
    let source = media().replace(
        "absent-init.mp4",
        "https://confidential-native-sentinel.invalid/init",
    );
    std::fs::write(&input, source).unwrap();
    let report = converter.lint_manifest(&input, OutputFormat::Hls).unwrap();
    assert!(report.is_clean_for_scope());
    for text in [
        serde_json::to_string(&report).unwrap(),
        serde_json::to_string(&sink.records()).unwrap(),
    ] {
        assert!(!text.contains("confidential-native-sentinel"));
        assert!(!text.contains(root.path().to_str().unwrap()));
    }
}

#[test]
fn session_key_identity_uses_each_captured_playlist_directory_without_fetching_keys() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("master.m3u8");
    std::fs::create_dir(root.path().join("nested")).unwrap();
    let child = root.path().join("nested/media.m3u8");
    let master = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"keys/confidential-native-sentinel.bin\"\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nnested/media.m3u8\n";
    let media = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"../keys/confidential-native-sentinel.bin\"\n#EXTINF:2,\nmissing.ts\n#EXT-X-ENDLIST\n";
    std::fs::write(&input, master).unwrap();
    std::fs::write(&child, media).unwrap();
    let converter = converter(Arc::new(Limiter::new(2, 512 * 1024 * 1024)));
    let report = converter.lint_manifest(&input, OutputFormat::Hls).unwrap();
    assert!(
        report.is_clean_for_scope(),
        "a canonical relative key match needs no key file"
    );
    assert_eq!(report.document_count, 2);
    std::fs::write(&child, media.replace("METHOD=AES-128", "METHOD=SAMPLE-AES")).unwrap();
    let report = converter.lint_manifest(&input, OutputFormat::Hls).unwrap();
    assert!(!report.is_clean_for_scope());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|issue| issue.rule_id == "HLS-SESSION-KEY-MATCH")
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("confidential-native-sentinel")
    );
    assert!(!root.path().join("keys").exists());
    assert_eq!(std::fs::read_to_string(&input).unwrap(), master);
}
