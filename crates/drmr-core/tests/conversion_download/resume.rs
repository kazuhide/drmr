//! Retry the public acquisition API against real HTTP without media tools.
use super::*;
use std::path::{Path, PathBuf};

const HLS: &[u8] = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nvideo/720p/one.ts\n#EXTINF:2,\nvideo/720p/two.ts\n#EXT-X-ENDLIST\n";

struct State {
    manifest: Vec<u8>,
    first: Vec<u8>,
    etag: bool,
    fail_second: bool,
    requests: Vec<String>,
    sent: usize,
}
struct ResumeServer {
    address: std::net::SocketAddr,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl ResumeServer {
    fn new(manifest: &[u8], etag: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            manifest: manifest.to_vec(),
            first: vec![0x47; 4096],
            etag,
            fail_second: true,
            requests: Vec::new(),
            sent: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, ending) = (state.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                if ending.load(Ordering::Acquire) {
                    break;
                }
                let mut socket = socket.unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    assert!(request.len() < 16384);
                }
                let request = String::from_utf8(request).unwrap();
                let target = request.split_whitespace().nth(1).unwrap();
                let mut state = shared.lock().unwrap();
                let first = target.ends_with("one.ts") || target.ends_with("s1.m4s");
                let second = target.ends_with("two.ts") || target.ends_with("s2.m4s");
                let (status, body) =
                    if first && state.etag && request.contains("If-None-Match: \"one\"") {
                        ("304 Not Modified", Vec::new())
                    } else if first {
                        ("200 OK", state.first.clone())
                    } else if second && state.fail_second {
                        ("404 Not Found", Vec::new())
                    } else if second {
                        ("200 OK", vec![0x47; 2048])
                    } else if target.ends_with("init.mp4") {
                        ("200 OK", b"initialization bytes".to_vec())
                    } else {
                        ("200 OK", state.manifest.clone())
                    };
                let etag = if first && state.etag {
                    "ETag: \"one\"\r\n"
                } else {
                    ""
                };
                state.sent += body.len();
                state.requests.push(request.clone());
                drop(state);
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    if status.starts_with("304") {
                        4096
                    } else {
                        body.len()
                    }
                )
                .unwrap();
                socket.write_all(&body).unwrap();
            }
        });
        Self {
            address,
            state,
            stop,
            thread: Some(thread),
        }
    }
    fn url(&self, name: &str) -> String {
        format!("http://{}/{name}", self.address)
    }
}
impl Drop for ResumeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn cache_path(directory: &Path) -> PathBuf {
    directory.with_file_name(format!(
        ".{}.drmr-download",
        directory.file_name().unwrap().to_str().unwrap()
    ))
}

#[test]
fn interrupted_hls_reuses_complete_bytes_after_conditional_http_and_keeps_source_layout() {
    let server = ResumeServer::new(HLS, true);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let command = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source);
    assert!(
        command
            .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
            .is_err()
    );
    assert!(!directory.exists());
    assert!(cache_path(&directory).is_dir());
    let mut state = server.state.lock().unwrap();
    state.fail_second = false;
    state.requests.clear();
    state.sent = 0;
    drop(state);
    let source = command
        .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
        .unwrap();
    assert_eq!(
        source.entrypoint(),
        directory.canonicalize().unwrap().join("stream.m3u8")
    );
    assert_eq!(
        std::fs::read(directory.join("video/720p/one.ts")).unwrap(),
        vec![0x47; 4096]
    );
    source.check(&Stop::new()).unwrap();
    assert!(!cache_path(&directory).exists());
    let state = server.state.lock().unwrap();
    assert!(
        state
            .requests
            .iter()
            .any(|request| request.contains("If-None-Match: \"one\""))
    );
    assert_eq!(
        state.sent,
        HLS.len() + 2048,
        "the complete first segment must not be downloaded again"
    );
    drop(state);
    assert!(
        command
            .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
            .is_err()
    );
    source.check(&Stop::new()).unwrap();
}

