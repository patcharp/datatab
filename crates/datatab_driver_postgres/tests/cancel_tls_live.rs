//! Cancels a running query on a server that accepts logins only over TLS,
//! through a proxy that records what each connection sends.
//!
//! Needs Docker, the local `postgres:17-alpine` image and the `openssl`
//! command. Certificates are generated when the test runs and removed after it.
//!
//! ```text
//! DATATAB_TEST_SLOT=<slot> DATATAB_TEST_SCRATCH=<folder> \
//!   cargo test -p datatab_driver_postgres --test cancel_tls_live --locked \
//!   -- --ignored --nocapture
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::result_large_err
)]

use datatab_core::secrecy::SecretString;
use datatab_core::{Connection, ConnectionProfile, DbConfig, DbDriver, DbError, QueryRequest};
use datatab_driver_postgres::PostgresDriver;
use datatab_test_support::containers;
use native_tls::{Identity, TlsAcceptor};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const IMAGE: &str = "postgres:17-alpine";
const FAKE_PASSWORD: &str = "not-a-real-password";
const SLOW_QUERY: &str = "SELECT pg_sleep(10)";
const FROM_SLOW_QUERY: &str = "FROM pg_stat_activity WHERE state = 'active' \
     AND query LIKE 'SELECT pg_sleep(%' AND pid <> pg_backend_pid()";
const STOP_WAIT: Duration = Duration::from_secs(3);
const RECORDED_PREFIX: usize = 64;

const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xD2, 0x16, 0x2F];
const CANCEL_REQUEST: [u8; 8] = [0, 0, 0, 16, 0x04, 0xD2, 0x16, 0x2E];
const TLS_HANDSHAKE_RECORD: u8 = 0x16;
const WIRE_OVER_TLS: &str = "SSLRequest, then a TLS handshake";
const WIRE_CLEAR_TEXT: &str = "CancelRequest in clear text";

const PG_HBA: &str = "local all all trust\n\
     hostssl all all 0.0.0.0/0 scram-sha-256\n\
     hostnossl all all 0.0.0.0/0 reject\n";

const MAKE_CERTIFICATES: &str = r#"set -e
new_key="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
self_signed() {
  openssl req -x509 $new_key -days 1 -keyout "$1.key" -out "$1.crt" \
    -subj "/CN=$2" -addext "subjectAltName=DNS:$2"
}
issued() {
  openssl req $new_key -keyout "$1.key" -out "$1.csr" -subj "/CN=$2"
  printf 'subjectAltName=DNS:%s\n' "$2" > "$1.ext"
  openssl x509 -req -days 1 -CAcreateserial -CA ca.crt -CAkey ca.key \
    -in "$1.csr" -extfile "$1.ext" -out "$1.crt"
}
self_signed ca datatab-test-ca
self_signed untrusted localhost
issued server localhost
issued wrong-name wrong-name.invalid
"#;

/// The server gets its own copy of the key, because PostgreSQL refuses a key
/// file that the host user owns.
const CONTAINER_SCRIPT: &str = "install -d -o postgres -m 700 /run/pgtls \
     && install -o postgres -m 600 /tls/server.key /run/pgtls/server.key \
     && install -o postgres -m 644 /tls/server.crt /run/pgtls/server.crt \
     && install -o postgres -m 600 /tls/pg_hba.conf /run/pgtls/pg_hba.conf \
     && exec docker-entrypoint.sh postgres -c ssl=on \
     -c ssl_cert_file=/run/pgtls/server.crt -c ssl_key_file=/run/pgtls/server.key \
     -c hba_file=/run/pgtls/pg_hba.conf";

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

/// A container that accepts logins only over TLS. Dropping it removes the
/// container and the throwaway certificates.
struct TlsPostgres {
    container: String,
    certificates: PathBuf,
}

