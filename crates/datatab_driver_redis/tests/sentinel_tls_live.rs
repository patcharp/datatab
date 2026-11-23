//! Functional Sentinel TLS checks using only redis:7-alpine throwaway containers.
//! This suite runs only on Linux because it uses host networking.
//! Certificates are generated below DATATAB_TEST_SCRATCH and removed on drop.
#![allow(
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::result_large_err
)]

#[path = "support/recording_proxy.rs"]
mod recording_proxy;
#[path = "support/container.rs"]
mod test_container;

use datatab_core::secrecy::SecretString;
use datatab_core::{Connection, ConnectionProfile, DbConfig, DbDriver, DbError, QueryRequest};
use datatab_driver_redis::RedisDriver;
use datatab_test_support::containers::retry_db_operation;
use recording_proxy::RecordingProxy;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Once;
use std::time::Duration;
use test_container::{TestContainer, command};

const START_LIMIT: Duration = Duration::from_secs(30);
const VALUE: &str = "sentinel-wire-value";
const MAKE_CERTIFICATES: &str = r#"set -e
new_key="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
for ca in ca other-ca; do
  openssl req -x509 $new_key -days 1 -keyout "$ca-private.pem" -out "$ca.crt" -subj "/CN=$ca"
done
for name in server mismatch untrusted client; do
  openssl req $new_key -keyout "$name-private.pem" -out "$name.csr" -subj "/CN=$name"
  case "$name" in
    server|untrusted) printf 'subjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth,clientAuth\n' > "$name.ext";;
    mismatch) printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth,clientAuth\n' > "$name.ext";;
    client) printf 'extendedKeyUsage=clientAuth\n' > "$name.ext";;
  esac
  authority=ca
  if [ "$name" = untrusted ]; then authority=other-ca; fi
  openssl x509 -req -days 1 -CAcreateserial -CA "$authority.crt" -CAkey "$authority-private.pem" \
    -in "$name.csr" -extfile "$name.ext" -out "$name.crt"
done
cat ca.crt other-ca.crt > server-ca.crt
"#;

const CONTAINER_SCRIPT: &str = "set -e; chown redis /run/redistls; \
    install -o redis -m 600 /tls/server-private.pem /run/redistls/server-private.pem; \
    install -o redis -m 644 /tls/server.crt /run/redistls/server.crt; \
    install -o redis -m 600 /tls/mismatch-private.pem /run/redistls/mismatch-private.pem; \
    install -o redis -m 644 /tls/mismatch.crt /run/redistls/mismatch.crt; \
    install -o redis -m 600 /tls/untrusted-private.pem /run/redistls/untrusted-private.pem; \
    install -o redis -m 644 /tls/untrusted.crt /run/redistls/untrusted.crt; \
    install -o redis -m 644 /tls/server-ca.crt /run/redistls/server-ca.crt; \
    install -o redis -m 600 /tls/$1.conf /data/redis.conf; shift; \
    exec docker-entrypoint.sh redis-server /data/redis.conf \"$@\"";

fn unused_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve port")
        .local_addr()
        .expect("reserved address")
        .port()
}

struct Topology {
    containers: Vec<TestContainer>,
    sentinel: RecordingProxy,
    master: RecordingProxy,
    replica: RecordingProxy,
    certificates: tempfile::TempDir,
    password: String,
    suffix: String,
}

