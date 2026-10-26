//! Acquisition-only path regressions use controlled HTTP fixtures, not media tools.
mod directory_urls;
mod origins;
use super::*;
use crate::source::platform::{Platform, Stream};
use std::{
    io::{self, Cursor, Read, Write},
    net::SocketAddr,
    time::Duration,
};

struct MemoryPlatform(
    Arc<BTreeMap<String, Vec<u8>>>,
    Arc<BTreeMap<String, String>>,
);
impl Platform for MemoryPlatform {
    fn resolve(&self, _host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))])
    }
    fn connect(&self, _address: SocketAddr, _timeout: Duration) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(MemoryStream {
            resources: self.0.clone(),
            redirects: self.1.clone(),
            request: Vec::new(),
            response: None,
        }))
    }
}
struct MemoryStream {
    resources: Arc<BTreeMap<String, Vec<u8>>>,
    redirects: Arc<BTreeMap<String, String>>,
    request: Vec<u8>,
    response: Option<Cursor<Vec<u8>>>,
}
impl Write for MemoryStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.request.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Read for MemoryStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.response.is_none() {
            let request = std::str::from_utf8(&self.request).map_err(io::Error::other)?;
            let target = request
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| io::Error::other("missing fixture request"))?;
            let bytes = if let Some(location) = self.redirects.get(target) {
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
            } else {
                let body = self
                    .resources
                    .get(target)
                    .ok_or_else(|| io::Error::other("unplanned fixture request"))?;
                let mut bytes = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                bytes.extend_from_slice(body);
                bytes
            };
            self.response = Some(Cursor::new(bytes));
        }
        self.response.as_mut().unwrap().read(out)
    }
}
impl Stream for MemoryStream {
    fn set_read_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
    fn set_write_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
}

fn resources(collision: bool) -> BTreeMap<String, Vec<u8>> {
    let mut media = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:6,\nseg_0.ts?token=first\n".to_owned();
    if collision {
        media.push_str("#EXTINF:6,\nseg_0.ts?token=second\n");
    }
    media.push_str("#EXT-X-ENDLIST\n");
    BTreeMap::from([
        (
            "/vod/master.m3u8".into(),
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo/720p/media.m3u8\n".to_vec(),
        ),
        ("/vod/video/720p/media.m3u8".into(), media.into_bytes()),
        (
            "/vod/video/720p/seg_0.ts?token=first".into(),
            b"retained transport bytes".to_vec(),
        ),
    ])
}

fn acquire(
    directory: &Path,
    mode: LayoutMode,
    responses: BTreeMap<String, Vec<u8>>,
    root: &str,
) -> Result<DownloadedSource, ConversionError> {
    acquire_redirected(directory, mode, responses, BTreeMap::new(), root)
}

fn acquire_redirected(
    directory: &Path,
    mode: LayoutMode,
    responses: BTreeMap<String, Vec<u8>>,
    redirects: BTreeMap<String, String>,
    root: &str,
) -> Result<DownloadedSource, ConversionError> {
    let owned = OwnedDownload::reserve_source(directory)?;
    owned.create_dir(Path::new("raw"))?;
    if mode == LayoutMode::Normalized {
        owned.create_dir(Path::new("media"))?;
    }
    let policy = FetchPolicy {
        allow_http: true,
        allow_private_network: true,
        allowed_origins: vec!["http://example.invalid".into()],
        ..FetchPolicy::default()
    };
    let fetcher = Fetcher::new(
        &policy,
        owned.path(),
        Arc::new(MemoryPlatform(Arc::new(responses), Arc::new(redirects))),
    );
    let mut graph = Graph {
        owned: &owned,
        policy: &policy,
        fetcher: &fetcher,
        playlists: BTreeMap::new(),
        media: BTreeMap::new(),
        weight: 0,
        text_bytes: 0,
        records: Vec::new(),
        resources: Vec::new(),
        expected: BTreeMap::new(),
        root_projection: Vec::new(),
        playlist_count: 0,
        layout_mode: mode,
        mapper: None,
    };
    let (entrypoint, contents) = graph.entrypoint(root)?;
    graph.fetch_media(2, &Stop::new())?;
    graph.check_acquired_limits(entrypoint, &contents)?;
    let Entrypoint::Projection(projection) = contents else {
        return Err(INVALID);
    };
    graph.save(entrypoint, &projection)?;
    let index = serde_json::to_vec(&graph.records).unwrap();
    graph.save("raw/index.json", &index)?;
    let files = std::mem::take(&mut graph.expected);
    drop(graph);
    fetcher.close_all();
    let retained_bytes = owned.retained_bytes()?;
    let mut seal = Seal {
        owned,
        files,
        entrypoint,
        retained_bytes,
        _retained: None,
    };
    seal.check(&Stop::new())?;
    seal.owned.retain();
    Ok(DownloadedSource {
        entrypoint: seal.owned.path().join(entrypoint),
        seal: Arc::new(seal),
    })
}

#[test]
fn source_download_keeps_nested_paths_and_child_relative_uris() {
    let parent = crate::native::private_tempdir().unwrap();
    let source = acquire(
        &parent.path().join("source"),
        LayoutMode::Source,
        resources(false),
        "http://example.invalid/vod/master.m3u8",
    )
    .unwrap();
    let root = source.directory();
    assert_eq!(
        std::fs::read(root.join("video/720p/seg_0.ts")).unwrap(),
        b"retained transport bytes"
    );
    let master = std::fs::read_to_string(source.entrypoint()).unwrap();
    assert!(master.contains("video/720p/media.m3u8"));
    let child = std::fs::read_to_string(root.join("video/720p/media.m3u8")).unwrap();
    assert!(child.contains("\nseg_0.ts\n"));
    assert!(!child.contains("token="));
    assert!(!root.join("media/r000000.ts").exists());
    source.check(&Stop::new()).unwrap();
}