impl TlsPostgres {
    fn start() -> Self {
        let scratch = std::env::var_os("DATATAB_TEST_SCRATCH")
            .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
        let certificates = scratch.join("cancel-tls-certificates");
        fs::create_dir_all(&certificates).expect("create certificate folder");
        fs::write(certificates.join("pg_hba.conf"), PG_HBA).expect("write pg_hba.conf");
        run_command(
            Command::new("sh")
                .args(["-c", MAKE_CERTIFICATES])
                .current_dir(&certificates),
        );

        let slot = std::env::var("DATATAB_TEST_SLOT").unwrap_or_else(|_| "local".to_string());
        let container = format!("datatab-{slot}-postgres");
        run_command(
            Command::new("docker")
                .args(["run", "-d", "--name", &container, "--memory=512m"])
                .args(["--tmpfs", "/var/lib/postgresql/data"])
                .args(["-p", "127.0.0.1::5432"])
                .args(["-v", &format!("{}:/tls:ro", certificates.display())])
                .args(["-e", &format!("POSTGRES_PASSWORD={FAKE_PASSWORD}")])
                .args(["--entrypoint", "sh", IMAGE, "-c", CONTAINER_SCRIPT]),
        );

        Self {
            container,
            certificates,
        }
    }

    fn port(&self) -> u16 {
        let published =
            run_command(Command::new("docker").args(["port", &self.container, "5432/tcp"]));
        let port = published.rsplit(':').next().expect("published port");
        port.parse().expect("published port number")
    }

    fn impostor(&self, file_stem: &str) -> Route {
        let read = |extension: &str| {
            fs::read(self.certificates.join(format!("{file_stem}.{extension}")))
                .expect("read impostor certificate")
        };
        let identity = Identity::from_pkcs8(&read("crt"), &read("key")).expect("load identity");
        Route::Impostor(TlsAcceptor::new(identity).expect("build acceptor"))
    }
}

impl Drop for TlsPostgres {
    fn drop(&mut self) {
        Command::new("docker")
            .args(["rm", "-f", &self.container])
            .output()
            .expect("failed to remove the container");
        fs::remove_dir_all(&self.certificates).expect("failed to remove the certificates");
    }
}

/// One connection the proxy accepted: what the client sent, and how the proxy's
/// threads for it ended.
#[derive(Default)]
struct ProxiedSocket {
    recording: Mutex<Vec<u8>>,
    end: Mutex<Option<Result<(), String>>>,
    ended: Condvar,
}

impl ProxiedSocket {
    fn finish(&self, end: io::Result<()>) {
        *self.end.lock().unwrap() = Some(end.map_err(|error| error.to_string()));
        self.ended.notify_all();
    }

    fn wait_for_end(&self) -> Result<(), String> {
        let end = self.end.lock().unwrap();
        let (end, _) = self
            .ended
            .wait_timeout_while(end, STOP_WAIT, |end| end.is_none())
            .unwrap();
        let unfinished = || Err("the proxy did not finish this connection".to_string());
        end.clone().unwrap_or_else(unfinished)
    }
}

