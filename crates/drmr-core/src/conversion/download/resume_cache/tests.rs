use super::*;

const URL: &str = "https://example.invalid/private/master.m3u8?signature=secret";

fn policy() -> FetchPolicy {
    FetchPolicy::default()
}

#[test]
fn cancelled_open_refuses_before_creating_a_cache() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let stop = Stop::new();
    stop.set();
    let error = ResumeCache::open(&download, URL, None, &policy(), &stop).unwrap_err();
    assert_eq!(error.0, "cancelled");
    assert!(!download.exists());
    assert!(!root.path().join(".source.drmr-download").exists());
}

fn source(root: &Path, bytes: &[u8]) -> (PathBuf, FileIdentity, source::Fetched) {
    let path = root.join("fresh-original");
    let mut file = crate::native::create_private_new(&path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
    let identity = FileIdentity::read(&file).unwrap();
    drop(file);
    (
        path,
        identity,
        source::Fetched {
            effective: URL.into(),
            sha256: fingerprint(bytes),
            size: bytes.len() as u64,
        },
    )
}

#[test]
fn completed_original_survives_retry_and_materializes_independent_private_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let (original, identity, fetched) = source(root.path(), b"complete encrypted resource");
    let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    cache
        .store_file(
            CompletedResource {
                requested: URL,
                range: None,
                kind: ResourceKind::Mp4,
                fetched: &fetched,
                etag: Some("\"stable\""),
                path: &original,
                created: identity,
            },
            &Stop::new(),
        )
        .unwrap();
    let cache_path = cache.directory().to_path_buf();
    drop(cache);
    let mut retry = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    let candidate = retry.lookup(URL, None).unwrap().unwrap();
    assert_eq!(candidate.kind, ResourceKind::Mp4);
    assert_eq!(candidate.size, 27);
    let destination = root.path().join("materialized");
    let created = retry
        .materialize(&candidate, &destination, &Stop::new())
        .unwrap();
    assert!(created != identity);
    assert_eq!(
        std::fs::read(&destination).unwrap(),
        b"complete encrypted resource"
    );
    retry.cleanup(&Stop::new()).unwrap();
    assert!(!cache_path.exists());
    assert_eq!(
        std::fs::read(&original).unwrap(),
        b"complete encrypted resource"
    );
}

#[test]
fn another_live_owner_is_refused_and_retry_can_choose_a_new_output() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let cache = ResumeCache::open(
        &download,
        URL,
        Some(&root.path().join("result-a")),
        &policy(),
        &Stop::new(),
    )
    .unwrap();
    assert!(ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).is_err());
    drop(cache);
    let mut retry = ResumeCache::open(
        &download,
        URL,
        Some(&root.path().join("result-b")),
        &policy(),
        &Stop::new(),
    )
    .unwrap();
    retry.cleanup(&Stop::new()).unwrap();
}

#[test]
fn changed_source_binding_is_refused_without_changing_the_cache() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    let path = cache.directory().to_path_buf();
    drop(cache);
    assert!(
        ResumeCache::open(
            &download,
            "https://example.invalid/other",
            None,
            &policy(),
            &Stop::new()
        )
        .is_err()
    );
    assert!(path.is_dir());
}

#[test]
fn missing_markers_are_never_repaired() {
    let root = crate::native::private_tempdir().unwrap();
    for marker in ["context.json", "lock"] {
        let download = root.path().join(marker.replace('.', "-"));
        let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
        let path = cache.directory().to_path_buf();
        drop(cache);
        std::fs::remove_file(path.join(marker)).unwrap();
        assert!(ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).is_err());
        assert!(!path.join(marker).exists());
    }
}

