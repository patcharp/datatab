use std::time::{Duration, Instant};
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::SyncRunner;
use testcontainers::{GenericImage, ImageExt};

/// Google's pull-through mirror of Docker Hub.
///
/// CI runners share NAT'd addresses, so anonymous Docker Hub pulls hit the
/// per-IP rate limit and abort mid-stream — the failure the workflow's
/// serialised pre-pull step exists to work around. The mirror has no such
/// limit and needs no credentials, which also keeps it working for pull
/// requests from forks, where secrets are unavailable.
const MIRROR: &str = "mirror.gcr.io";

/// A Docker Hub image, pulled from [`MIRROR`] instead.
///
/// Official images live under `library/` on the mirror, which is implicit on
/// Docker Hub: `postgres` is `library/postgres` here. A name that already
/// carries an organisation keeps it.
fn hub_image(image: &str, tag: &str) -> GenericImage {
    // The name and the tag stay apart: testcontainers joins them as
    // `{name}:{tag}`, so a tag in the name would be written twice.
    GenericImage::new(mirror_repository(image), tag.to_string())
}

/// The mirror's repository for a Docker Hub image, without a tag.
fn mirror_repository(image: &str) -> String {
    if image.contains('/') {
        format!("{MIRROR}/{image}")
    } else {
        format!("{MIRROR}/library/{image}")
    }
}

/// The mirror's full image reference, for the helpers that shell out to
/// `docker` instead of going through `testcontainers`.
fn mirror_image_name(image: &str, tag: &str) -> String {
    format!("{}:{tag}", mirror_repository(image))
}

#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_postgres_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    let image = hub_image("postgres", "16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_DB", "postgres");

    let container = image.start().expect("failed to start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .expect("failed to get postgres host port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    run(url)
}

/// Starts a throwaway PostgreSQL 16 container that authenticates with `trust`
/// (no password at all) and returns its host port.
///
/// `POSTGRES_USER` doubles as the superuser name and the default database, so
/// a deliberately different database name (`testdb`) makes a swallowed
/// `dbname=` connection parameter observable instead of silently falling back
/// to the user name.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_trust_postgres_port<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(u16) -> Result<T, E>,
{
    let image = hub_image("postgres", "16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "testuser")
        .with_env_var("POSTGRES_DB", "testdb")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust");

    let container = image.start().expect("failed to start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .expect("failed to get postgres host port");

    run(port)
}

#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_pgvector_postgres_16_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    let image = hub_image("pgvector/pgvector", "0.8.0-pg16")
        .with_exposed_port(ContainerPort::Tcp(5432))
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_DB", "postgres");

    let container = image
        .start()
        .expect("failed to start pgvector postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .expect("failed to get pgvector postgres host port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    run(url)
}

pub fn with_mysql_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    with_mysql_image_url(hub_image("mysql", "8.4"), run)
}

/// MySQL 5.7 predates `information_schema` tables such as `CHECK_CONSTRAINTS`,
/// so it covers the fallbacks the driver keeps for older servers.
pub fn with_mysql_57_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    with_mysql_image_url(hub_image("mysql", "5.7"), run)
}

#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
fn with_mysql_image_url<T, E, F>(image: GenericImage, run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    let image = image
        .with_exposed_port(ContainerPort::Tcp(3306))
        .with_wait_for(WaitFor::message_on_stderr("ready for connections"))
        .with_env_var("MYSQL_ROOT_PASSWORD", "root")
        .with_env_var("MYSQL_DATABASE", "testdb");

    let container = image.start().expect("failed to start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .expect("failed to get mysql host port");
    let url = format!("mysql://root:root@127.0.0.1:{port}/testdb");

    run(url)
}

#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_mongodb_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    let image = hub_image("mongo", "7")
        .with_exposed_port(ContainerPort::Tcp(27017))
        .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"));

    let container = image.start().expect("failed to start mongo container");
    let port = container
        .get_host_port_ipv4(27017)
        .expect("failed to get mongo host port");
    let url = format!("mongodb://127.0.0.1:{port}/testdb");

    run(url)
}

