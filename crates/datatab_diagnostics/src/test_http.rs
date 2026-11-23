//! A one-request HTTP server on loopback, for the tests that need to see what
//! the sender puts on the wire. No test reaches a real host: `127.0.0.1` with
//! a port the kernel picks is the whole of its network.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Serves exactly one request and answers it with `response`. Returns the
/// address to point a sender at, and a join handle that yields the raw
/// request (head and body) it received.
pub(super) fn serve_once(response: &'static str) -> (SocketAddr, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept one request");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");

        let mut raw = Vec::new();
        let mut chunk = [0u8; 4096];

        loop {
            let read = stream.read(&mut chunk).expect("read request");
            if read == 0 {
                break;
            }
            raw.extend_from_slice(&chunk[..read]);

            let text = String::from_utf8_lossy(&raw).to_string();
            let Some(headers_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let head = &text[..headers_end];
            let body_len = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    (name.eq_ignore_ascii_case("content-length"))
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);

            if raw.len() >= headers_end + 4 + body_len {
                break;
            }
        }

        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");

        String::from_utf8_lossy(&raw).to_string()
    });

    (addr, handle)
}