/// A peer closing or resetting its socket ends a proxied stream normally. A TLS
/// client that closes with session tickets unread resets it, for one.
fn ended_normally(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::NotConnected
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

#[derive(Clone)]
enum Route {
    Forward,
    RefuseTls,
    Impostor(TlsAcceptor),
}

/// Sits between the client and the server. A connection takes the route set
/// when it arrives, so a test can change what the cancel request meets without
/// touching the session that runs the query.
#[derive(Clone)]
struct RecordingProxy {
    port: u16,
    upstream_port: u16,
    route: Arc<Mutex<Route>>,
    connections: Arc<Mutex<Vec<Arc<ProxiedSocket>>>>,
    impostor_saw_cancel: Arc<AtomicBool>,
}

impl RecordingProxy {
    fn start(upstream_port: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let proxy = Self {
            port: listener.local_addr().expect("proxy address").port(),
            upstream_port,
            route: Arc::new(Mutex::new(Route::Forward)),
            connections: Arc::default(),
            impostor_saw_cancel: Arc::default(),
        };

        std::thread::spawn({
            let proxy = proxy.clone();
            move || {
                for client in listener.incoming().flatten() {
                    let proxy = proxy.clone();
                    let socket = Arc::<ProxiedSocket>::default();
                    proxy.connections.lock().unwrap().push(socket.clone());
                    std::thread::spawn(move || socket.finish(proxy.serve(client, &socket)));
                }
            }
        });

        proxy
    }

    fn serve(&self, mut client: TcpStream, socket: &ProxiedSocket) -> io::Result<()> {
        let recording = &socket.recording;
        let route = self.route.lock().unwrap().clone();

        if matches!(route, Route::Forward) {
            let mut upstream = TcpStream::connect(("127.0.0.1", self.upstream_port))?;
            let (mut reply_from, mut reply_to) = (upstream.try_clone()?, client.try_clone()?);
            let reply = std::thread::spawn(move || {
                ended_normally(io::copy(&mut reply_from, &mut reply_to).map(drop))
            });
            let sent = ended_normally(copy_recording(&mut client, &mut upstream, recording));
            ended_normally(upstream.shutdown(Shutdown::Write))?;
            sent?;
            return reply.join().expect("proxy reply thread panicked");
        }

        client.set_read_timeout(Some(STOP_WAIT))?;
        let mut first_message = [0u8; 8];
        client.read_exact(&mut first_message)?;
        recording.lock().unwrap().extend_from_slice(&first_message);
        let asked_for_tls = first_message == SSL_REQUEST;

        if let (Route::Impostor(acceptor), true) = (route, asked_for_tls) {
            client.write_all(b"S")?;
            let mut session = acceptor.accept(client).map_err(io::Error::other)?;
            let mut decrypted = [0u8; 8];
            session.read_exact(&mut decrypted)?;
            let saw_cancel = decrypted == CANCEL_REQUEST;
            self.impostor_saw_cancel.store(saw_cancel, Ordering::SeqCst);
            return Ok(());
        }

        if asked_for_tls {
            client.write_all(b"N")?;
        }
        ended_normally(copy_recording(&mut client, &mut io::sink(), recording))
    }
}

fn copy_recording(
    from: &mut impl Read,
    to: &mut impl Write,
    recording: &Mutex<Vec<u8>>,
) -> io::Result<()> {
    let mut buffer = [0u8; 4096];
    loop {
        let read = from.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }

        let mut recording = recording.lock().unwrap();
        let room = RECORDED_PREFIX.saturating_sub(recording.len());
        recording.extend_from_slice(&buffer[..read.min(room)]);
        drop(recording);

        to.write_all(&buffer[..read])?;
    }
}

fn describe_wire(bytes: &[u8]) -> &'static str {
    let after_ssl_request = bytes.strip_prefix(&SSL_REQUEST);
    match after_ssl_request.unwrap_or(bytes) {
        rest if rest.starts_with(&CANCEL_REQUEST) => WIRE_CLEAR_TEXT,
        [TLS_HANDSHAKE_RECORD, ..] if after_ssl_request.is_some() => WIRE_OVER_TLS,
        [] if after_ssl_request.is_some() => "SSLRequest, then nothing",
        _ => "other bytes",
    }
}

/// Each connect path pairs with one public cancel call. The two pairs together
/// cover both connect paths and both calls a test can reach.
#[derive(Clone, Copy, Debug)]
enum CancelPath {
    FormThenHandle,
    UriThenActive,
}

fn connect(
    ssl_mode: &str,
    port: u16,
    path: CancelPath,
    ca_path: &Path,
) -> Result<Box<dyn Connection>, DbError> {
    let ca_path = ca_path.display().to_string();
    let uri = matches!(path, CancelPath::UriThenActive).then(|| {
        format!(
            "postgres://postgres:{FAKE_PASSWORD}@localhost:{port}/postgres\
             ?sslmode={ssl_mode}&sslrootcert={ca_path}"
        )
    });
    let profile = ConnectionProfile::new(
        "cancel-tls",
        DbConfig::Postgres {
            use_uri: uri.is_some(),
            uri,
            host: "localhost".to_string(),
            port,
            user: "postgres".to_string(),
            database: "postgres".to_string(),
            ssl_mode: Some(ssl_mode.to_string()),
            ssl_root_cert_path: Some(ca_path),
            ssl_client_cert_path: None,
            ssl_client_key_path: None,
            ssh_tunnel: None,
            ssh_tunnel_profile_id: None,
        },
    );

    let password = SecretString::from(FAKE_PASSWORD.to_string());
    PostgresDriver::new().connect_with_secrets(&profile, Some(&password), None)
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What one cancel attempt did: the call's result, whether the query really
/// stopped on the server, what the new connections sent, and how the proxy's
/// threads for them ended.
struct Observed {
    cancel: Result<(), DbError>,
    stopped: bool,
    query_cancelled: bool,
    wire: Vec<&'static str>,
    sockets: Vec<Result<(), String>>,
}

impl Observed {
    fn one_socket_ending(&self, cleanly: bool) -> bool {
        matches!(self.sockets.as_slice(), [end] if end.is_ok() == cleanly)
    }
}

struct Scenarios<'a> {
    server: &'a TlsPostgres,
    watcher: &'a dyn Connection,
    proxy: &'a RecordingProxy,
    failures: Vec<String>,
}