struct RedisContainer(String);

impl Drop for RedisContainer {
    fn drop(&mut self) {
        let result = std::process::Command::new("docker")
            .args(["rm", "-f", "-v", &self.0])
            .output();
        if !matches!(&result, Ok(output) if output.status.success()) {
            eprintln!("failed to remove redis container {}: {result:?}", self.0);
            assert!(std::thread::panicking(), "redis container cleanup failed");
        }
    }
}

pub fn with_redis_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    with_redis_container(|url, _container| run(url))
}

/// Runs the callback with a Redis URL and container name for `docker exec`.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures"
)]
pub fn with_redis_container<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String, &str) -> Result<T, E>,
{
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_NAME: AtomicU64 = AtomicU64::new(0);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let name = format!(
        "datatab-main-redis-{}-{timestamp}-{}",
        std::process::id(),
        NEXT_NAME.fetch_add(1, Ordering::Relaxed)
    );
    let output = std::process::Command::new("docker")
        .args([
            "create",
            "--name",
            &name,
            "--memory=256m",
            "--tmpfs",
            "/data",
            "-p",
            "127.0.0.1::6379",
            "mirror.gcr.io/library/redis:7",
        ])
        .output()
        .expect("create redis container");
    assert!(
        output.status.success(),
        "create redis container: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let container = RedisContainer(name);
    let output = std::process::Command::new("docker")
        .args(["start", &container.0])
        .output()
        .expect("start redis container");
    assert!(
        output.status.success(),
        "start redis container: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    retry_db_operation(Duration::from_secs(30), || {
        let output = std::process::Command::new("docker")
            .args(["logs", &container.0])
            .output()?;
        if String::from_utf8_lossy(&output.stdout).contains("Ready to accept connections") {
            Ok(())
        } else {
            Err(std::io::Error::other("redis not ready"))
        }
    })
    .expect("redis readiness");
    let output = std::process::Command::new("docker")
        .args(["port", &container.0, "6379/tcp"])
        .output()
        .expect("redis port");
    assert!(output.status.success(), "redis port lookup failed");
    let address = String::from_utf8(output.stdout).expect("redis port UTF-8");
    let port = address.trim().rsplit(':').next().expect("redis port");
    run(format!("redis://127.0.0.1:{port}/0"), &container.0)
}

/// Host TCP ports of the six nodes (3 masters + 3 replicas) started by the
/// `grokzen/redis-cluster` all-in-one image.
pub const REDIS_CLUSTER_PORTS: [u16; 6] = [7000, 7001, 7002, 7003, 7004, 7005];

