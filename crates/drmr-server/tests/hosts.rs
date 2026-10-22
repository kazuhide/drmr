mod support;

use std::io::{Read, Write};
use std::net::TcpStream;
use support::Harness;

fn raw_request(h: &Harness, headers: &str) -> String {
    let address = h.client.base.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(
            format!("GET /api/common/capabilities HTTP/1.1\r\nConnection: close\r\n{headers}\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn untrusted_hosts_cannot_read_common_resources_or_static_pages() {
    let h = Harness::new();

    for path in [
        "/",
        "/api/common/capabilities",
        "/clearkey/license?asset_id=sample",
        "/api/common/sessions",
    ] {
        let answer = h.client.get_with(path, &[("Host", "attacker.example")]);
        assert_eq!(answer.status, 403, "{path}");
        assert!(!answer.text().contains("PRIVATE_SENTINEL"));
        assert!(!answer.text().contains("sample"));
    }
}

#[test]
fn common_host_refusals_retain_distinct_domain_codes() {
    let h = Harness::new();
    for (host, status, code) in [
        ("attacker.example", 403, "untrusted-host"),
        ("localhost:bad", 400, "invalid-host"),
    ] {
        let response = h
            .client
            .get_with("/api/common/capabilities", &[("Host", host)]);
        assert_eq!(response.status, status);
        let body = response.json();
        assert_eq!(body["schema_version"], 1);
        assert_eq!(body["error"]["code"], code);
        assert_ne!(body["error"]["code"], "same-origin-required");
        assert!(!response.text().contains(host));
    }
    let response = h.client.post_json_with(
        "/api/common/sessions",
        &serde_json::json!({}),
        &[("Origin", "https://attacker.example")],
    );
    assert_eq!(response.status, 403);
    assert_eq!(response.json()["error"]["code"], "same-origin-required");
}

#[test]
fn trusted_local_hosts_with_valid_ports_remain_usable() {
    let h = Harness::new();
    for host in [
        "localhost",
        "LOCALHOST:8080",
        "127.0.0.1:8080",
        "[::1]:8080",
    ] {
        assert_eq!(
            h.client
                .get_with("/api/common/sessions", &[("Host", host)])
                .status,
            200,
            "{host}"
        );
    }
    assert_eq!(h.client.get("/api/common/sessions").status, 200);
}

#[test]
fn malformed_or_ambiguous_hosts_are_rejected_before_routing() {
    let h = Harness::new();
    for host in [
        "user@localhost",
        "localhost:",
        "localhost:65536",
        "localhost:bad",
        "localhost.evil.example",
    ] {
        let response = raw_request(&h, &format!("Host: {host}\r\n"));
        assert!(
            response.starts_with("HTTP/1.1 400") || response.starts_with("HTTP/1.1 403"),
            "{host}: {response}"
        );
    }
    for headers in ["", "Host: localhost\r\nHost: attacker.example\r\n"] {
        let response = raw_request(&h, headers);
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    }
}