#[test]
fn normalized_download_retains_the_legacy_flat_projection() {
    let parent = crate::native::private_tempdir().unwrap();
    let source = acquire(
        &parent.path().join("source"),
        LayoutMode::Normalized,
        resources(false),
        "http://example.invalid/vod/master.m3u8",
    )
    .unwrap();
    assert!(source.directory().join("p000001.m3u8").is_file());
    assert_eq!(
        std::fs::read(source.directory().join("media/r000000.ts")).unwrap(),
        b"retained transport bytes"
    );
    source.check(&Stop::new()).unwrap();
}

#[test]
fn source_download_refuses_query_collisions_without_leaving_a_partial_source() {
    let parent = crate::native::private_tempdir().unwrap();
    let path = parent.path().join("source");
    assert_eq!(
        acquire(
            &path,
            LayoutMode::Source,
            resources(true),
            "http://example.invalid/vod/master.m3u8"
        )
        .unwrap_err(),
        ConversionError("source-layout-collision")
    );
    assert!(!path.exists());
}

#[test]
fn source_dash_download_keeps_referenced_media_paths() {
    let parent = crate::native::private_tempdir().unwrap();
    let manifest = br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4" codecs="avc1.64001f"><SegmentTemplate timescale="1" duration="6" initialization="video/720p/init.mp4" media="video/720p/seg_$Number$.m4s" startNumber="1"/></Representation></AdaptationSet></Period></MPD>"#;
    let source = acquire(
        &parent.path().join("source"),
        LayoutMode::Source,
        BTreeMap::from([
            ("/vod/input.mpd".into(), manifest.to_vec()),
            ("/vod/video/720p/init.mp4".into(), b"init bytes".to_vec()),
            ("/vod/video/720p/seg_1.m4s".into(), b"media bytes".to_vec()),
        ]),
        "http://example.invalid/vod/input.mpd",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(source.directory().join("video/720p/seg_1.m4s")).unwrap(),
        b"media bytes"
    );
    let projection = std::fs::read_to_string(source.entrypoint()).unwrap();
    assert!(projection.contains("video/720p/init.mp4"));
    assert!(projection.contains("video/720p/seg_1.m4s"));
    source.check(&Stop::new()).unwrap();
}

fn indexed_object() -> Vec<u8> {
    let mut bytes = 8_u32.to_be_bytes().to_vec();
    bytes.extend_from_slice(b"ftyp");
    for value in [56_u32, u32::from_be_bytes(*b"sidx"), 0, 1, 1, 0, 0] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(&2_u32.to_be_bytes());
    for _ in 0..2 {
        for value in [8_u32, 6, 0x9000_0000] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
    }
    for _ in 0..2 {
        bytes.extend_from_slice(&8_u32.to_be_bytes());
        bytes.extend_from_slice(b"mdat");
    }
    bytes
}

fn indexed_server() -> (
    String,
    Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let manifest = br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT12S"><Period><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4" codecs="avc1.64001f"><BaseURL>video/720p/indexed.mp4</BaseURL><SegmentBase timescale="1" indexRange="8-63" indexRangeExact="true"><Initialization range="0-7"/></SegmentBase></Representation></AdaptationSet></Period></MPD>"#;
    let responses = BTreeMap::from([
        ("/vod/input.mpd", manifest.to_vec()),
        ("/vod/video/720p/indexed.mp4", indexed_object()),
    ]);
    let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = stopped.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let mut connection = loop {
                match listener.accept() {
                    Ok((connection, _)) => break connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        if stop.load(std::sync::atomic::Ordering::Acquire) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            };
            connection.set_nonblocking(false).unwrap();
            connection
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192);
                let mut byte = [0];
                connection.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            let request = std::str::from_utf8(&request).unwrap();
            let target = request.split_whitespace().nth(1).unwrap();
            let body = responses.get(target).expect("planned fixture request");
            write!(
                connection,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            connection.write_all(body).unwrap();
        }
    });
    (format!("http://{address}/vod/input.mpd"), stopped, server)
}

#[test]
fn public_acquisition_enforces_the_segment_limit_after_an_indexed_object_arrives() {
    use crate::{
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };
    let root = crate::native::private_tempdir().unwrap();
    let converter = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "must-not-run",
        "must-not-run",
    )
    .with_max_workers(Some(1))
    .unwrap();
    for limit in [1, 2] {
        let (url, stopped, server) = indexed_server();
        let policy = FetchPolicy {
            allow_http: true,
            allow_private_network: true,
            max_segments_per_track: limit,
            ..FetchPolicy::default()
        };
        let destination = root.path().join(format!("source-{limit}"));
        let result = converter.acquire_source(&url, &destination, &policy);
        stopped.store(true, std::sync::atomic::Ordering::Release);
        server.join().unwrap();
        if limit == 1 {
            assert_eq!(
                result.unwrap_err(),
                ConversionError("dash-index-entry-limit-or-size")
            );
            assert!(!destination.exists());
        } else {
            result.unwrap().check(&Stop::new()).unwrap();
        }
    }
}