/// Spin up a six-node Redis Cluster via the `grokzen/redis-cluster`
/// all-in-one image and pass the per-node connection URLs
/// (`redis://127.0.0.1:<port>/0`, ordered by `REDIS_CLUSTER_PORTS`) to `run`.
///
/// # Port mapping
///
/// A Redis Cluster client does not stay pinned to the node it dialed: it
/// reads `CLUSTER SLOTS`/`CLUSTER SHARDS` and routes every command to
/// whatever address each master advertises there. Under testcontainers'
/// usual random host-port mapping, that reply would carry the container's
/// *internal* ports, which are unreachable from the host the moment the
/// client starts following slot ranges instead of the seed address the test
/// dialed in on.
///
/// This is worked around the way the image's own docs recommend: `IP=0.0.0.0`
/// makes every node advertise `0.0.0.0` (which the OS resolves back to
/// loopback on connect), and each container port is mapped 1:1 onto the same
/// host port via `with_mapped_port` so a client dialing `0.0.0.0:<port>`
/// lands on the right node through the identical published mapping. This
/// means ports 7000-7005 must be free on the host — acceptable on a CI
/// runner, but it can collide with a Redis Cluster already running locally.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup is a hard test-infrastructure failure that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_redis_cluster_urls<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(Vec<String>) -> Result<T, E>,
{
    // Docker Hub directly, not the mirror: mirror.gcr.io does not carry this
    // repository (every tag 404s there, while Docker Hub serves them). It is
    // therefore the one anonymous Hub pull left, and the least maintained
    // image in the suite — a personal account, last pushed in 2024. Replacing
    // it is its own change: the all-in-one layout, the fixed 7000-7005 ports
    // and the IP=0.0.0.0 contract below are specific to this image.
    let mut image = GenericImage::new("grokzen/redis-cluster", "7.0.10")
        .with_wait_for(WaitFor::seconds(5))
        .with_env_var("IP", "0.0.0.0");

    for port in REDIS_CLUSTER_PORTS {
        image = image.with_mapped_port(port, ContainerPort::Tcp(port));
    }

    let _container = image
        .start()
        .expect("failed to start redis cluster container");

    let urls: Vec<String> = REDIS_CLUSTER_PORTS
        .iter()
        .map(|port| format!("redis://127.0.0.1:{port}/0"))
        .collect();

    run(urls)
}

/// Connection parameters for a ClickHouse test container.
pub struct ClickHouseConfig {
    pub endpoint: String,
    pub user: String,
    pub password: String,
    pub database: String,
}

/// Spin up a ClickHouse 25.8 LTS container and wait for its HTTP API.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_clickhouse<T, E, F>(run: F) -> Result<T, E>
where
    E: From<datatab_core::DbError>,
    F: FnOnce(ClickHouseConfig) -> Result<T, E>,
{
    let user = "datatab";
    let password = "datatab";
    let database = "datatab_test";
    let image = hub_image("clickhouse/clickhouse-server", "25.8.30.16")
        .with_exposed_port(ContainerPort::Tcp(8123))
        .with_wait_for(WaitFor::seconds(1))
        .with_env_var("CLICKHOUSE_USER", user)
        .with_env_var("CLICKHOUSE_PASSWORD", password)
        .with_env_var("CLICKHOUSE_DB", database)
        .with_env_var("CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT", "1");

    let container = image.start().expect("failed to start clickhouse container");
    let port = container
        .get_host_port_ipv4(8123)
        .expect("failed to get clickhouse HTTP host port");
    let endpoint = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| datatab_core::DbError::connection_failed(error.to_string()))
        .map_err(E::from)?;

    retry_db_operation(Duration::from_secs(60), || {
        client
            .get(format!("{endpoint}/ping"))
            .basic_auth(user, Some(password))
            .send()
            .map_err(|error| datatab_core::DbError::connection_failed(error.to_string()))
            .map_err(E::from)
            .and_then(|response| {
                if response.status().is_success() {
                    Ok(())
                } else {
                    Err(E::from(datatab_core::DbError::connection_failed(format!(
                        "ClickHouse ping returned {}",
                        response.status()
                    ))))
                }
            })
    })?;

    run(ClickHouseConfig {
        endpoint,
        user: user.to_string(),
        password: password.to_string(),
        database: database.to_string(),
    })
}

