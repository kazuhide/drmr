//! The fixture client must preserve final responses sent before a request body is read.
mod support;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::time::Duration;

#[test]
fn an_early_rejection_is_observed_without_sending_the_large_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16 * 1024);
        }
        stream.write_all(b"HTTP/1.1 413 Content Too Large\r\nContent-Length: 3\r\nConnection: close\r\n\r\nno!").unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut body = [0; 1];
        let received = stream.read(&mut body).unwrap();
        (String::from_utf8(headers).unwrap(), received)
    });
    let client = support::Client::new(&base);
    let answer = client.request(
        "POST",
        "/reject",
        &[("Content-Type", "application/json")],
        Some(&vec![b'x'; 8 * 1024 * 1024]),
    );
    assert_eq!(answer.status, 413);
    assert_eq!(answer.text(), "no!");
    let (headers, received) = server.join().unwrap();
    assert!(
        headers
            .to_ascii_lowercase()
            .contains("expect: 100-continue\r\n")
    );
    assert_eq!(
        received, 0,
        "an early rejection must not trigger body upload"
    );
}