impl Topology {
    fn start(
        test: &str,
        certificate: &str,
        master_client_auth: bool,
        sentinel_client_auth: bool,
    ) -> Self {
        static TLS_PROVIDER: Once = Once::new();
        TLS_PROVIDER.call_once(|| {
            if rustls::crypto::CryptoProvider::get_default().is_none() {
                rustls::crypto::aws_lc_rs::default_provider()
                    .install_default()
                    .expect("install test TLS provider");
            }
        });
        let scratch = std::env::var_os("DATATAB_TEST_SCRATCH")
            .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from);
        fs::create_dir_all(&scratch).expect("create test scratch");
        let certificates = tempfile::Builder::new()
            .prefix("sentinel-tls-")
            .tempdir_in(scratch)
            .expect("certificate directory");
        command(
            Command::new("sh")
                .args(["-c", MAKE_CERTIFICATES])
                .current_dir(certificates.path()),
        );
        let master_port = unused_port();
        let replica_port = unused_port();
        let sentinel_port = unused_port();
        let mut topology = Self {
            containers: Vec::new(),
            master: RecordingProxy::start(master_port),
            replica: RecordingProxy::start(replica_port),
            sentinel: RecordingProxy::start(sentinel_port),
            certificates,
            password: uuid::Uuid::new_v4().to_string(),
            suffix: format!("{test}-{}", uuid::Uuid::new_v4()),
        };
        topology.start_node(
            "master",
            master_port,
            certificate,
            master_client_auth,
            false,
        );
        topology.start_node(
            "replica",
            replica_port,
            certificate,
            master_client_auth,
            false,
        );
        topology.start_node(
            "sentinel",
            sentinel_port,
            "server",
            sentinel_client_auth,
            true,
        );
        topology.wait_until_ready();
        topology
    }

    fn start_node(
        &mut self,
        name: &str,
        port: u16,
        certificate: &str,
        client_auth: bool,
        sentinel: bool,
    ) {
        let mut config = format!(
            "bind 127.0.0.1\nport 0\ntls-port {port}\ntls-cert-file /run/redistls/{certificate}.crt\n\
             tls-key-file /run/redistls/{certificate}-private.pem\ntls-ca-cert-file /run/redistls/server-ca.crt\n\
             tls-auth-clients {}\ntls-replication yes\n",
            if client_auth { "yes" } else { "no" }
        );
        if sentinel {
            config.push_str(&format!(
                "sentinel monitor primary 127.0.0.1 {} 1\nsentinel auth-pass primary {}\n\
                 sentinel down-after-milliseconds primary 5000\nsentinel failover-timeout primary 10000\n",
                self.master.port, self.password));
        } else {
            config.push_str(&format!(
                "requirepass {}\nmasterauth {}\n",
                self.password, self.password
            ));
            if name == "replica" {
                config.push_str(&format!(
                    "replicaof 127.0.0.1 {}\nreplica-announce-ip 127.0.0.1\nreplica-announce-port {}\n",
                    self.master.port, self.replica.port));
            }
        }
        fs::write(
            self.certificates.path().join(format!("{name}.conf")),
            config,
        )
        .expect("write Redis config");
        let mut arguments = vec![
            "sh".into(),
            "-c".into(),
            CONTAINER_SCRIPT.into(),
            "fixture".into(),
            name.into(),
        ];
        if sentinel {
            arguments.push("--sentinel".into());
        }
        self.containers.push(TestContainer::start(
            &format!("datatab-main-redis-sentinel-{}-{name}", self.suffix),
            &[
                "--network=host".into(),
                "--entrypoint=sh".into(),
                "-v".into(),
                format!("{}:/tls:ro", self.certificates.path().display()),
            ],
            // --entrypoint=sh means the image arguments begin at -c.
            &arguments[1..],
        ));
    }

    fn control(&self, port: u16, sentinel: bool) -> redis::Connection {
        self.try_control(port, sentinel)
            .expect("control connection")
    }

    fn try_control(&self, port: u16, sentinel: bool) -> redis::RedisResult<redis::Connection> {
        // Control commands may skip the name check so the mismatched-master
        // fixture can become ready. Driver assertions always use their own mode.
        let client = redis::Client::build_with_tls(
            redis::ConnectionInfo {
                addr: redis::ConnectionAddr::TcpTls {
                    host: "127.0.0.1".into(),
                    port,
                    insecure: true,
                    tls_params: None,
                },
                redis: redis::RedisConnectionInfo {
                    password: if sentinel {
                        None
                    } else {
                        Some(self.password.clone())
                    },
                    ..Default::default()
                },
            },
            redis::TlsCertificates {
                root_cert: Some(fs::read(self.certificates.path().join("ca.crt")).expect("CA")),
                client_tls: Some(redis::ClientTlsConfig {
                    client_cert: fs::read(self.certificates.path().join("client.crt"))
                        .expect("client certificate"),
                    client_key: fs::read(self.certificates.path().join("client-private.pem"))
                        .expect("client key"),
                }),
            },
        )
        .expect("build control client");
        client.get_connection()
    }

    fn wait_until_ready(&self) {
        for container in &self.containers {
            retry_db_operation(START_LIMIT, || {
                let output = Command::new("docker")
                    .args([
                        "exec",
                        &container.name,
                        "redis-cli",
                        "--tls",
                        "--insecure",
                        "--cert",
                        "/run/redistls/server.crt",
                        "--key",
                        "/run/redistls/server-private.pem",
                        "-p",
                    ])
                    .arg(self.backend_port(container))
                    .arg("PING")
                    .output()
                    .expect("readiness command");
                if String::from_utf8_lossy(&output.stdout).contains("PONG")
                    || String::from_utf8_lossy(&output.stdout).contains("NOAUTH")
                {
                    Ok(())
                } else {
                    Err("not ready")
                }
            })
            .expect("Redis TLS node starts");
        }
        retry_db_operation(START_LIMIT, || {
            let info: String = redis::cmd("INFO")
                .arg("replication")
                .query(&mut self.control(self.replica.port, false))
                .expect("replica info");
            if info.contains("master_link_status:up") {
                Ok(())
            } else {
                Err("replication pending")
            }
        })
        .expect("TLS replication ready");
        retry_db_operation(START_LIMIT, || {
            let replicas: Vec<redis::Value> = redis::cmd("SENTINEL")
                .arg("REPLICAS")
                .arg("primary")
                .query(&mut self.control(self.sentinel.port, true))
                .expect("Sentinel replicas");
            if replicas.len() == 1 {
                Ok(())
            } else {
                Err("replica discovery pending")
            }
        })
        .expect("Sentinel knows the replica");
    }

    fn backend_port(&self, container: &TestContainer) -> String {
        let role = container.name.rsplit('-').next().expect("container role");
        let config = fs::read_to_string(self.certificates.path().join(format!("{role}.conf")))
            .expect("read generated fixture config");
        config
            .lines()
            .find_map(|line| line.strip_prefix("tls-port "))
            .expect("TLS port")
            .to_string()
    }

    fn profile(&self, mode: &str, root: &str, client: bool) -> ConnectionProfile {
        ConnectionProfile::new(
            "sentinel-tls",
            DbConfig::Redis {
                use_uri: false,
                uri: None,
                host: "127.0.0.1".into(),
                port: self.sentinel.port,
                user: Some("default".into()),
                database: Some(2),
                tls: false,
                ssl_mode: Some(mode.into()),
                ssl_root_cert_path: Some(self.certificates.path().join(root).display().to_string()),
                ssl_client_cert_path: client.then(|| {
                    self.certificates
                        .path()
                        .join("client.crt")
                        .display()
                        .to_string()
                }),
                ssl_client_key_path: client.then(|| {
                    self.certificates
                        .path()
                        .join("client-private.pem")
                        .display()
                        .to_string()
                }),
                ssh_tunnel: None,
                ssh_tunnel_profile_id: None,
                topology: Some("sentinel".into()),
                sentinel_master_name: Some("primary".into()),
                additional_nodes: None,
            },
        )
    }

    fn connect(&self, profile: &ConnectionProfile) -> Result<Box<dyn Connection>, DbError> {
        RedisDriver::new().connect_with_secrets(
            profile,
            Some(&SecretString::from(self.password.clone())),
            None,
        )
    }

    fn failover_and_disconnect_old_clients(&self) {
        let mut sentinel = self.control(self.sentinel.port, true);
        redis::cmd("SENTINEL")
            .arg("FAILOVER")
            .arg("primary")
            .query::<String>(&mut sentinel)
            .expect("request failover");
        retry_db_operation(START_LIMIT, || {
            let address: (String, u16) = redis::cmd("SENTINEL")
                .arg("GET-MASTER-ADDR-BY-NAME")
                .arg("primary")
                .query(&mut sentinel)
                .expect("new master address");
            if address.1 == self.replica.port {
                Ok(())
            } else {
                Err("promotion pending")
            }
        })
        .expect("Sentinel announces the promoted replica");
        retry_db_operation(START_LIMIT, || {
            let mut connection = self.try_control(self.master.port, false)?;
            let role: redis::Role = redis::cmd("ROLE").query(&mut connection)?;
            if matches!(role, redis::Role::Replica { .. }) {
                Ok(())
            } else {
                Err(redis::RedisError::from((
                    redis::ErrorKind::ResponseError,
                    "demotion pending",
                )))
            }
        })
        .expect("old master demoted");
        // Force the old client socket to close, exercising the existing one-shot
        // connection-failure retry rather than reconnecting the driver in the test.
        redis::cmd("CLIENT")
            .arg("KILL")
            .arg("TYPE")
            .arg("normal")
            .arg("SKIPME")
            .arg("yes")
            .query::<i64>(&mut self.control(self.master.port, false))
            .expect("close old master clients");
    }

    fn assert_role_required_before_retry(&self, connection: &dyn Connection) {
        let mut promoted = self.control(self.replica.port, false);
        redis::cmd("ACL")
            .arg("SETUSER")
            .arg("default")
            .arg("-role")
            .query::<String>(&mut promoted)
            .expect("deny ROLE on new master");
        let error = connection
            .execute(&QueryRequest::new(format!(
                "SET sentinel-after-failover {VALUE}"
            )))
            .expect_err("re-resolve must require ROLE before retrying the write");
        assert!(
            error.to_string().contains("role"),
            "must report the denied ROLE: {error}"
        );
        let value: Option<String> = redis::cmd("GET")
            .arg("sentinel-after-failover")
            .query(&mut promoted)
            .expect("check rejected write");
        assert!(
            value.is_none(),
            "retry must not run before the role check succeeds"
        );
        redis::cmd("ACL")
            .arg("SETUSER")
            .arg("default")
            .arg("+role")
            .query::<String>(&mut promoted)
            .expect("allow ROLE on new master");
    }

    fn assert_wires(&self) {
        for proxy in [&self.sentinel, &self.master, &self.replica] {
            proxy.assert_tls_only(&[&self.password, VALUE, "GET-MASTER-ADDR-BY-NAME"]);
        }
    }
}