#[test]
fn changed_bytes_and_replaced_identical_objects_are_refused() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let (original, identity, fetched) = source(root.path(), b"complete bytes");
    let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    cache
        .store_file(
            CompletedResource {
                requested: URL,
                range: None,
                kind: ResourceKind::Media,
                fetched: &fetched,
                etag: None,
                path: &original,
                created: identity,
            },
            &Stop::new(),
        )
        .unwrap();
    let candidate = cache.lookup(URL, None).unwrap().unwrap();
    let blob = candidate.path.join("data");
    let mut writer = crate::native::create_private_update(&blob).unwrap();
    writer.write_all(b"modified bytes").unwrap();
    drop(writer);
    assert!(
        cache
            .materialize(&candidate, &root.path().join("copy"), &Stop::new())
            .is_err()
    );
    assert!(!root.path().join("copy").exists());
    std::fs::remove_file(&blob).unwrap();
    crate::native::create_private_new(&blob)
        .unwrap()
        .write_all(b"complete bytes")
        .unwrap();
    assert!(cache.lookup(URL, None).is_err());
}

#[test]
fn store_binds_the_fetched_digest_and_never_replaces_a_changed_representation() {
    fn completed<'a>(
        path: &'a Path,
        fetched: &'a source::Fetched,
        created: FileIdentity,
    ) -> CompletedResource<'a> {
        CompletedResource {
            requested: URL,
            range: None,
            kind: ResourceKind::Media,
            fetched,
            etag: Some("\"original\""),
            path,
            created,
        }
    }
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let (original, identity, mut fetched) = source(root.path(), b"complete bytes");
    let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    let correct_hash = fetched.sha256.clone();
    fetched.sha256 = "0".repeat(64);
    assert!(
        cache
            .store_file(completed(&original, &fetched, identity), &Stop::new())
            .is_err()
    );
    assert!(cache.lookup(URL, None).unwrap().is_none());
    fetched.sha256 = correct_hash;
    cache
        .store_file(completed(&original, &fetched, identity), &Stop::new())
        .unwrap();
    let previous = cache.lookup(URL, None).unwrap().unwrap();
    fetched.effective = "https://example.invalid/changed".into();
    assert!(
        cache
            .store_file(completed(&original, &fetched, identity), &Stop::new())
            .is_err()
    );
    let current = cache.lookup(URL, None).unwrap().unwrap();
    assert_eq!(previous.sha256, current.sha256);
    assert_eq!(previous.effective_url_sha256, current.effective_url_sha256);
    assert_eq!(previous.etag, current.etag);
}

#[test]
fn no_clobber_and_foreign_entries_survive_cleanup_refusal() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let mut cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    cache
        .store_manifest(URL, URL, b"#EXTM3U\n#EXT-X-ENDLIST\n", &Stop::new())
        .unwrap();
    let candidate = cache.lookup(URL, None).unwrap().unwrap();
    let destination = root.path().join("foreign");
    std::fs::write(&destination, b"foreign sentinel").unwrap();
    assert!(
        cache
            .materialize(&candidate, &destination, &Stop::new())
            .is_err()
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"foreign sentinel");
    let sentinel = cache.directory().join("foreign");
    std::fs::write(&sentinel, b"foreign sentinel").unwrap();
    assert!(cache.cleanup(&Stop::new()).is_err());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"foreign sentinel");
}

#[test]
fn receipt_omits_source_url_and_request_headers() {
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let mut policy = policy();
    policy.headers_by_origin.push((
        "https://example.invalid".into(),
        vec![("Authorization".into(), "Bearer secret-header".into())],
    ));
    let cache = ResumeCache::open(&download, URL, None, &policy, &Stop::new()).unwrap();
    cache
        .store_manifest(URL, URL, b"#EXTM3U\n", &Stop::new())
        .unwrap();
    let candidate = cache.lookup(URL, None).unwrap().unwrap();
    for path in [
        cache.directory().join("context.json"),
        candidate.path.join("receipt.json"),
    ] {
        let text = std::fs::read_to_string(path).unwrap();
        for secret in [
            URL,
            "signature",
            "secret-header",
            "Authorization",
            "https://",
        ] {
            assert!(!text.contains(secret));
        }
    }
}

#[cfg(unix)]
#[test]
fn insecure_roots_and_linked_or_insecure_blobs_are_refused_without_repair() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = crate::native::private_tempdir().unwrap();
    let download = root.path().join("source");
    let cache = ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).unwrap();
    let path = cache.directory().to_path_buf();
    drop(cache);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(ResumeCache::open(&download, URL, None, &policy(), &Stop::new()).is_err());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
