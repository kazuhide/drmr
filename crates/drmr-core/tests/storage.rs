//! Local and S3 storage semantics with isolated temporary stores and HTTP fixtures.

mod common;

use common::fake_s3::{FakeS3, Fault};
use common::{names_with_prefix, write};
use drmr_core::storage::{
    LocalStorageBackend, ObjectRef, S3StorageBackend, StorageBackend, StorageError, TransferPolicy,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn refs(keys: &[&str]) -> Vec<ObjectRef> {
    keys.iter()
        .map(|key| ObjectRef::new(*key).unwrap())
        .collect()
}

fn quick(max_attempts: i64) -> TransferPolicy {
    TransferPolicy {
        max_attempts,
        backoff_base_s: 0.0,
        ..TransferPolicy::default()
    }
}

fn s3(
    fake: &FakeS3,
    bucket: &str,
    policy: TransferPolicy,
) -> Result<S3StorageBackend, StorageError> {
    S3StorageBackend::with_access(bucket, "ap-northeast-1", policy, "output", &fake.access())
}

fn tokyo(policy: TransferPolicy) -> (FakeS3, S3StorageBackend) {
    let fake = FakeS3::start();
    fake.bucket("tokyo-batch-test", "ap-northeast-1");
    let backend = s3(&fake, "tokyo-batch-test", policy).unwrap();
    (fake, backend)
}

// ---------------------------------------------------------------------------------------
// The cases of the fixture cases.
// ---------------------------------------------------------------------------------------

#[test]
fn test_local_roundtrip_atomic_and_input_protection() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    write(&store.join("input/source.bin"), b"original");
    let backend = LocalStorageBackend::new(&store).unwrap();
    let staged = backend
        .get_many(&refs(&["input/source.bin"]), &temp.path().join("stage"))
        .unwrap();
    assert_eq!(staged.len(), 1);
    assert_eq!(std::fs::read(&staged[0]).unwrap(), b"original");
    assert!(!staged[0].is_symlink());
    let item = (
        staged[0].clone(),
        ObjectRef::new("output/a/data.bin").unwrap(),
    );
    backend.put_many(std::slice::from_ref(&item)).unwrap();
    backend.put_many(std::slice::from_ref(&item)).unwrap();
    assert_eq!(
        std::fs::read(store.join("output/a/data.bin")).unwrap(),
        b"original"
    );
    let refused = backend
        .delete_prefix(&ObjectRef::new("input").unwrap())
        .unwrap_err();
    assert_eq!(refused.token(), "output-prefix-invalid");
    backend
        .delete_prefix(&ObjectRef::new("output/a").unwrap())
        .unwrap();
    assert!(!store.join("output/a").exists());
    assert_eq!(
        std::fs::read(store.join("input/source.bin")).unwrap(),
        b"original"
    );
    assert_eq!(
        names_with_prefix(temp.path(), ".transfer-"),
        Vec::<String>::new()
    );
}

#[test]
fn test_object_keys_are_confined() {
    for key in ["", "/abs", "../secret", "a/../b", "a//b", "a\\b"] {
        let error = ObjectRef::new(key).unwrap_err();
        assert_eq!(error.token(), "object-key-invalid", "{key:?}");
    }
    for key in ["a\u{1}b", "a?b", "a#b", "a/./b", "a/"] {
        assert!(ObjectRef::new(key).is_err(), "{key:?}");
    }
    assert!(ObjectRef::new("host/a/seg_$Number$.m4s").is_ok());
    assert_eq!(
        format!("{:?}", ObjectRef::new("secret/location").unwrap()),
        "ObjectRef()"
    );
}

#[test]
fn test_region_rejected_before_client_access() {
    let error = S3StorageBackend::new("bucket", "us-east-1").unwrap_err();
    assert_eq!(error.token(), "storage-region-invalid");
}