#[test]
#[ignore = "requires Docker, local redis:7-alpine and openssl"]
fn test_verified_unverified_and_legacy_tls_and_failover() {
    let topology = Topology::start("failover", "server", false, false);
    let connection = topology
        .connect(&topology.profile("verify", "ca.crt", false))
        .expect("private CA verifies both legs");
    connection
        .execute(&QueryRequest::new(format!("SET sentinel-check {VALUE}")))
        .expect("verified write");
    assert_eq!(
        connection
            .execute(&QueryRequest::new("GET sentinel-check"))
            .expect("verified read")
            .text_body
            .as_deref(),
        Some(VALUE)
    );
    topology
        .connect(&topology.profile("on", "other-ca.crt", false))
        .expect("mode on encrypts both legs")
        .execute(&QueryRequest::new("PING"))
        .expect("unverified TLS PING");
    let mut legacy = topology.profile("on", "ca.crt", false);
    if let DbConfig::Redis { tls, ssl_mode, .. } = &mut legacy.config {
        *tls = true;
        *ssl_mode = None;
    }
    topology
        .connect(&legacy)
        .expect("legacy TLS true")
        .ping()
        .expect("legacy PING");
    let mut additional = topology.profile("verify", "ca.crt", false);
    if let DbConfig::Redis {
        port,
        additional_nodes,
        ..
    } = &mut additional.config
    {
        *port = 1;
        *additional_nodes = Some(format!("127.0.0.1:{}", topology.sentinel.port));
    }
    topology
        .connect(&additional)
        .expect("additional Sentinel keeps private CA TLS")
        .ping()
        .expect("additional PING");
    let mut unknown = additional.clone();
    if let DbConfig::Redis {
        sentinel_master_name,
        ..
    } = &mut unknown.config
    {
        *sentinel_master_name = Some("unknown-primary".into());
    }
    let error = match topology.connect(&unknown) {
        Err(error) => error,
        Ok(_) => panic!("unknown master was accepted"),
    };
    let message = error.to_string();
    assert!(
        message.contains("did not return a master address"),
        "must report the reachable node's missing master: {message}"
    );
    assert!(
        message.to_ascii_lowercase().contains("connection refused"),
        "must preserve the earlier node's connection failure: {message}"
    );

    topology.failover_and_disconnect_old_clients();
    topology.assert_role_required_before_retry(connection.as_ref());
    connection
        .execute(&QueryRequest::new(format!(
            "SET sentinel-after-failover {VALUE}"
        )))
        .expect("one TLS re-resolve and command retry after failover");
    assert_eq!(
        connection
            .execute(&QueryRequest::new("GET sentinel-after-failover"))
            .expect("new master read")
            .text_body
            .as_deref(),
        Some(VALUE)
    );
    topology.assert_wires();
}

