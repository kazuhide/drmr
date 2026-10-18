mod support;

use drmr_server::{ShutdownError, jobs::JobError};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;
use support::{Harness, stage};

#[test]
fn active_and_queued_shutdown_drains_ownership_even_when_the_app_is_retained() {
    let mut h = Harness::new();
    stage(&h.catalog, "active");
    stage(&h.catalog, "pending");
    h.cli.hold();
    let start = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "active"}));
    assert_eq!(start.status, 202);
    let job_id = start.json()["job_id"].as_str().unwrap().to_string();
    h.cli.wait_calls(1);
    assert_eq!(
        h.client
            .post_json("/api/queue", &json!({"asset_ids": ["pending"]}))
            .status,
        202
    );
    assert!(h.app().queue.runner_active());
    let mut server = h.server.take().unwrap();
    let app = server.app.clone();
    let addr = server.addr;
    server.shutdown().unwrap();
    server.shutdown().unwrap();
    assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err());
    assert!(!app.manager.is_busy());
    assert!(!app.queue.runner_active());
    assert!(app.queue.tick().is_none());
    assert!(matches!(
        app.manager.submit_command(&[], &[]),
        Err(JobError::ShuttingDown)
    ));
    assert_eq!(app.service.status(&job_id).unwrap()["state"], "interrupted");
    assert_eq!(
        h.cli.calls(),
        1,
        "pending work must not start during or after shutdown"
    );
    assert!(h.output.join("active/reports").is_dir());
    assert!(h.catalog.join("active/keys.env").is_file());
}

#[test]
fn failed_history_cleanup_keeps_the_server_owner_available_for_recovery() {
    let mut h = Harness::new();
    stage(&h.catalog, "retained");
    h.cli.hold();
    let response = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "retained"}));
    assert_eq!(response.status, 202);
    let job_id = response.json()["job_id"].as_str().unwrap().to_string();
    h.cli.wait_calls(1);
    let reports = h.output.join("retained/reports");
    let held = h.output.join("retained/reports-held");
    std::fs::rename(&reports, &held).unwrap();
    std::fs::write(&reports, b"owned fixture blocks terminal persistence").unwrap();
    let mut server = h.server.take().unwrap();
    let app = server.app.clone();
    assert!(matches!(
        server.shutdown(),
        Err(ShutdownError::RecordsUnconfirmed)
    ));
    assert!(std::sync::Arc::ptr_eq(&app, &server.app));
    assert!(TcpStream::connect_timeout(&server.addr, Duration::from_secs(1)).is_ok());
    assert!(!app.manager.is_busy());
    assert_eq!(app.service.status(&job_id).unwrap()["state"], "interrupted");
    assert_eq!(h.client.get("/api/catalog").status, 503);
    std::fs::remove_file(&reports).unwrap();
    std::fs::rename(&held, &reports).unwrap();
    server.shutdown().unwrap();
    assert!(TcpStream::connect_timeout(&server.addr, Duration::from_secs(1)).is_err());
    assert_eq!(app.service.status(&job_id).unwrap()["state"], "interrupted");
}

#[test]
fn an_unread_file_response_cannot_keep_the_runtime_or_file_worker_alive() {
    let mut h = Harness::new();
    let (entry, _) = stage(&h.catalog, "large");
    let path = entry.join("download/large.m4s");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();
    let mut server = h.server.take().unwrap();
    let mut client = TcpStream::connect(server.addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        client,
        "GET /source/large/large.m4s HTTP/1.1\r\nHost: {}\r\n\r\n",
        server.addr
    )
    .unwrap();
    let mut first = [0u8; 1];
    client.read_exact(&mut first).unwrap();
    assert_eq!(first[0], b'H');
    // Leave the reader connected without draining its response body.
    let (done, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let result = server.shutdown();
        done.send(result).unwrap();
    });
    received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 * 1024);
    drop(client);
}