#[test]
fn test_transfer_budget_validated() {
    let policy = TransferPolicy {
        batch_size: 1001,
        ..TransferPolicy::default()
    };
    assert_eq!(
        policy.validate().unwrap_err().token(),
        "transfer-policy-invalid"
    );
    for (batch, concurrency, attempts) in [
        (0, 8, 4),
        (1000, 0, 4),
        (1000, 33, 4),
        (1000, 8, 0),
        (1000, 8, 11),
    ] {
        let policy = TransferPolicy {
            batch_size: batch,
            max_concurrency: concurrency,
            max_attempts: attempts,
            ..TransferPolicy::default()
        };
        assert!(policy.validate().is_err());
    }
    for budget in [0, -1] {
        assert!(
            TransferPolicy {
                max_objects: budget,
                ..TransferPolicy::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TransferPolicy {
                max_object_bytes: budget,
                ..TransferPolicy::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TransferPolicy {
                max_asset_bytes: budget,
                ..TransferPolicy::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(TransferPolicy::default().validate().is_ok());
}

#[test]
fn test_transfer_time_values_must_be_finite() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let policies = [
            TransferPolicy {
                per_object_timeout_s: value,
                ..TransferPolicy::default()
            },
            TransferPolicy {
                backoff_base_s: value,
                ..TransferPolicy::default()
            },
            TransferPolicy {
                backoff_max_s: value,
                ..TransferPolicy::default()
            },
        ];
        for policy in policies {
            assert_eq!(
                policy.validate().unwrap_err().token(),
                "transfer-policy-invalid"
            );
        }
    }
    assert!(
        TransferPolicy {
            per_object_timeout_s: 0.0,
            ..TransferPolicy::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        TransferPolicy {
            backoff_base_s: -0.1,
            ..TransferPolicy::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn test_local_symlinks_and_overlapping_stage_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    std::fs::create_dir(&root).unwrap();
    write(&temp.path().join("secret"), b"private");
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.path().join("secret"), root.join("alias")).unwrap();
    #[cfg(windows)]
    {
        write(&temp.path().join("secret-directory/data"), b"private");
        common::windows::junction(&root.join("alias"), &temp.path().join("secret-directory"));
    }
    let alias = if cfg!(windows) { "alias/data" } else { "alias" };
    let backend = LocalStorageBackend::new(&root).unwrap();
    let error = backend
        .get_many(&refs(&[alias]), &temp.path().join("stage"))
        .unwrap_err();
    assert_eq!(error.token(), "storage-path-invalid");
    write(&root.join("plain"), b"plain");
    let error = backend.get_many(&refs(&["plain"]), &root).unwrap_err();
    assert_eq!(error.token(), "storage-stage-overlap");
    let error = backend
        .get_many(&refs(&["plain"]), &root.join("nested"))
        .unwrap_err();
    assert_eq!(error.token(), "storage-stage-overlap");
}

#[test]
fn test_moto_s3_region_roundtrip_and_owned_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let (fake, backend) = tokyo(TransferPolicy::default());
    fake.put("tokyo-batch-test", "input/original", b"source-bytes");
    let staged = backend
        .get_many(&refs(&["input/original"]), &temp.path().join("stage"))
        .unwrap();
    assert_eq!(std::fs::read(&staged[0]).unwrap(), b"source-bytes");
    backend
        .put_many(&[(
            staged[0].clone(),
            ObjectRef::new("output/a/original").unwrap(),
        )])
        .unwrap();
    assert!(
        backend
            .exists(&ObjectRef::new("output/a/original").unwrap())
            .unwrap()
    );
    assert!(
        !backend
            .exists(&ObjectRef::new("output/a/missing").unwrap())
            .unwrap()
    );
    assert_eq!(
        backend
            .list_prefix(&ObjectRef::new("output/a").unwrap())
            .unwrap()
            .len(),
        1
    );
    backend
        .delete_prefix(&ObjectRef::new("output/a").unwrap())
        .unwrap();
    assert_eq!(
        backend
            .list_prefix(&ObjectRef::new("output/a").unwrap())
            .unwrap(),
        Vec::<ObjectRef>::new()
    );
    assert_eq!(
        fake.object("tokyo-batch-test", "input/original").unwrap(),
        b"source-bytes"
    );
    fake.bucket("west-batch-test", "us-west-2");
    let error = s3(&fake, "west-batch-test", TransferPolicy::default()).unwrap_err();
    assert_eq!(error.token(), "storage-region-invalid");
}

#[test]
fn test_s3_bounded_retry_sanitizes_provider_error() {
    let (fake, backend) = tokyo(quick(3));
    fake.on(|request| (request.method == "HEAD").then_some(Fault::Status(503, "SlowDown")));
    let error = backend
        .exists(&ObjectRef::new("input/test").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-transient");
    assert!(error.is_retryable());
    assert_eq!(fake.count("HEAD"), 3);
    assert!(!format!("{error} {error:?}").contains("private"));
}

#[test]
fn test_local_missing_source_error_never_carries_object_key() {
    let temp = tempfile::tempdir().unwrap();
    let backend = LocalStorageBackend::new(&temp.path().join("store")).unwrap();
    let error = backend
        .get_many(
            &refs(&["sentinel-private-file"]),
            &temp.path().join("stage"),
        )
        .unwrap_err();
    assert_eq!(error.token(), "storage-read-failed");
    assert!(!format!("{error} {error:?}").contains("sentinel"));
}

#[test]
fn test_real_sdk_transport_errors_are_bounded_retryable() {
    // ReadTimeoutError, ConnectionClosedError, IncompleteReadError, ResponseStreamingError: the
    // transport failures botocore raises, produced here by a real connection.
    let faults = [
        Fault::Delay(Duration::from_secs(2)),
        Fault::Close,
        Fault::PartialHeaders,
        Fault::Truncate {
            declared: 10,
            body: b"x".to_vec(),
        },
    ];
    for fault in faults {
        let policy = TransferPolicy {
            per_object_timeout_s: 0.3,
            ..quick(2)
        };
        let (fake, backend) = tokyo(policy);
        fake.put("tokyo-batch-test", "input/source", b"0123456789");
        let injected = fault.clone();
        fake.on(move |request| (request.method == "GET").then(|| injected.clone()));
        let started = Instant::now();
        let error = backend
            .get_many(
                &refs(&["input/source"]),
                &tempfile::tempdir().unwrap().path().join("s"),
            )
            .unwrap_err();
        assert!(error.is_retryable(), "{fault:?}");
        assert_eq!(error.token(), "storage-transient", "{fault:?}");
        assert_eq!(fake.count_object("GET"), 2, "{fault:?}");
        assert!(
            started.elapsed() < Duration::from_millis(1900),
            "{fault:?} took {:?}",
            started.elapsed()
        );
        assert!(!format!("{error:?}").contains("sentinel"));
    }
}

#[test]
fn test_object_size_transport_errors_are_bounded_retryable() {
    for fault in [
        Fault::Close,
        Fault::PartialHeaders,
        Fault::Delay(Duration::from_secs(2)),
    ] {
        let policy = TransferPolicy {
            per_object_timeout_s: 0.3,
            ..quick(2)
        };
        let (fake, backend) = tokyo(policy);
        fake.put("tokyo-batch-test", "input/source", b"data");
        let injected = fault.clone();
        fake.on(move |request| (request.method == "HEAD").then(|| injected.clone()));
        let error = backend
            .object_size(&ObjectRef::new("input/source").unwrap())
            .unwrap_err();
        assert!(error.is_retryable(), "{fault:?}");
        assert_eq!(fake.count("HEAD"), 2, "{fault:?}");
    }
}

#[test]
fn test_actual_sdk_streamingbody_truncation_retries_and_leaves_no_object() {
    let temp = tempfile::tempdir().unwrap();
    let (fake, backend) = tokyo(quick(2));
    fake.put("tokyo-batch-test", "input/source", b"0123456789");
    fake.on(|request| {
        (request.method == "GET").then(|| Fault::Truncate {
            declared: 10,
            body: b"x".to_vec(),
        })
    });
    let error = backend
        .get_many(&refs(&["input/source"]), temp.path())
        .unwrap_err();
    assert!(error.is_retryable());
    assert_eq!(fake.count_object("GET"), 2);
    assert!(!temp.path().join("input/source").exists());
    assert_eq!(
        names_with_prefix(temp.path(), ".transfer-"),
        Vec::<String>::new()
    );
}

// ---------------------------------------------------------------------------------------
// `storage` behaviour beyond the Python cases: timeouts, classification, pagination,
// batching and budgets.
// ---------------------------------------------------------------------------------------

#[test]
fn a_read_timeout_bounds_every_request() {
    // botocore's read_timeout = per_object_timeout_s: a stalled response is abandoned.
    let policy = TransferPolicy {
        per_object_timeout_s: 0.2,
        ..quick(1)
    };
    let (fake, backend) = tokyo(policy);
    fake.put("tokyo-batch-test", "input/slow", b"data");
    fake.on(|request| {
        (request.method == "HEAD").then_some(Fault::Delay(Duration::from_millis(1500)))
    });
    let started = Instant::now();
    let error = backend
        .object_size(&ObjectRef::new("input/slow").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-transient");
    assert!(
        started.elapsed() < Duration::from_millis(1200),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn progressing_response_can_outlast_the_read_idle_timeout() {
    let policy = TransferPolicy {
        per_object_timeout_s: 0.3,
        ..quick(1)
    };
    let (fake, backend) = tokyo(policy);
    fake.put("tokyo-batch-test", "input/progress", b"abcdefgh");
    fake.on(|request| {
        (request.method == "GET").then(|| Fault::SlowBody {
            pause: Duration::from_millis(100),
            body: b"abcdefgh".to_vec(),
        })
    });
    let temp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let staged = backend
        .get_many(&refs(&["input/progress"]), temp.path())
        .unwrap();
    assert_eq!(std::fs::read(&staged[0]).unwrap(), b"abcdefgh");
    assert!(started.elapsed() >= Duration::from_millis(600));
    assert_eq!(fake.count_object("GET"), 1);
}

#[test]
fn signed_requests_never_follow_redirects_or_expose_their_location() {
    let (fake, backend) = tokyo(quick(3));
    fake.put("tokyo-batch-test", "input/source", b"original");
    fake.put("tokyo-batch-test", "input/redirected", b"redirected");
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nLocation: {}/tokyo-batch-test/input/redirected?sentinel-private-token\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        fake.endpoint
    )
    .into_bytes();
    fake.on(move |request| (request.method == "GET").then(|| Fault::Raw(response.clone())));
    let error = backend
        .get_many(
            &refs(&["input/source"]),
            tempfile::tempdir().unwrap().path(),
        )
        .unwrap_err();
    assert_eq!(error.token(), "storage-failed");
    assert!(!error.is_retryable());
    assert_eq!(fake.count_object("GET"), 1);
    assert!(!format!("{error} {error:?}").contains("sentinel"));
    assert!(
        fake.requests()
            .iter()
            .all(|request| request.key != "input/redirected")
    );
}

#[test]
fn malformed_responses_and_missing_lengths_never_publish_a_download() {
    let cases: &[(&[u8], bool)] = &[
        (
            b"HTTP/1.1 sentinel-private-status\r\nConnection: close\r\n\r\n",
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\npayload",
            false,
        ),
    ];
    for (response, retryable) in cases {
        let (fake, backend) = tokyo(quick(2));
        fake.put("tokyo-batch-test", "input/source", b"original");
        let response = response.to_vec();
        fake.on(move |request| (request.method == "GET").then(|| Fault::Raw(response.clone())));
        let temp = tempfile::tempdir().unwrap();
        let error = backend
            .get_many(&refs(&["input/source"]), temp.path())
            .unwrap_err();
        assert_eq!(error.is_retryable(), *retryable);
        assert_eq!(fake.count_object("GET"), if *retryable { 2 } else { 1 });
        assert!(!temp.path().join("input/source").exists());
        assert!(!format!("{error} {error:?}").contains("sentinel"));
    }
}

#[test]
fn retry_codes_beyond_the_error_body_budget_are_not_read() {
    let (fake, backend) = tokyo(quick(3));
    fake.put("tokyo-batch-test", "input/source", b"original");
    let body = format!(
        "<Error>{}<Code>Throttling</Code><Message>sentinel-private-message</Message></Error>",
        " ".repeat(64 * 1024)
    );
    let response = format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes();
    fake.on(move |request| (request.method == "GET").then(|| Fault::Raw(response.clone())));
    let temp = tempfile::tempdir().unwrap();
    let error = backend
        .get_many(&refs(&["input/source"]), temp.path())
        .unwrap_err();
    assert_eq!(error.token(), "storage-failed");
    assert!(!error.is_retryable());
    assert_eq!(fake.count_object("GET"), 1);
    assert!(!temp.path().join("input/source").exists());
    assert!(!format!("{error} {error:?}").contains("sentinel"));
}

#[test]
fn status_codes_are_classified_like_call() {
    let cases: [(u16, &str, bool); 7] = [
        (500, "InternalError", true),
        (503, "ServiceUnavailable", true),
        (502, "BadGateway", true),
        (400, "RequestTimeout", true),
        (400, "Throttling", true),
        (400, "InvalidArgument", false),
        (403, "AccessDenied", false),
    ];
    for (status, code, retryable) in cases {
        let (fake, backend) = tokyo(quick(3));
        fake.put("tokyo-batch-test", "input/x", b"data");
        fake.on(move |request| (request.method == "GET").then_some(Fault::Status(status, code)));
        let error = backend
            .get_many(
                &refs(&["input/x"]),
                &tempfile::tempdir().unwrap().path().join("s"),
            )
            .unwrap_err();
        assert_eq!(error.is_retryable(), retryable, "{status} {code}");
        let expected = if retryable {
            "storage-transient"
        } else {
            "storage-failed"
        };
        assert_eq!(error.token(), expected);
        assert_eq!(
            fake.count_object("GET"),
            if retryable { 3 } else { 1 },
            "{status} {code}"
        );
    }
}

#[test]
fn exists_is_false_only_for_not_found() {
    let (fake, backend) = tokyo(quick(3));
    assert!(
        !backend
            .exists(&ObjectRef::new("output/a/none").unwrap())
            .unwrap()
    );
    fake.on(|request| (request.method == "HEAD").then_some(Fault::Status(403, "AccessDenied")));
    let error = backend
        .exists(&ObjectRef::new("output/a/none").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-failed");
    assert_eq!(fake.count("HEAD"), 2, "the 403 is not retried");
}

#[test]
fn list_prefix_follows_continuation_tokens() {
    let policy = TransferPolicy {
        batch_size: 2,
        ..TransferPolicy::default()
    };
    let (fake, backend) = tokyo(policy);
    for index in 0..5 {
        fake.put(
            "tokyo-batch-test",
            &format!("input/title/seg {index}.ts"),
            b"x",
        );
    }
    fake.put("tokyo-batch-test", "input/title/dir/", b"");
    fake.put("tokyo-batch-test", "input/titled", b"x");
    let listed = backend
        .list_prefix(&ObjectRef::new("input/title").unwrap())
        .unwrap();
    let keys: Vec<&str> = listed.iter().map(ObjectRef::key).collect();
    assert_eq!(
        keys,
        [
            "input/title/seg 0.ts",
            "input/title/seg 1.ts",
            "input/title/seg 2.ts",
            "input/title/seg 3.ts",
            "input/title/seg 4.ts"
        ]
    );
    let lists: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.query("list-type").is_some())
        .collect();
    assert_eq!(lists.len(), 3);
    assert!(
        lists
            .iter()
            .all(|r| r.query("prefix") == Some("input/title/") && r.query("max-keys") == Some("2"))
    );
    assert_eq!(lists[1].query("continuation-token"), Some("2"));
}

#[test]
fn a_truncated_listing_without_a_token_is_invalid() {
    let (fake, backend) = tokyo(quick(1));
    fake.on(|request| {
        request.query("list-type").map(|_| {
            Fault::Xml("<ListBucketResult><IsTruncated>true</IsTruncated><Contents><Key>input/a</Key></Contents></ListBucketResult>".to_string())
        })
    });
    let error = backend
        .list_prefix(&ObjectRef::new("input").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-list-invalid");
}

#[test]
fn listings_and_transfers_are_bounded_by_the_object_budget() {
    let policy = TransferPolicy {
        max_objects: 3,
        batch_size: 2,
        ..TransferPolicy::default()
    };
    let (fake, backend) = tokyo(policy);
    for index in 0..5 {
        fake.put("tokyo-batch-test", &format!("input/{index}"), b"x");
    }
    let error = backend
        .list_prefix(&ObjectRef::new("input").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-object-budget");
    let temp = tempfile::tempdir().unwrap();
    let local = LocalStorageBackend::with(&temp.path().join("store"), policy, "output").unwrap();
    for index in 0..4 {
        write(&temp.path().join(format!("store/input/{index}")), b"x");
    }
    let keys: Vec<String> = (0..4).map(|index| format!("input/{index}")).collect();
    let all: Vec<ObjectRef> = keys
        .iter()
        .map(|key| ObjectRef::new(key.as_str()).unwrap())
        .collect();
    let error = local
        .get_many(&all, &temp.path().join("stage"))
        .unwrap_err();
    assert_eq!(error.token(), "storage-object-budget");
    assert!(
        !temp.path().join("stage/input").exists(),
        "nothing transferred over the budget"
    );
    let error = local
        .list_prefix(&ObjectRef::new("input").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-object-budget");
}

#[test]
fn delete_prefix_deletes_in_batches_and_only_owned_prefixes() {
    let policy = TransferPolicy {
        batch_size: 2,
        ..TransferPolicy::default()
    };
    let (fake, backend) = tokyo(policy);
    for index in 0..5 {
        fake.put("tokyo-batch-test", &format!("output/a/{index}"), b"x");
    }
    fake.put("tokyo-batch-test", "output/ab", b"x");
    fake.put("tokyo-batch-test", "input/a/0", b"x");
    backend
        .delete_prefix(&ObjectRef::new("output/a").unwrap())
        .unwrap();
    assert_eq!(fake.keys("tokyo-batch-test"), ["input/a/0", "output/ab"]);
    assert_eq!(fake.count("POST"), 3);
    for prefix in ["input/a", "output"] {
        let error = backend
            .delete_prefix(&ObjectRef::new(prefix).unwrap())
            .unwrap_err();
        assert_eq!(error.token(), "output-prefix-invalid");
    }
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("file");
    write(&source, b"x");
    let error = backend
        .put_many(&[(source, ObjectRef::new("input/x").unwrap())])
        .unwrap_err();
    assert_eq!(error.token(), "output-prefix-invalid");
}

#[test]
fn byte_budgets_hold_before_and_during_a_transfer() {
    let temp = tempfile::tempdir().unwrap();
    let policy = TransferPolicy {
        max_asset_bytes: 5,
        ..quick(3)
    };
    let (fake, backend) = tokyo(policy);
    fake.put("tokyo-batch-test", "input/a", b"abc");
    fake.put("tokyo-batch-test", "input/b", b"def");
    let error = backend
        .get_many(&refs(&["input/a", "input/b"]), temp.path())
        .unwrap_err();
    assert_eq!(error.token(), "storage-byte-budget");
    assert_eq!(
        fake.count_object("GET"),
        0,
        "the budget is checked from the sizes, before any download"
    );
    // An object that streams past max_object_bytes stops there and is not retried.
    let policy = TransferPolicy {
        max_object_bytes: 4,
        ..quick(3)
    };
    let (fake, backend) = tokyo(policy);
    fake.put("tokyo-batch-test", "input/c", b"abcd");
    fake.on(|request| {
        (request.method == "GET").then(|| Fault::Truncate {
            declared: 9,
            body: b"abcdefghi".to_vec(),
        })
    });
    let error = backend
        .get_many(&refs(&["input/c"]), temp.path())
        .unwrap_err();
    assert_eq!(error.token(), "storage-failed");
    assert!(!error.is_retryable());
    assert_eq!(fake.count_object("GET"), 1);
    assert_eq!(
        names_with_prefix(temp.path(), ".transfer-"),
        Vec::<String>::new()
    );
    // Local: the asset budget and the object cap.
    let store = temp.path().join("store");
    write(&store.join("input/a"), b"abc");
    write(&store.join("input/b"), b"def");
    let budget = LocalStorageBackend::with(
        &store,
        TransferPolicy {
            max_asset_bytes: 5,
            ..TransferPolicy::default()
        },
        "output",
    )
    .unwrap();
    let error = budget
        .get_many(&refs(&["input/a", "input/b"]), &temp.path().join("stage"))
        .unwrap_err();
    assert_eq!(error.token(), "storage-byte-budget");
    let capped = LocalStorageBackend::with(
        &store,
        TransferPolicy {
            max_object_bytes: 2,
            ..TransferPolicy::default()
        },
        "output",
    )
    .unwrap();
    let error = capped
        .get_many(&refs(&["input/a"]), &temp.path().join("stage"))
        .unwrap_err();
    assert_eq!(error.token(), "storage-object-invalid");
    let error = capped
        .put_many(&[(store.join("input/a"), ObjectRef::new("output/x").unwrap())])
        .unwrap_err();
    assert_eq!(error.token(), "storage-object-invalid");
}

#[test]
fn a_failed_transfer_reports_the_earliest_failure_in_item_order() {
    // Two failures of the copy phase: an object over the cap (`storage-object-invalid`) and a
    // target whose directory is a file (`storage-read-failed`). With four workers both run;
    // the error is the earlier one in item order, as `executor.map` raises it. (A link fails
    // earlier, in the size check before any copy — as `_confined` does in Python.)
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    write(&store.join("input/ok"), b"x");
    write(&store.join("input/big"), b"too large");
    write(&store.join("blocker/inner"), b"y");
    let stage = temp.path().join("stage");
    write(
        &stage.join("blocker"),
        b"a file where a directory is needed",
    );
    let policy = TransferPolicy {
        max_object_bytes: 4,
        max_concurrency: 4,
        ..TransferPolicy::default()
    };
    let backend = LocalStorageBackend::with(&store, policy, "output").unwrap();
    for _ in 0..20 {
        let order = refs(&["input/big", "input/ok", "blocker/inner", "input/ok"]);
        let error = backend.get_many(&order, &stage).unwrap_err();
        assert_eq!(error.token(), "storage-object-invalid");
        let order = refs(&["blocker/inner", "input/ok", "input/ok", "input/big"]);
        let error = backend.get_many(&order, &stage).unwrap_err();
        assert_eq!(error.token(), "storage-read-failed");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(store.join("input/ok"), store.join("input/link")).unwrap();
    #[cfg(windows)]
    {
        write(&store.join("redirected/ok"), b"x");
        common::windows::junction(&store.join("input/link"), &store.join("redirected"));
    }
    let link = if cfg!(windows) {
        "input/link/ok"
    } else {
        "input/link"
    };
    let error = backend
        .get_many(&refs(&["input/big", link]), &stage)
        .unwrap_err();
    assert_eq!(error.token(), "storage-path-invalid");
}

#[test]
fn local_listing_is_sorted_refuses_links_and_returns_a_file_prefix_itself() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store");
    for key in [
        "host/t/b/2.m4s",
        "host/t/a.m4s",
        "host/t/b/1.m4s",
        "host/other",
    ] {
        write(&store.join(key), b"x");
    }
    let backend = LocalStorageBackend::new(&store).unwrap();
    let listed = backend
        .list_prefix(&ObjectRef::new("host/t").unwrap())
        .unwrap();
    let keys: Vec<&str> = listed.iter().map(ObjectRef::key).collect();
    assert_eq!(keys, ["host/t/a.m4s", "host/t/b/1.m4s", "host/t/b/2.m4s"]);
    let single = backend
        .list_prefix(&ObjectRef::new("host/other").unwrap())
        .unwrap();
    assert_eq!(single, refs(&["host/other"]));
    assert_eq!(
        backend
            .list_prefix(&ObjectRef::new("host/missing").unwrap())
            .unwrap(),
        Vec::<ObjectRef>::new()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(store.join("host/other"), store.join("host/t/b/link")).unwrap();
    #[cfg(windows)]
    {
        std::fs::create_dir(store.join("host/other-directory")).unwrap();
        common::windows::junction(
            &store.join("host/t/b/link"),
            &store.join("host/other-directory"),
        );
    }
    let error = backend
        .list_prefix(&ObjectRef::new("host/t").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-path-invalid");
    let error = backend
        .object_size(&ObjectRef::new("host/t/b/link").unwrap())
        .unwrap_err();
    assert_eq!(error.token(), "storage-path-invalid");
    assert_eq!(
        backend
            .object_size(&ObjectRef::new("host/other").unwrap())
            .unwrap(),
        1
    );
    assert!(
        backend
            .exists(&ObjectRef::new("host/other").unwrap())
            .unwrap()
    );
    assert!(
        !backend
            .exists(&ObjectRef::new("host/none").unwrap())
            .unwrap()
    );
}

#[test]
fn debug_output_names_no_root_bucket_or_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store-sentinel");
    let local = LocalStorageBackend::new(&root).unwrap();
    assert!(!format!("{local:?}").contains("sentinel"));
    let fake = FakeS3::start();
    fake.bucket("bucket-sentinel", "ap-northeast-1");
    let remote = s3(&fake, "bucket-sentinel", TransferPolicy::default()).unwrap();
    let shown = format!("{remote:?} {:?}", fake.access());
    assert!(
        !shown.contains("sentinel")
            && !shown.contains("synthetic-secret")
            && !shown.contains("AKID"),
        "{shown}"
    );
    assert_eq!(remote.bucket(), Some("bucket-sentinel"));
    assert_eq!(
        local.root(),
        Some(
            std::fs::canonicalize(temp.path())
                .unwrap()
                .join("store-sentinel")
                .as_path()
        )
    );
    let _: PathBuf = root;
}