#[test]
#[ignore = "requires Docker, local redis:7-alpine and openssl"]
fn test_mutual_tls_to_sentinel_and_master() {
    let topology = Topology::start("mutual", "server", true, true);
    let profile = topology.profile("verify", "ca.crt", true);
    topology
        .connect(&profile)
        .expect("mTLS authenticates to both Sentinel and master")
        .execute(&QueryRequest::new("PING"))
        .expect("mTLS command");
    topology.assert_wires();
}

#[test]
#[ignore = "requires Docker, local redis:7-alpine and openssl"]
fn test_master_san_mismatch_is_refused_after_sentinel_tls_succeeds() {
    let topology = Topology::start("san", "mismatch", false, false);
    assert_master_tls_refused(&topology, "not valid for name");
    topology.assert_wires();
}

#[test]
#[ignore = "requires Docker, local redis:7-alpine and openssl"]
fn test_master_wrong_ca_is_refused_after_sentinel_tls_succeeds() {
    let topology = Topology::start("wrong-ca", "untrusted", false, false);
    assert_master_tls_refused(&topology, "UnknownIssuer");
    topology.assert_wires();
}

#[test]
#[ignore = "requires Docker, local redis:7-alpine and openssl"]
fn test_master_missing_client_certificate_is_refused_after_sentinel_tls_succeeds() {
    let topology = Topology::start("missing-client", "server", true, false);
    assert_master_tls_refused(&topology, "CertificateRequired");
    topology.assert_wires();
}

fn assert_master_tls_refused(topology: &Topology, reason: &str) {
    let error = match topology.connect(&topology.profile("verify", "ca.crt", false)) {
        Err(error) => error,
        Ok(_) => panic!("invalid master TLS was accepted"),
    };
    let message = error.to_string();
    assert!(
        message.contains(&format!("master 127.0.0.1:{}", topology.master.port)),
        "must identify the master leg and address: {message}"
    );
    assert!(
        message.contains(reason),
        "must report the master TLS failure ({reason}): {message}"
    );
}
