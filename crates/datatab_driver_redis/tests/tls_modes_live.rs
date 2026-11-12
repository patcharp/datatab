//! Connects to a server that accepts only TLS in each TLS mode of the form,
//! through a proxy that records what each connection sends.
//!
//! Needs Docker, the local `redis:7-alpine` image and the `openssl` command.
//! Certificates are generated when the test runs and removed after it.
//!
//! ```text
//! DATATAB_TEST_SLOT=<slot> DATATAB_TEST_SCRATCH=<folder> \
//!   cargo test -p datatab_driver_redis --test tls_modes_live --locked \
//!   -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::result_large_err
)]

use datatab_core::secrecy::SecretString;
use datatab_core::{Connection, ConnectionProfile, DbConfig, DbDriver, DbError, QueryRequest};
use datatab_driver_redis::RedisDriver;
use datatab_test_support::containers;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const IMAGE: &str = "redis:7-alpine";
const FAKE_PASSWORD: &str = "not-a-real-password";
const CLEAR_TEXT_VALUE: &str = "value-that-must-not-cross-in-clear";
const START_LIMIT: Duration = Duration::from_secs(30);
const TLS_HANDSHAKE_RECORD: u8 = 0x16;

const MAKE_CERTIFICATES: &str = r#"set -e
new_key="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
self_signed() {
  openssl req -x509 $new_key -days 1 -keyout "$1.key" -out "$1.crt" -subj "/CN=$1"
}
self_signed ca
self_signed other-ca
openssl req $new_key -keyout server.key -out server.csr -subj "/CN=localhost"
printf 'subjectAltName=DNS:localhost\n' > server.ext
openssl x509 -req -days 1 -CAcreateserial -CA ca.crt -CAkey ca.key \
  -in server.csr -extfile server.ext -out server.crt
"#;

/// The server gets its own copy of the key, because the image runs Redis as a
/// user that cannot read a key file the host user owns.
const CONTAINER_SCRIPT: &str = "install -d -o redis -m 700 /run/redistls \
     && install -o redis -m 600 /tls/server.key /run/redistls/server.key \
     && install -o redis -m 644 /tls/server.crt /run/redistls/server.crt \
     && install -o redis -m 644 /tls/ca.crt /run/redistls/ca.crt \
     && exec docker-entrypoint.sh redis-server --port 0 --tls-port 6379 \
     --tls-cert-file /run/redistls/server.crt --tls-key-file /run/redistls/server.key \
     --tls-ca-cert-file /run/redistls/ca.crt --tls-auth-clients no \
     --requirepass \"$REQUIRED_PASSWORD\"";

fn run_command(command: &mut Command) -> String {
    let output = command.output().expect("failed to start command");
    assert!(
        output.status.success(),
        "{:?} failed: {}",
        command.get_program(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A container that accepts connections only over TLS. Dropping it removes the
/// container and the throwaway certificates.
struct TlsRedis {
    container: String,
    certificates: PathBuf,
}

impl TlsRedis {
    fn start() -> Self {
        let scratch = std::env::var_os("DATATAB_TEST_SCRATCH")
            .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
        let certificates = scratch.join("redis-tls-certificates");
        fs::create_dir_all(&certificates).expect("create certificate folder");
        run_command(
            Command::new("sh")
                .args(["-c", MAKE_CERTIFICATES])
                .current_dir(&certificates),
        );

        let slot = std::env::var("DATATAB_TEST_SLOT").unwrap_or_else(|_| "local".to_string());
        let container = format!("datatab-{slot}-redis");
        let server = Self {
            container,
            certificates,
        };
        run_command(
            Command::new("docker")
                .args(["run", "-d", "--name", &server.container, "--memory=256m"])
                .args(["--tmpfs", "/data"])
                .args(["-p", "127.0.0.1::6379"])
                .args(["-v", &format!("{}:/tls:ro", server.certificates.display())])
                .args(["-e", &format!("REQUIRED_PASSWORD={FAKE_PASSWORD}")])
                .args(["--entrypoint", "sh", IMAGE, "-c", CONTAINER_SCRIPT]),
        );
        server
    }

    fn port(&self) -> u16 {
        let published =
            run_command(Command::new("docker").args(["port", &self.container, "6379/tcp"]));
        let port = published.rsplit(':').next().expect("published port");
        port.parse().expect("published port number")
    }

    fn certificate(&self, name: &str) -> PathBuf {
        self.certificates.join(name)
    }
}

impl Drop for TlsRedis {
    fn drop(&mut self) {
        Command::new("docker")
            .args(["rm", "-f", &self.container])
            .output()
            .expect("failed to remove the container");
        fs::remove_dir_all(&self.certificates).expect("failed to remove the certificates");
    }
}

/// Copies one direction of a connection. `recording` gets each chunk before it
/// is forwarded, so whatever the peer has answered is already recorded.
fn relay(
    mut from: TcpStream,
    mut to: TcpStream,
    recording: Option<(&Mutex<Vec<Vec<u8>>>, usize)>,
) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => 0,
            Err(error) => return Err(error),
        };
        if read == 0 {
            return match to.shutdown(Shutdown::Write) {
                Err(error) if error.kind() != io::ErrorKind::NotConnected => Err(error),
                _ => Ok(()),
            };
        }
        if let Some((connections, index)) = recording {
            connections.lock().unwrap()[index].extend_from_slice(&buffer[..read]);
        }
        if let Err(error) = to.write_all(&buffer[..read]) {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(error);
        }
    }
}