impl Scenarios<'_> {
    fn slow_queries_running(&self) -> usize {
        let sql = format!("SELECT pid {FROM_SLOW_QUERY}");
        let result = self.watcher.execute(&QueryRequest::new(sql));
        result.expect("watcher query failed").rows.len()
    }

    /// Runs a slow query through the proxy, sends the cancel along `route`, and
    /// stops the query itself if the cancel did not.
    fn observe(&self, ssl_mode: &str, path: CancelPath, route: Route) -> Observed {
        *self.proxy.route.lock().unwrap() = Route::Forward;
        let ca_path = self.server.certificates.join("ca.crt");
        let connection =
            connect(ssl_mode, self.proxy.port, path, &ca_path).expect("connect failed");
        let connection = &*connection;
        let stop_wait = match route {
            Route::Forward => STOP_WAIT,
            Route::RefuseTls | Route::Impostor(_) => Duration::ZERO,
        };

        std::thread::scope(|scope| {
            let running = scope.spawn(|| connection.execute(&QueryRequest::new(SLOW_QUERY)));
            let started = wait_until(STOP_WAIT, || self.slow_queries_running() > 0);
            assert!(started, "the slow query never started");

            let first_new_connection = self.proxy.connections.lock().unwrap().len();
            *self.proxy.route.lock().unwrap() = route;
            let cancel = match path {
                CancelPath::FormThenHandle => connection.cancel_handle().cancel(),
                CancelPath::UriThenActive => connection.cancel_active(),
            };
            let stopped = wait_until(stop_wait, || {
                running.is_finished() && self.slow_queries_running() == 0
            });

            // The client can return before the proxy has accepted its socket.
            // A socket that never comes fails the scenario's socket check.
            wait_until(STOP_WAIT, || {
                self.proxy.connections.lock().unwrap().len() > first_new_connection
            });
            let new_sockets =
                self.proxy.connections.lock().unwrap()[first_new_connection..].to_vec();
            let sockets = new_sockets
                .iter()
                .map(|socket| socket.wait_for_end())
                .collect();

            if !stopped {
                let sql = format!("SELECT pg_terminate_backend(pid) {FROM_SLOW_QUERY}");
                let terminated = self.watcher.execute(&QueryRequest::new(sql));
                terminated.expect("failed to stop the slow query");
            }
            let query = running.join().expect("query thread panicked");

            let wire = new_sockets
                .iter()
                .map(|socket| describe_wire(&socket.recording.lock().unwrap()))
                .collect();

            Observed {
                cancel,
                stopped,
                query_cancelled: matches!(query, Err(DbError::Cancelled)),
                wire,
                sockets,
            }
        })
    }

    fn report(&mut self, label: &str, observed: &Observed, checks: &[(bool, &str)]) {
        println!(
            "{label}: cancel={:?} query_stopped={} query_reported_cancelled={} wire={:?} \
             proxy_threads={:?}",
            observed.cancel,
            observed.stopped,
            observed.query_cancelled,
            observed.wire,
            observed.sockets
        );
        let failed = checks.iter().filter(|(passed, _)| !passed);
        self.failures
            .extend(failed.map(|(_, check)| format!("{label}: expected {check}")));
    }

    /// The cancel reaches the real server over TLS and stops the query.
    fn expect_cancelled_over_tls(&mut self, ssl_mode: &str, path: CancelPath) {
        let observed = self.observe(ssl_mode, path, Route::Forward);
        self.report(
            &format!("{ssl_mode} {path:?}"),
            &observed,
            &[
                (observed.cancel.is_ok(), "the cancel call to succeed"),
                (observed.stopped, "the query to stop"),
                (observed.query_cancelled, "the query to report a cancel"),
                (observed.wire == [WIRE_OVER_TLS], "one TLS connection"),
                (observed.one_socket_ending(true), "a clean proxied socket"),
            ],
        );
    }

    /// The server refuses TLS on the cancel connection. `prefer` accepts that,
    /// as it does when connecting; every stricter mode keeps the key to itself.
    fn expect_refused_tls(&mut self, ssl_mode: &str, sends_clear_text: bool) {
        let observed = self.observe(ssl_mode, CancelPath::UriThenActive, Route::RefuseTls);
        let clear_text = observed.wire.contains(&WIRE_CLEAR_TEXT);
        self.report(
            &format!("{ssl_mode} with TLS refused on cancel"),
            &observed,
            &[
                (observed.cancel.is_ok() == sends_clear_text, "cancel result"),
                (clear_text == sends_clear_text, "clear text cancel or none"),
                (observed.one_socket_ending(true), "a clean refusal socket"),
            ],
        );
    }

    /// A different server answers the cancel connection. The connection's
    /// certificate checks decide whether it receives the cancel key.
    fn expect_impostor(&mut self, ssl_mode: &str, certificate: &str, accepted: bool) {
        let saw_cancel = self.proxy.impostor_saw_cancel.clone();
        saw_cancel.store(false, Ordering::SeqCst);
        let route = self.server.impostor(certificate);
        let observed = self.observe(ssl_mode, CancelPath::UriThenActive, route);
        self.report(
            &format!("{ssl_mode} with impostor certificate {certificate}"),
            &observed,
            &[
                (observed.cancel.is_ok() == accepted, "cancel result"),
                (
                    saw_cancel.load(Ordering::SeqCst) == accepted,
                    "key delivery",
                ),
                (observed.one_socket_ending(accepted), "the handshake result"),
            ],
        );
    }
}