/// Spin up a libSQL server (`sqld`, the engine behind Turso) and wait for its
/// health endpoint. The container serves the Hrana HTTP protocol without
/// authentication.
///
/// Set `TURSO_TEST_URL` (and optionally `TURSO_TEST_TOKEN`) to target an
/// existing server instead of starting a container; the closure then receives
/// that URL and no container is created.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_libsql_server<T, E, F>(run: F) -> Result<T, E>
where
    E: From<datatab_core::DbError>,
    F: FnOnce(String) -> Result<T, E>,
{
    if let Ok(url) = std::env::var("TURSO_TEST_URL") {
        return run(url);
    }

    let image = GenericImage::new("ghcr.io/tursodatabase/libsql-server", "v0.24.33")
        .with_exposed_port(ContainerPort::Tcp(8080))
        .with_wait_for(WaitFor::seconds(1));

    let container = image
        .start()
        .expect("failed to start libsql-server container");
    let port = container
        .get_host_port_ipv4(8080)
        .expect("failed to get libsql-server host port");
    let endpoint = format!("http://127.0.0.1:{port}");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| datatab_core::DbError::connection_failed(error.to_string()))
        .map_err(E::from)?;

    retry_db_operation(Duration::from_secs(60), || {
        client
            .get(format!("{endpoint}/health"))
            .send()
            .map_err(|error| datatab_core::DbError::connection_failed(error.to_string()))
            .map_err(E::from)
            .and_then(|response| {
                if response.status().is_success() {
                    Ok(())
                } else {
                    Err(E::from(datatab_core::DbError::connection_failed(format!(
                        "libsql-server health returned {}",
                        response.status()
                    ))))
                }
            })
    })?;

    run(endpoint)
}

/// Password used when launching the SQL Server test container.
///
/// SQL Server requires a "strong" SA password: at least 8 characters with
/// uppercase, lowercase, digit, and special-character classes represented.
/// The same constant is reused inside test URIs.
pub const MSSQL_TEST_PASSWORD: &str = "Strong!Passw0rd";

#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_mssql_url<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(String) -> Result<T, E>,
{
    // The official `mcr.microsoft.com/mssql/server` image takes the EULA via
    // `ACCEPT_EULA=Y` and the SA password via `MSSQL_SA_PASSWORD`. The image
    // is amd64-only; on arm64 hosts, run via emulation or substitute the
    // Azure SQL Edge image manually.
    let image = GenericImage::new("mcr.microsoft.com/mssql/server", "2022-latest")
        .with_exposed_port(ContainerPort::Tcp(1433))
        .with_wait_for(WaitFor::message_on_stdout(
            "SQL Server is now ready for client connections",
        ))
        .with_env_var("ACCEPT_EULA", "Y")
        .with_env_var("MSSQL_SA_PASSWORD", MSSQL_TEST_PASSWORD)
        .with_env_var("MSSQL_PID", "Developer");

    let container = image.start().expect("failed to start mssql container");
    let port = container
        .get_host_port_ipv4(1433)
        .expect("failed to get mssql host port");
    // Connect to `master` by default; tests that need a clean database
    // create `datatab_test` themselves and `USE` it.
    let url = format!(
        "sqlserver://sa:{password}@127.0.0.1:{port}/master",
        password = MSSQL_TEST_PASSWORD
    );

    run(url)
}

/// Container parameters for an InfluxDB v2 instance.
pub struct InfluxV2Config {
    pub endpoint: String,
    pub token: String,
    pub org: String,
    pub bucket: String,
}

/// Container parameters for an InfluxDB v1 instance.
pub struct InfluxV1Config {
    pub endpoint: String,
}