/// Forwards to the server and records what clients send, one entry per
/// connection. The threads end with the sockets or with the test process.
struct RecordingProxy {
    port: u16,
    connections: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl RecordingProxy {
    fn start(upstream_port: u16) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind proxy");
        let port = listener.local_addr().expect("proxy address").port();
        let connections = Arc::new(Mutex::new(Vec::new()));
        thread::spawn({
            let connections = connections.clone();
            move || {
                for client in listener.incoming() {
                    let client = client.expect("accept client");
                    let server =
                        TcpStream::connect(("127.0.0.1", upstream_port)).expect("connect server");
                    let index = {
                        let mut connections = connections.lock().unwrap();
                        connections.push(Vec::new());
                        connections.len() - 1
                    };
                    let (client_copy, server_copy) = (
                        client.try_clone().expect("clone client socket"),
                        server.try_clone().expect("clone server socket"),
                    );
                    thread::spawn({
                        let connections = connections.clone();
                        move || {
                            relay(client, server, Some((&connections, index)))
                                .expect("relay client")
                        }
                    });
                    thread::spawn(move || {
                        relay(server_copy, client_copy, None).expect("relay server")
                    });
                }
            }
        });
        Self { port, connections }
    }

    fn connections(&self) -> Vec<Vec<u8>> {
        self.connections.lock().unwrap().clone()
    }