#[test]
fn manifest_change_refuses_to_mix_generations_and_preserves_retry_cache() {
    let server = ResumeServer::new(HLS, true);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let command = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source);
    assert!(
        command
            .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
            .is_err()
    );
    let mut state = server.state.lock().unwrap();
    state.manifest.extend_from_slice(b"\n");
    state.fail_second = false;
    drop(state);
    let error = command
        .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
        .unwrap_err();
    assert_eq!(error.0, "download-source-changed");
    assert!(!directory.exists());
    assert!(cache_path(&directory).is_dir());
}

#[test]
fn absent_validators_download_again_and_refuse_changed_bytes() {
    let server = ResumeServer::new(HLS, false);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let command = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source);
    assert!(
        command
            .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
            .is_err()
    );
    let mut state = server.state.lock().unwrap();
    state.first[0] ^= 1;
    state.fail_second = false;
    drop(state);
    let error = command
        .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
        .unwrap_err();
    assert_eq!(error.0, "download-source-changed");
    assert!(!directory.exists());
}

#[test]
fn unchanged_resources_without_validators_are_compared_after_a_full_download() {
    let server = ResumeServer::new(HLS, false);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let command = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source);
    assert!(
        command
            .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
            .is_err()
    );
    server.state.lock().unwrap().fail_second = false;
    let source = command
        .acquire_source(&server.url("stream.m3u8"), &directory, &policy())
        .unwrap();
    source.check(&Stop::new()).unwrap();
    assert!(!cache_path(&directory).exists());
    let state = server.state.lock().unwrap();
    assert!(
        state
            .requests
            .iter()
            .all(|request| !request.contains("If-None-Match"))
    );
}

#[test]
fn interrupted_static_dash_revalidates_complete_segments_and_keeps_timeline() {
    let mpd = b"<MPD xmlns='urn:mpeg:dash:schema:mpd:2011' type='static' mediaPresentationDuration='PT4S'><Period><AdaptationSet><Representation id='v' bandwidth='10'><SegmentTemplate initialization='init.mp4' media='s$Number$.m4s' timescale='1'><SegmentTimeline><S d='2' r='1'/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>";
    let server = ResumeServer::new(mpd, true);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let command = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source);
    assert!(
        command
            .acquire_source(&server.url("manifest.mpd"), &directory, &policy())
            .is_err()
    );
    let mut state = server.state.lock().unwrap();
    state.fail_second = false;
    state.requests.clear();
    state.sent = 0;
    drop(state);
    let source = command
        .acquire_source(&server.url("manifest.mpd"), &directory, &policy())
        .unwrap();
    source.check(&Stop::new()).unwrap();
    let projected = std::fs::read_to_string(source.entrypoint()).unwrap();
    assert!(projected.contains("SegmentTimeline"));
    assert_eq!(
        std::fs::read(directory.join("s1.m4s")).unwrap(),
        vec![0x47; 4096]
    );
    assert!(!cache_path(&directory).exists());
    let state = server.state.lock().unwrap();
    assert!(
        state
            .requests
            .iter()
            .any(|request| request.contains("If-None-Match: \"one\""))
    );
    assert_eq!(state.sent, mpd.len() + b"initialization bytes".len() + 2048);
}

#[test]
fn mp4_acquisition_cleans_the_checkpoint_only_after_sealed_publication() {
    let bytes = b"\0\0\0\x18ftypisom\0\0\0\0isomiso2\0\0\0\x10mdatcontent!";
    let server = ResumeServer::new(bytes, false);
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("source");
    let source = converter(Stop::new())
        .with_download_resume(true)
        .with_layout(drmr_core::conversion::layout::LayoutMode::Source)
        .acquire_source(&server.url("asset.mp4"), &directory, &policy())
        .unwrap();
    assert_eq!(source.entrypoint().file_name().unwrap(), "asset.mp4");
    assert_eq!(std::fs::read(source.entrypoint()).unwrap(), bytes);
    source.check(&Stop::new()).unwrap();
    assert!(!cache_path(&directory).exists());
}