/// Spin up an InfluxDB 2.7 container and pass its endpoint + credentials to `run`.
///
/// Waits until the `/health` endpoint returns a 2xx response before calling `run`.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_influxdb_v2<T, E, F>(run: F) -> Result<T, E>
where
    E: From<datatab_core::DbError>,
    F: FnOnce(InfluxV2Config) -> Result<T, E>,
{
    let token = "datatab-test-token";
    let org = "datatab-test-org";
    let bucket = "datatab-test-bucket";

    // InfluxDB v2 logs to stdout; the "Listening" message signals HTTP readiness.
    let image = hub_image("influxdb", "2.7")
        .with_exposed_port(ContainerPort::Tcp(8086))
        .with_wait_for(WaitFor::message_on_stdout("Listening"))
        .with_env_var("DOCKER_INFLUXDB_INIT_MODE", "setup")
        .with_env_var("DOCKER_INFLUXDB_INIT_USERNAME", "admin")
        .with_env_var("DOCKER_INFLUXDB_INIT_PASSWORD", "adminpassword")
        .with_env_var("DOCKER_INFLUXDB_INIT_ORG", org)
        .with_env_var("DOCKER_INFLUXDB_INIT_BUCKET", bucket)
        .with_env_var("DOCKER_INFLUXDB_INIT_ADMIN_TOKEN", token);

    let container = image
        .start()
        .expect("failed to start influxdb v2 container");
    let port = container
        .get_host_port_ipv4(8086)
        .expect("failed to get influxdb v2 host port");
    let endpoint = format!("http://127.0.0.1:{port}");

    // Wait until the InfluxDB HTTP API is ready.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| datatab_core::DbError::connection_failed(e.to_string()))
        .map_err(E::from)?;

    retry_db_operation(Duration::from_secs(30), || {
        let url = format!("{endpoint}/health");
        client
            .get(&url)
            .send()
            .map_err(|e| datatab_core::DbError::connection_failed(e.to_string()))
            .map_err(E::from)
            .and_then(|resp| {
                if resp.status().is_success() {
                    Ok(())
                } else {
                    Err(E::from(datatab_core::DbError::connection_failed(format!(
                        "health check returned {}",
                        resp.status()
                    ))))
                }
            })
    })?;

    run(InfluxV2Config {
        endpoint,
        token: token.to_string(),
        org: org.to_string(),
        bucket: bucket.to_string(),
    })
}

/// Spin up an InfluxDB 1.8 container and pass its endpoint to `run`.
#[expect(
    clippy::expect_used,
    clippy::unwrap_in_result,
    reason = "container startup and host-port lookup are hard test-infrastructure failures that must abort the run, distinct from the callback's `Err` contract"
)]
pub fn with_influxdb_v1<T, E, F>(run: F) -> Result<T, E>
where
    E: From<datatab_core::DbError>,
    F: FnOnce(InfluxV1Config) -> Result<T, E>,
{
    // InfluxDB v1 logs to stderr; the "Listening on HTTP" message signals readiness.
    let image = hub_image("influxdb", "1.8")
        .with_exposed_port(ContainerPort::Tcp(8086))
        .with_wait_for(WaitFor::message_on_stderr("Listening on HTTP"));

    let container = image
        .start()
        .expect("failed to start influxdb v1 container");
    let port = container
        .get_host_port_ipv4(8086)
        .expect("failed to get influxdb v1 host port");
    let endpoint = format!("http://127.0.0.1:{port}");

    // Wait until HTTP is ready.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| datatab_core::DbError::connection_failed(e.to_string()))
        .map_err(E::from)?;

    retry_db_operation(Duration::from_secs(30), || {
        client
            .get(format!("{endpoint}/ping"))
            .send()
            .map_err(|e| datatab_core::DbError::connection_failed(e.to_string()))
            .map_err(E::from)
            .and_then(|resp| {
                if resp.status().as_u16() < 300 {
                    Ok(())
                } else {
                    Err(E::from(datatab_core::DbError::connection_failed(format!(
                        "ping returned {}",
                        resp.status()
                    ))))
                }
            })
    })?;

    run(InfluxV1Config { endpoint })
}

/// Where a test can reach a Cassandra or ScyllaDB container: loopback and the
/// CQL port the node advertises.
pub struct CassandraEndpoint {
    pub host: String,
    pub port: u16,
}

/// The CQL port every helper here maps onto the host 1:1.
///
/// A random host port does not work: after the control connection the driver
/// opens its request pool against the node's own `native_transport_port` as
/// advertised in cluster metadata, so the host port has to be the one the node
/// names. Port 9042 must therefore be free on the host, which the one-container
/// rule keeps true.
const CQL_PORT: u16 = 9042;

