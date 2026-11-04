//! Complete native renditions can be checked before the last HTTP resource arrives.
use drmr_core::{
    concurrency::Stop,
    conversion::{
        Converter, HlsInputKeys, InputKeys,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        layout::LayoutMode,
        plan::bundle::{BundleOutputIntent, ManifestValidation},
    },
    events::{Collector, NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    resources::Limiter,
    source::FetchPolicy,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn resources() -> &'static BTreeMap<String, Vec<u8>> {
    static RESOURCES: OnceLock<BTreeMap<String, Vec<u8>>> = OnceLock::new();
    RESOURCES.get_or_init(|| {
        let root = drmr_core::native::private_tempdir().unwrap();
        let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
        let mut result = BTreeMap::from([
            ("/master.m3u8".into(), b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"default-audio-group\",NAME=\"stream_0\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,URI=\"audio/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"default-audio-group\"\nvideo/media.m3u8\n".to_vec()),
        ]);
        for (kind, selector) in [("video", "0:v:0"), ("audio", "0:a:0")] {
            let directory = root.path().join(kind);
            drmr_core::native::create_private_dir_all(&directory).unwrap();
            let output = Command::new("ffmpeg").args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
                .arg(&clip).args(["-map", selector, "-c", "copy", "-hls_time", "2", "-hls_playlist_type", "vod", "-hls_segment_filename"])
                .arg(directory.join("segment-%d.ts")).arg(directory.join("media.m3u8"))
                .output().unwrap();
            assert!(output.status.success(), "fixture creation failed: {}", String::from_utf8_lossy(&output.stderr));
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                result.insert(format!("/{kind}/{}", path.file_name().unwrap().to_str().unwrap()), std::fs::read(&path).unwrap());
            }
        }
        result
    })
}

// This bounds a causal ordering handshake, not the native parser's speed.
const OVERLAP_BARRIER_WAIT: Duration = Duration::from_secs(15);

fn last_video_resource() -> &'static str {
    resources()
        .keys()
        .filter(|name| {
            name.starts_with("/video/")
                && Path::new(name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("ts"))
        })
        .max()
        .unwrap()
        .as_str()
}

struct Witness {
    held: Stop,
    prepared: Stop,
    records: Collector,
    destination: PathBuf,
    cancel: Option<Stop>,
}
impl Sink for Witness {
    fn emit(&self, record: &Record) {
        self.records.emit(record);
        if record.get("event").and_then(|v| v.as_str()) == Some("native-rendition-prepared") {
            assert!(
                ["input.m3u8", "master.m3u8"]
                    .iter()
                    .all(|name| !self.destination.join(name).exists()),
                "entrypoint published before acquisition completed"
            );
            self.prepared.set();
            if let Some(stop) = &self.cancel {
                stop.set();
            }
        }
    }
}