    fn describe(&self) -> String {
        let summaries: Vec<String> = self
            .connections()
            .iter()
            .map(|bytes| {
                format!(
                    "{} bytes, first byte {:?}, password in clear: {}",
                    bytes.len(),
                    bytes.first(),
                    contains(bytes, FAKE_PASSWORD)
                )
            })
            .collect();
        format!("{} TCP connection(s): {summaries:?}", summaries.len())
    }
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn connect_with(
    uri: Option<String>,
    host: &str,
    port: u16,
    ssl_mode: Option<&str>,
    root_certificate: Option<&Path>,
) -> Result<Box<dyn Connection>, DbError> {
    let profile = ConnectionProfile::new(
        "redis-tls-modes",
        DbConfig::Redis {
            use_uri: uri.is_some(),
            uri,
            host: host.to_string(),
            port,
            user: None,
            database: Some(0),
            tls: false,
            ssl_mode: ssl_mode.map(str::to_string),
            ssl_root_cert_path: root_certificate.map(|path| path.display().to_string()),
            ssl_client_cert_path: None,
            ssl_client_key_path: None,
            ssh_tunnel: None,
            ssh_tunnel_profile_id: None,
            topology: None,
            sentinel_master_name: None,
            additional_nodes: None,
        },
    );
    let password = SecretString::from(FAKE_PASSWORD.to_string());
    RedisDriver::new().connect_with_secrets(&profile, Some(&password), None)
}

fn connect(
    host: &str,
    port: u16,
    ssl_mode: &str,
    root_certificate: Option<&Path>,
) -> Result<Box<dyn Connection>, DbError> {
    connect_with(None, host, port, Some(ssl_mode), root_certificate)
}

fn connect_uri(uri: String) -> Result<Box<dyn Connection>, DbError> {
    connect_with(Some(uri), "", 0, None, None)
}

/// `verify` refuses this server, and refuses before the password leaves.
fn assert_verify_refuses(port: u16, host: &str, root_certificate: Option<&Path>, case: &str) {
    let proxy = RecordingProxy::start(port);
    let Err(error) = connect(host, proxy.port, "verify", root_certificate) else {
        panic!("verify connected with {case}");
    };
    println!(
        "verify with {case}: refused: {error}; wire: {}",
        proxy.describe()
    );
    assert!(
        error.to_string().contains("certificate"),
        "verify with {case} failed for another reason: {error}"
    );
    assert!(
        proxy
            .connections()
            .iter()
            .all(|bytes| !contains(bytes, FAKE_PASSWORD)),
        "verify with {case} sent the password in clear"
    );
}

#[test]
#[ignore = "requires a Docker daemon, the redis:7-alpine image and the openssl command"]
fn each_tls_mode_does_what_the_form_says() {
    // The provider `crates/datatab/src/main.rs` installs in the app.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("no provider is installed yet");

    let server = TlsRedis::start();
    let port = server.port();
    let right_ca = server.certificate("ca.crt");
    let other_ca = server.certificate("other-ca.crt");

    // Proves the server and its certificate are good before any mode is judged.
    let trusted = containers::retry_db_operation(START_LIMIT, || {
        connect("localhost", port, "verify", Some(&right_ca))
    })
    .expect("verify with the right CA connects");
    trusted
        .execute(&QueryRequest::new("PING"))
        .expect("PING over verified TLS");
    println!("verify with the right CA: connected");

    assert_verify_refuses(port, "localhost", Some(&other_ca), "another CA");
    assert_verify_refuses(port, "localhost", None, "no CA, so the system roots");
    assert_verify_refuses(port, "127.0.0.1", Some(&right_ca), "a name not on the cert");

    // Proves the recorder can read clear text. The client's first command is
    // not AUTH, and the server closes the connection before AUTH is sent.
    let plain_proxy = RecordingProxy::start(port);
    let plain = connect("localhost", plain_proxy.port, "off", None);
    println!("off: wire: {}", plain_proxy.describe());
    assert!(plain.is_err(), "the server accepts only TLS");
    assert!(
        plain_proxy
            .connections()
            .iter()
            .any(|bytes| contains(bytes, "SETINFO")),
        "off should send readable commands to this recorder"
    );

    let proxy = RecordingProxy::start(port);
    let connection = connect("localhost", proxy.port, "on", None);
    println!("on: wire: {}", proxy.describe());
    let connection = match connection {
        Ok(connection) => connection,
        Err(error) => panic!("on did not connect: {error}"),
    };
    let key = "datatab-tls-mode-check";
    connection
        .execute(&QueryRequest::new(format!("SET {key} {CLEAR_TEXT_VALUE}")))
        .expect("SET over unverified TLS");
    let read = connection
        .execute(&QueryRequest::new(format!("GET {key}")))
        .expect("GET over unverified TLS");
    assert_eq!(read.text_body.as_deref(), Some(CLEAR_TEXT_VALUE));

    println!("on: after SET and GET, wire: {}", proxy.describe());
    let connections = proxy.connections();
    assert!(!connections.is_empty(), "on must have crossed the proxy");
    for bytes in &connections {
        assert_eq!(
            bytes.first(),
            Some(&TLS_HANDSHAKE_RECORD),
            "every connection of on starts with a TLS handshake"
        );
        assert!(!contains(bytes, FAKE_PASSWORD), "password crossed in clear");
        assert!(!contains(bytes, CLEAR_TEXT_VALUE), "value crossed in clear");
    }

    let Err(error) = connect_uri(format!("rediss://localhost:{port}/")) else {
        panic!("a rediss URI without #insecure connected to an untrusted server");
    };
    assert!(
        error.to_string().contains("certificate"),
        "a rediss URI failed for another reason: {error}"
    );
    connect_uri(format!("rediss://localhost:{port}/#insecure"))
        .expect("a rediss URI with #insecure connects")
        .execute(&QueryRequest::new("PING"))
        .expect("PING over a rediss URI with #insecure");
    println!("URI mode: rediss refused, rediss with #insecure connected");
}