/// The role a container with `PasswordAuthenticator` creates for itself, and
/// the password that ships with it. It is the image's own default, not a secret
/// of ours: the container lives for one test and is removed afterwards.
pub const CQL_TEST_USER: &str = "cassandra";
pub const CQL_TEST_PASSWORD: &str = "cassandra";

/// One Cassandra or ScyllaDB container at a time (ruling 2 on #64).
///
/// Both images publish the same 1:1 port, and one server alone can take a
/// gigabyte and a half, so a second container would neither start nor fit. The
/// lock is held for the container's whole life, which makes parallel test
/// threads wait instead of colliding.
///
/// The lock is process-local and bounds nothing else: two test binaries, or two
/// `cargo test` runs on the same machine, still collide on host port 9042. These
/// suites therefore run one binary at a time, with `--test-threads=1`, so a
/// container is never started beside another.
static CQL_CONTAINER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The line each server logs once it accepts CQL clients, with or without TLS.
const CQL_READY: &str = "Starting listening for CQL clients";

/// How long a container is given to answer CQL. A first start initialises its
/// system keyspaces, which on one core takes most of a minute; the budget is
/// five minutes because ScyllaDB on one core, with TLS configured, starts slower
/// than the Cassandra image this helper first served (240 s).
const CQL_START_LIMIT: Duration = Duration::from_secs(300);

/// A running CQL container, removed when it drops.
struct CqlContainer {
    name: String,
    /// Held for the container's life: the next one waits for it.
    _exclusive: std::sync::MutexGuard<'static, ()>,
}

#[expect(
    clippy::expect_used,
    reason = "container startup is a hard test-infrastructure failure that must abort the run, distinct from the callback's `Err` contract"
)]
impl CqlContainer {
    /// Creates and starts one container. `args` follow `--name` and end with
    /// the image and its command.
    fn start(server: &str, args: &[String]) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_NAME: AtomicU64 = AtomicU64::new(0);
        let exclusive = CQL_CONTAINER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let name = format!(
            "datatab-main-{server}-{}-{timestamp}-{}",
            std::process::id(),
            NEXT_NAME.fetch_add(1, Ordering::Relaxed)
        );
        let output = std::process::Command::new("docker")
            .args(["create", "--pull=never", "--name", &name])
            .args(args)
            .output()
            .expect("create cql container");
        assert!(
            output.status.success(),
            "create {server} container: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let container = Self {
            name,
            _exclusive: exclusive,
        };
        let output = std::process::Command::new("docker")
            .args(["start", &container.name])
            .output()
            .expect("start cql container");
        assert!(
            output.status.success(),
            "start {server} container: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        retry_db_operation::<(), std::io::Error, _>(CQL_START_LIMIT, || {
            let output = std::process::Command::new("docker")
                .args(["logs", &container.name])
                .output()?;
            // Cassandra logs to stdout and ScyllaDB to stderr, so both streams
            // are read.
            let logs = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if logs.contains(CQL_READY) {
                Ok(())
            } else {
                Err(std::io::Error::other(format!("{server} not ready")))
            }
        })
        .expect("cql readiness");
        container
    }
}

impl Drop for CqlContainer {
    fn drop(&mut self) {
        let result = std::process::Command::new("docker")
            .args(["rm", "-f", "-v", &self.name])
            .output();
        if !matches!(&result, Ok(output) if output.status.success()) {
            eprintln!("failed to remove cql container {}: {result:?}", self.name);
            assert!(std::thread::panicking(), "cql container cleanup failed");
        }
    }
}

/// Runs one callback against a fresh container of `args`.
fn run_cql_container<T, E, F>(server: &str, args: &[String], run: F) -> Result<T, E>
where
    F: FnOnce(CassandraEndpoint) -> Result<T, E>,
{
    let container = CqlContainer::start(server, args);
    let outcome = run(CassandraEndpoint {
        host: "127.0.0.1".to_string(),
        port: CQL_PORT,
    });
    // Removed before the outcome is returned, so a passing run leaves nothing
    // behind either.
    drop(container);
    outcome
}