struct Server {
    address: std::net::SocketAddr,
    stop: Stop,
    overlap: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(witness: Arc<Witness>, barrier: bool) -> Self {
        Self::start(witness, barrier, false, false)
    }
    fn start(
        witness: Arc<Witness>,
        barrier: bool,
        mismatched_metadata: bool,
        fetch_failure: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Stop::new();
        let ending = stop.clone();
        let overlap = Arc::new(AtomicBool::new(false));
        let observed = overlap.clone();
        let worker = std::thread::spawn(move || {
            std::thread::scope(|scope| {
                for stream in listener.incoming() {
                    if ending.is_set() {
                        break;
                    }
                    let mut stream = stream.unwrap();
                    let witness = witness.clone();
                    let ending = ending.clone();
                    let observed = observed.clone();
                    scope.spawn(move || {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                return;
                            }
                            request.push(byte[0]);
                            assert!(request.len() < 8192);
                        }
                        let target = std::str::from_utf8(&request)
                            .unwrap()
                            .split_whitespace()
                            .nth(1)
                            .unwrap();
                        let mut body = resources()[target].clone();
                        if mismatched_metadata && target == "/master.m3u8" {
                            body = String::from_utf8(body)
                                .unwrap()
                                .replace("BANDWIDTH=1000000", "BANDWIDTH=1000000,RESOLUTION=4x4")
                                .into_bytes();
                        }
                        let last = target == last_video_resource();
                        if fetch_failure && last {
                            witness.held.set();
                            witness.prepared.wait(Some(OVERLAP_BARRIER_WAIT));
                            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            return;
                        }
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.flush().unwrap();
                        if target == last_video_resource()
                        {
                            witness.held.set();
                            if barrier {
                                observed.store(
                                    witness.prepared.wait(Some(OVERLAP_BARRIER_WAIT)),
                                    Ordering::Release,
                                );
                            }
                        }
                        if !ending.is_set() {
                            let _ = stream.write_all(&body);
                        }
                    });
                }
            });
        });
        Self {
            address,
            stop,
            overlap,
            worker: Some(worker),
        }
    }
    fn url(&self) -> String {
        format!("http://{}/master.m3u8", self.address)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.set();
        let _ = TcpStream::connect(self.address);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn outputs() -> Vec<BundleOutputIntent> {
    [OutputFormat::Dash, OutputFormat::Hls]
        .into_iter()
        .enumerate()
        .map(|(index, format)| BundleOutputIntent {
            id: format!("output-{index}"),
            format,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
            processing: None,
            dash_addressing: None,
            manifest_profile: None,
        })
        .collect()
}
fn converter(witness: Arc<Witness>, workers: usize, memory: u64) -> Converter {
    Converter::new(
        Arc::new(
            ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
                .with_limiter(Arc::new(Limiter::new(4, memory))),
        ),
        "packager",
        "must-not-start-decoder",
    )
    .with_activity_sink(witness)
    .with_max_workers(Some(workers))
    .unwrap()
}
fn witness(source: &Path) -> Arc<Witness> {
    Arc::new(Witness {
        held: Stop::new(),
        prepared: Stop::new(),
        records: Collector::default(),
        destination: source.into(),
        cancel: None,
    })
}
fn policy(url: &str) -> FetchPolicy {
    FetchPolicy {
        allowed_origins: vec![drmr_core::source::origin(url).unwrap()],
        allow_http: true,
        allow_private_network: true,
        ..FetchPolicy::default()
    }
}

#[test]
fn native_admission_finishes_before_last_http_body_and_is_reused_by_bundle_conversion() {
    for layout in [LayoutMode::Normalized, LayoutMode::Source] {
        native_admission_roundtrip(layout);
    }
}

fn native_admission_roundtrip(layout: LayoutMode) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let witness = witness(&source);
    let server = Server::new(witness.clone(), true);
    let command = converter(witness.clone(), 2, 4 * 1024 * 1024 * 1024).with_layout(layout);
    let outputs = outputs();
    let downloaded = command
        .download_bundle_source(
            &server.url(),
            &source,
            &output,
            &policy(&server.url()),
            &outputs,
        )
        .unwrap();
    assert_eq!(
        downloaded.entrypoint_relative(),
        Path::new(if layout == LayoutMode::Source {
            "master.m3u8"
        } else {
            "input.m3u8"
        })
    );
    assert!(
        server.overlap.load(Ordering::Acquire),
        "native preparation waited for the last HTTP resource"
    );
    assert_eq!(
        witness
            .records
            .records()
            .iter()
            .filter(|r| r["event"] == "native-rendition-prepared")
            .count(),
        2
    );
    let keys = BundleOperatorKeys {
        input: InputKeys::default(),
        hls_input: HlsInputKeys::default(),
        outputs: BTreeMap::new(),
    };
    let report = command
        .convert_bundle_downloaded(
            &downloaded,
            &outputs,
            &output,
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert!(
        report
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed)
    );
    assert_eq!(
        witness
            .records
            .records()
            .iter()
            .filter(|r| r["event"] == "native-admission-reused")
            .count(),
        1
    );
    let verification = command
        .verify_bundle_config(
            downloaded.entrypoint(),
            &outputs,
            &output,
            &keys,
            &ManifestValidationTools::default(),
        )
        .unwrap();
    assert!(verification.passed);
    assert_eq!(
        witness
            .records
            .records()
            .iter()
            .filter(|r| r["event"] == "native-admission-reused")
            .count(),
        1,
        "independent verify reused speculative preparation"
    );
}

