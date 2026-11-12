//! Checks what a cancel request sends first, against a fake server that records
//! the wire. A login over TLS needs real certificates, so `cancel_tls_live.rs`
//! covers `require` and the verify modes.

#![allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::result_large_err
)]

use datatab_core::secrecy::SecretString;
use datatab_core::{Connection, ConnectionProfile, DbConfig, DbDriver, DbError, QueryRequest};
use datatab_driver_redshift::RedshiftDriver;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FAKE_PASSWORD: &str = "not-a-real-password";
const SERVER_WAIT: Duration = Duration::from_secs(10);

const SSL_REQUEST: Message = [0, 0, 0, 8, 0x04, 0xD2, 0x16, 0x2F];
const CANCEL_REQUEST: Message = [0, 0, 0, 16, 0x04, 0xD2, 0x16, 0x2E];
/// AuthenticationOk, BackendKeyData and ReadyForQuery.
const LOGIN_OK: &[u8] = b"R\0\0\0\x08\0\0\0\0K\0\0\0\x0c\0\0\0\x01\0\0\0\x02Z\0\0\0\x05I";

/// The length and code that start an untyped frontend message.
type Message = [u8; 8];

/// How the fake server answers an `SSLRequest`.
#[derive(Clone, Copy)]
enum SslAnswer {
    /// Refuses TLS, so the client continues in clear text on the same socket.
    Refuse,
    /// Accepts TLS, then sends bytes that are not a TLS record.
    BreakHandshake,
}

#[derive(Clone, Copy)]
enum CancelPath {
    FormThenHandle,
    UriThenActive,
}

struct Shared {
    answer: SslAnswer,
    query_running: AtomicBool,
    query_started: Sender<()>,
    cancel_wire: Sender<Vec<Message>>,
    end_session: Mutex<Receiver<()>>,
}

struct FakeServer {
    port: u16,
    query_started: Receiver<()>,
    cancel_wire: Receiver<Vec<Message>>,
    end_session: Sender<()>,
}

impl FakeServer {
    fn start(answer: SslAnswer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake server");
        let port = listener.local_addr().expect("fake server address").port();
        let (query_started_sender, query_started) = channel();
        let (cancel_wire_sender, cancel_wire) = channel();
        let (end_session, end_session_receiver) = channel();
        let shared = Arc::new(Shared {
            answer,
            query_running: AtomicBool::new(false),
            query_started: query_started_sender,
            cancel_wire: cancel_wire_sender,
            end_session: Mutex::new(end_session_receiver),
        });

        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let shared = shared.clone();
                std::thread::spawn(move || serve(stream, &shared));
            }
        });

        Self {
            port,
            query_started,
            cancel_wire,
            end_session,
        }
    }
}

/// Handles one connection. Only a connection that arrives while the query runs
/// reports its wire, so a failed TLS login before it does not count.
fn serve(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(SERVER_WAIT))?;
    let is_cancel_connection = shared.query_running.load(Ordering::SeqCst);
    let report = |wire: &[Message]| {
        if is_cancel_connection {
            shared
                .cancel_wire
                .send(wire.to_vec())
                .map_err(io::Error::other)?;
        }
        Ok::<(), io::Error>(())
    };
    let mut wire = Vec::new();

    loop {
        let mut message: Message = [0; 8];
        stream.read_exact(&mut message)?;
        wire.push(message);

        if message == CANCEL_REQUEST {
            return report(&wire);
        }

        if message != SSL_REQUEST {
            return serve_session(stream, message, shared);
        }

        match shared.answer {
            SslAnswer::Refuse => stream.write_all(b"N")?,
            SslAnswer::BreakHandshake => {
                report(&wire)?;
                stream.write_all(b"S")?;
                return stream.write_all(b"this is not a TLS record\n");
            }
        }
    }
}

/// Accepts the login and keeps the query running until the test ends it.
fn serve_session(mut stream: TcpStream, startup: Message, shared: &Shared) -> io::Result<()> {
    let [a, b, c, d, ..] = startup;
    let length = u32::from_be_bytes([a, b, c, d]) as usize;
    let mut parameters = vec![0; length - startup.len()];
    stream.read_exact(&mut parameters)?;
    stream.write_all(LOGIN_OK)?;

    let mut first_query_byte = [0; 1];
    stream.read_exact(&mut first_query_byte)?;
    shared.query_running.store(true, Ordering::SeqCst);
    shared.query_started.send(()).map_err(io::Error::other)?;

    let end_session = shared.end_session.lock().expect("end session lock");
    end_session
        .recv_timeout(SERVER_WAIT)
        .map_err(io::Error::other)
}

fn connect(ssl_mode: &str, port: u16, path: CancelPath) -> Result<Box<dyn Connection>, DbError> {
    let uri = matches!(path, CancelPath::UriThenActive)
        .then(|| format!("redshift://datatab@127.0.0.1:{port}/dev?sslmode={ssl_mode}"));
    let profile = ConnectionProfile::new(
        "cancel-tls",
        DbConfig::Redshift {
            use_uri: uri.is_some(),
            uri,
            host: "127.0.0.1".to_string(),
            port,
            user: "datatab".to_string(),
            database: "dev".to_string(),
            ssl_mode: Some(ssl_mode.to_string()),
            ssl_root_cert_path: None,
            ssl_client_cert_path: None,
            ssl_client_key_path: None,
            ssh_tunnel: None,
            ssh_tunnel_profile_id: None,
        },
    );

    let password = SecretString::from(FAKE_PASSWORD.to_string());
    RedshiftDriver::new().connect_with_secrets(&profile, Some(&password), None)
}

/// Runs a query the server never answers, cancels it, and returns what the
/// cancel connection sent.
fn cancel_wire(ssl_mode: &str, answer: SslAnswer, path: CancelPath) -> Vec<Message> {
    let server = FakeServer::start(answer);
    let connection = connect(ssl_mode, server.port, path).expect("the fake login succeeds");
    let connection = &*connection;

    std::thread::scope(|scope| {
        let running = scope.spawn(|| connection.execute(&QueryRequest::new("SELECT 1")));
        server
            .query_started
            .recv_timeout(SERVER_WAIT)
            .expect("the query reaches the server");

        let cancel = match path {
            CancelPath::FormThenHandle => connection.cancel_handle().cancel(),
            CancelPath::UriThenActive => connection.cancel_active(),
        };
        let wire = server.cancel_wire.recv_timeout(SERVER_WAIT);

        server.end_session.send(()).expect("end the session");
        let query = running.join().expect("query thread panicked");
        assert!(query.is_err(), "the closed session fails the query");

        cancel.expect("the cancel call succeeds");
        wire.expect("a cancel connection arrives")
    })
}

#[test]
fn cancel_asks_for_tls_when_the_connection_has_a_connector() {
    for path in [CancelPath::FormThenHandle, CancelPath::UriThenActive] {
        assert_eq!(
            cancel_wire("prefer", SslAnswer::Refuse, path),
            [SSL_REQUEST, CANCEL_REQUEST]
        );
    }
}

#[test]
fn cancel_stays_in_clear_text_after_the_prefer_retry() {
    for path in [CancelPath::FormThenHandle, CancelPath::UriThenActive] {
        assert_eq!(
            cancel_wire("prefer", SslAnswer::BreakHandshake, path),
            [CANCEL_REQUEST]
        );
    }
}

#[test]
fn cancel_stays_in_clear_text_with_disable() {
    for path in [CancelPath::FormThenHandle, CancelPath::UriThenActive] {
        assert_eq!(
            cancel_wire("disable", SslAnswer::Refuse, path),
            [CANCEL_REQUEST]
        );
    }
}