/// The `docker create` arguments shared by every Cassandra container here.
///
/// A single node with its data directory on tmpfs: the suite only needs the
/// answers it serves from memory, and the memory cap keeps one test run from
/// competing with the rest of the machine. The heap is set explicitly because
/// the image sizes it without seeing the container's memory cap, and the
/// default heap is killed by the kernel inside it (design §7).
fn cassandra_args(entrypoint_script: &str) -> Vec<String> {
    vec![
        "--memory=1536m".to_string(),
        "--tmpfs".to_string(),
        "/var/lib/cassandra".to_string(),
        "-e".to_string(),
        "MAX_HEAP_SIZE=768M".to_string(),
        "-e".to_string(),
        "HEAP_NEWSIZE=128M".to_string(),
        "-p".to_string(),
        format!("127.0.0.1:{CQL_PORT}:{CQL_PORT}"),
        mirror_image_name("cassandra", "5.0"),
        "bash".to_string(),
        "-c".to_string(),
        entrypoint_script.to_string(),
    ]
}

/// The `docker create` arguments shared by every ScyllaDB container here.
fn scylla_args(entrypoint_script: &str) -> Vec<String> {
    vec![
        "--memory=1024m".to_string(),
        "--tmpfs".to_string(),
        "/var/lib/scylla".to_string(),
        "-p".to_string(),
        format!("127.0.0.1:{CQL_PORT}:{CQL_PORT}"),
        "--entrypoint".to_string(),
        "/bin/sh".to_string(),
        mirror_image_name("scylladb/scylla", "6.2"),
        "-c".to_string(),
        entrypoint_script.to_string(),
    ]
}

/// The stock Cassandra image leaves materialized views off, so the suite that
/// creates one turns the feature on and then hands over to the image's own
/// start command. Nothing else in the image's configuration is changed.
const CASSANDRA_START: &str = "sed -i 's/^materialized_views_enabled: false/materialized_views_enabled: true/' /etc/cassandra/cassandra.yaml && exec docker-entrypoint.sh cassandra -f";

/// The same start, plus the role database: with `PasswordAuthenticator` the
/// image creates its `cassandra` superuser on the first start, so a test proves
/// the password path instead of connecting anonymously.
const CASSANDRA_PASSWORD_START: &str = "sed -i -e 's/^materialized_views_enabled: false/materialized_views_enabled: true/' -e 's/^authenticator: AllowAllAuthenticator/authenticator: PasswordAuthenticator/' /etc/cassandra/cassandra.yaml && exec docker-entrypoint.sh cassandra -f";

/// ScyllaDB's own start flags, with its role database turned on.
///
/// The authenticator is a server option, not a yaml edit: this image ships the
/// `authenticator` line commented out, and the entry point hands an unknown
/// option to the server, which then creates its `cassandra` superuser.
///
/// The node advertises `127.0.0.1` as the address clients use, which is where the
/// helper publishes the port. Without it ScyllaDB names its bridge address, and a
/// driver's request pool would have to reach that address directly; with it every
/// connection goes through the published port, and a certificate for `127.0.0.1`
/// is the one `verify-full` has to check.
///
/// The heap is 500M, not the 750M the first design assumed: seastar aborts with
/// "needed 734003200 available 500000000" before the server binds a port, because
/// inside this container it computes 500 MB as available, and it wants its request
/// to sit 50 MiB under that. 500M is the largest heap that starts here, and it is
/// ample for the suite's few dozen rows.
const SCYLLA_PASSWORD_START: &str = "exec /docker-entrypoint.py --smp 1 --memory 500M --overprovisioned 1 --developer-mode 1 --authenticator PasswordAuthenticator --broadcast-rpc-address 127.0.0.1";