#[test]
fn single_worker_low_memory_and_single_output_keep_ordinary_acquisition() {
    for (workers, memory, count) in [
        (1, 4_u64 << 30, 2),
        (2, 1_u64 << 30, 2),
        (2, 4_u64 << 30, 1),
    ] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let source = root.path().join("source");
        let witness = witness(&source);
        let server = Server::new(witness.clone(), false);
        let command = converter(witness.clone(), workers, memory);
        let outputs = outputs();
        let downloaded = command
            .download_bundle_source(
                &server.url(),
                &source,
                &root.path().join("output"),
                &policy(&server.url()),
                &outputs[..count],
            )
            .unwrap();
        downloaded.check(&Stop::new()).unwrap();
        assert!(!witness.prepared.is_set());
    }
}

#[test]
fn cancellation_after_native_preparation_joins_workers_and_removes_partial_acquisition() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let stop = Stop::new();
    let witness = Arc::new(Witness {
        held: Stop::new(),
        prepared: Stop::new(),
        records: Collector::default(),
        destination: source.clone(),
        cancel: Some(stop.clone()),
    });
    let server = Server::new(witness.clone(), true);
    let command = converter(witness, 2, 4_u64 << 30).with_stop(stop);
    let result = command.download_bundle_source(
        &server.url(),
        &source,
        &root.path().join("output"),
        &policy(&server.url()),
        &outputs(),
    );
    assert_eq!(result.unwrap_err().0, "cancelled");
    assert!(
        !source.exists(),
        "cancelled acquisition retained unfinished files"
    );
}

#[test]
fn prepared_admission_never_authorizes_changed_retained_source() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let witness = witness(&source);
    let server = Server::new(witness.clone(), false);
    let command = converter(witness.clone(), 2, 4_u64 << 30);
    let outputs = outputs();
    let downloaded = command
        .download_bundle_source(
            &server.url(),
            &source,
            &output,
            &policy(&server.url()),
            &outputs,
        )
        .unwrap();
    assert!(witness.prepared.is_set());
    let path = downloaded.entrypoint().to_path_buf();
    let original = std::fs::read(&path).unwrap();
    let mut changed = original.clone();
    changed[0] ^= 1;
    std::fs::write(&path, &changed).unwrap();
    let keys = BundleOperatorKeys {
        input: InputKeys::default(),
        hls_input: HlsInputKeys::default(),
        outputs: BTreeMap::new(),
    };
    let result = command.convert_bundle_downloaded(
        &downloaded,
        &outputs,
        &output,
        &keys,
        &ManifestValidationTools::default(),
    );
    assert_eq!(result.unwrap_err().0, "download-source-changed");
    assert!(!output.exists());
    assert!(
        witness
            .records
            .records()
            .iter()
            .all(|r| r["event"] != "native-admission-reused")
    );
    std::fs::write(&path, original).unwrap();
    downloaded.check(&Stop::new()).unwrap();
}

#[test]
fn cached_native_tracks_requalify_the_actual_master_metadata() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let witness = witness(&source);
    let server = Server::start(witness.clone(), false, true, false);
    let command = converter(witness.clone(), 2, 4_u64 << 30);
    let outputs = outputs();
    let downloaded = command
        .download_bundle_source(
            &server.url(),
            &source,
            &output,
            &policy(&server.url()),
            &outputs,
        )
        .unwrap();
    assert!(witness.prepared.is_set());
    let keys = BundleOperatorKeys {
        input: InputKeys::default(),
        hls_input: HlsInputKeys::default(),
        outputs: BTreeMap::new(),
    };
    let result = command.convert_bundle_downloaded(
        &downloaded,
        &outputs,
        &output,
        &keys,
        &ManifestValidationTools::default(),
    );
    assert_eq!(result.unwrap_err().0, "presentation-metadata-mismatch");
    assert!(!output.exists());
    downloaded.check(&Stop::new()).unwrap();
}

#[test]
fn sibling_fetch_failure_keeps_the_original_error_and_does_not_cancel_the_caller() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    let output = root.path().join("output");
    let witness = witness(&source);
    let server = Server::start(witness.clone(), false, false, true);
    let stop = Stop::new();
    let command = converter(witness.clone(), 2, 4_u64 << 30).with_stop(stop.clone());
    let result = command.download_bundle_source(
        &server.url(),
        &source,
        &output,
        &policy(&server.url()),
        &outputs(),
    );
    assert!(
        witness.prepared.is_set(),
        "failure fixture must reach overlapping native preparation"
    );
    assert_eq!(result.unwrap_err().0, "download-source-unavailable");
    assert!(
        !stop.is_set(),
        "the pool failure poisoned the caller cancellation flag"
    );
    assert!(!source.exists() && !output.exists());
}