#[test]
#[ignore = "requires a Docker daemon, the postgres:17-alpine image and the openssl command"]
fn cancel_follows_the_tls_of_the_connection() {
    let server = TlsPostgres::start();
    let port = server.port();
    let ca_path = server.certificates.join("ca.crt");
    let path = CancelPath::FormThenHandle;

    let watcher = containers::retry_db_operation(Duration::from_secs(30), || {
        connect("require", port, path, &ca_path)
    })
    .expect("the server never accepted a TLS login");

    assert!(
        connect("disable", port, path, &ca_path).is_err(),
        "the server must refuse a login without TLS"
    );

    let proxy = RecordingProxy::start(port);
    let mut scenarios = Scenarios {
        server: &server,
        watcher: &*watcher,
        proxy: &proxy,
        failures: Vec::new(),
    };

    for ssl_mode in ["prefer", "require", "verify-ca", "verify-full"] {
        scenarios.expect_cancelled_over_tls(ssl_mode, CancelPath::FormThenHandle);
        scenarios.expect_cancelled_over_tls(ssl_mode, CancelPath::UriThenActive);
    }

    scenarios.expect_refused_tls("prefer", true);
    for ssl_mode in ["require", "verify-ca", "verify-full"] {
        scenarios.expect_refused_tls(ssl_mode, false);
    }

    scenarios.expect_impostor("verify-ca", "untrusted", false);
    scenarios.expect_impostor("verify-full", "wrong-name", false);
    // The chain is valid and verify-ca skips the name, so this impostor is
    // accepted. That shows the two refusals above come from the certificate
    // checks and not from an impostor that never works.
    scenarios.expect_impostor("verify-ca", "wrong-name", true);

    assert!(
        scenarios.failures.is_empty(),
        "{}",
        scenarios.failures.join("\n")
    );
}