/// ScyllaDB with its native transport encrypted, started with the certificates
/// mounted read-only at `/tls`.
///
/// The certificate and key are copied where the server can read them, and the
/// encryption block is appended to `scylla.yaml` before the image's entry point
/// reads it. With encryption on, the native transport port accepts TLS only,
/// which is what lets the test prove that a mode really changes the connection.
const SCYLLA_TLS_START: &str = "mkdir -p /etc/scylla/tls && install -m 644 /tls/server.crt /etc/scylla/tls/server.crt && install -m 644 /tls/server.key /etc/scylla/tls/server.key && printf '%s\\n' 'client_encryption_options:' '  enabled: true' '  certificate: /etc/scylla/tls/server.crt' '  keyfile: /etc/scylla/tls/server.key' '  require_client_auth: false' >> /etc/scylla/scylla.yaml && exec /docker-entrypoint.py --smp 1 --memory 500M --overprovisioned 1 --developer-mode 1 --authenticator PasswordAuthenticator --broadcast-rpc-address 127.0.0.1";

/// Spin up Cassandra and wait until it accepts CQL connections, anonymously.
pub fn with_cassandra<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(CassandraEndpoint) -> Result<T, E>,
{
    run_cql_container("cassandra", &cassandra_args(CASSANDRA_START), run)
}

/// Spin up Cassandra whose CQL port requires the image's own `cassandra` role.
pub fn with_authenticated_cassandra<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(CassandraEndpoint) -> Result<T, E>,
{
    run_cql_container("cassandra", &cassandra_args(CASSANDRA_PASSWORD_START), run)
}

/// Spin up ScyllaDB whose CQL port requires the image's own `cassandra` role.
pub fn with_scylla<T, E, F>(run: F) -> Result<T, E>
where
    F: FnOnce(CassandraEndpoint) -> Result<T, E>,
{
    run_cql_container("scylla", &scylla_args(SCYLLA_PASSWORD_START), run)
}

/// Spin up ScyllaDB that accepts TLS only on its CQL port.
///
/// `certificates` holds `server.crt` and `server.key`, which the container reads
/// from its `/tls` mount. ScyllaDB carries the TLS modes because the driver
/// checks each node's IP against the certificate, which makes `verify-ca` the
/// practical verifying mode (ruling 7).
pub fn with_scylla_tls<T, E, F>(certificates: &std::path::Path, run: F) -> Result<T, E>
where
    F: FnOnce(CassandraEndpoint) -> Result<T, E>,
{
    let mut args = scylla_args(SCYLLA_TLS_START);
    // The mount goes before the image name, the command after it.
    let image_at = args
        .iter()
        .position(|arg| arg.contains("scylladb/scylla"))
        .unwrap_or(args.len());
    args.splice(
        image_at..image_at,
        [
            "-v".to_string(),
            format!("{}:/tls:ro", certificates.display()),
        ],
    );
    run_cql_container("scylla", &args, run)
}

pub fn retry_db_operation<T, E, F>(timeout: Duration, mut operation: F) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
{
    let deadline = Instant::now() + timeout;

    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::{hub_image, mirror_image_name};
    use testcontainers::Image;

    #[test]
    fn the_image_name_carries_no_tag_because_testcontainers_adds_it() {
        let image = hub_image("postgres", "16");
        assert_eq!(image.name(), "mirror.gcr.io/library/postgres");
        assert_eq!(image.tag(), "16");

        let image = hub_image("pgvector/pgvector", "pg16");
        assert_eq!(image.name(), "mirror.gcr.io/pgvector/pgvector");
        assert_eq!(image.tag(), "pg16");
    }

    #[test]
    fn a_shelled_out_reference_carries_its_tag_once() {
        assert_eq!(
            mirror_image_name("cassandra", "5.0"),
            "mirror.gcr.io/library/cassandra:5.0"
        );
        assert_eq!(
            mirror_image_name("pgvector/pgvector", "pg16"),
            "mirror.gcr.io/pgvector/pgvector:pg16"
        );
    }
}
