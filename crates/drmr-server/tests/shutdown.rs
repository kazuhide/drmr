mod support;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use support::{Harness, start_analysis};

#[test]
fn active_common_shutdown_drains_ownership_even_when_the_app_is_retained() {
    let mut h = Harness::new();
    std::fs::write(h.input.join("clip.mp4"), b"original fixture").unwrap();
    h.cli.hold();
    let started = start_analysis(&h, 'a');
    h.cli.wait_calls(1);
    let id = started["job_id"].as_str().unwrap();
    let mut server = h.server.take().unwrap();
    let app = server.app.clone();
    let addr = server.addr;
    server.shutdown().unwrap();
    server.shutdown().unwrap();
    assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err());
    let observed = app.manager.observation(id).unwrap();
    assert_eq!(observed.handle.state, "interrupted");
    assert!(observed.cleanup_confirmed && observed.supervision_confirmed);
    assert!(!app.manager.is_busy());
    assert!(app.manager.execution().closing);
    let cap = app.common.dispatch(&drmr_server::http::Req {
        method: "GET".into(),
        path: "/api/common/capabilities".into(),
        ..Default::default()
    });
    assert_eq!(cap.status, 200);
    assert_eq!(h.cli.calls(), 1);
    assert_eq!(
        std::fs::read(h.input.join("clip.mp4")).unwrap(),
        b"original fixture"
    );
}
#[test]
fn retired_mutation_routes_never_launch_a_child() {
    let h = Harness::new();
    for path in [
        "/api/convert",
        "/api/hls/analyses",
        "/api/hls/packages",
        "/api/queue",
    ] {
        assert_eq!(h.client.post_json(path, &json!({})).status, 404, "{path}");
    }
    assert_eq!(h.cli.calls(), 0);
}
#[test]
fn an_unread_file_response_cannot_keep_the_runtime_or_file_worker_alive() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    let assets = root.path().join("static");
    std::fs::create_dir(&input).unwrap();
    std::fs::create_dir(&assets).unwrap();
    let path = assets.join("large.bin");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let mut config = drmr_server::ServerConfig::new(input, root.path().join("output"));
    config.bind_port = 0;
    config.static_dir = Some(assets);
    let mut server = drmr_server::spawn(config).unwrap();
    let mut client = TcpStream::connect(server.addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        client,
        "GET /static/large.bin HTTP/1.1\r\nHost: {}\r\n\r\n",
        server.addr
    )
    .unwrap();
    let mut first = [0u8; 1];
    client.read_exact(&mut first).unwrap();
    assert_eq!(first[0], b'H');
    let (done, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        done.send(server.shutdown()).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 * 1024);
    drop(client);
}
