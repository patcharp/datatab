# Driver live integration tests

All 10 first-party feature drivers (clickhouse, influxdb, mongodb, mssql, mysql,
postgres, redis, redshift, sqlite, turso) ship a live integration suite. Nine of
them run against a local backend:
`testcontainers` starts an isolated container per test with a dynamic host port
and tears it down automatically; SQLite uses a temporary local file. No real
cloud credentials are needed anywhere in the local suite. Redshift is the
exception: no Docker-based emulator exists, so its suite is credential-gated
and excluded from the local commands below.

The `datatab_driver_host` and `datatab_driver_ipc` crates are RPC infrastructure
for external drivers, not database suites — they ship no `tests/` directory and
are covered by unit tests.

## Prerequisites

- A working Docker daemon: `testcontainers` talks to Docker directly, so every
  ignored test below fails without one (Turso can alternatively target
  `TURSO_TEST_URL`).
- `cargo nextest` is preferred (the Nix dev shell provides it). Plain
  `cargo test` works too: select one `--test` binary per invocation and append
  `-- --ignored`; keep `--test-threads=1` after the `--` for suites marked
  single-threaded.

## Run the full local suite

Every ignored test across all container- and file-backed crates, plus the
non-ignored SQLite suite, with no cloud credentials involved:

```bash
cargo nextest run \
  -p datatab_driver_mysql -p datatab_driver_mongodb -p datatab_driver_redis \
  -p datatab_driver_sqlite \
  -p datatab_driver_mssql -p datatab_driver_influxdb -p datatab_driver_clickhouse \
  -p datatab_driver_turso \
  --run-ignored all --test-threads=1

# PostgreSQL is run separately because its crate also contains the
# credential-gated SSH suite; select only its container-backed binaries:
cargo nextest run -p datatab_driver_postgres --run-ignored all --test-threads=1 \
  --test live_integration --test schema_columns_live --test ddl_integration \
  --test relational_filter_live --test instance_catalog
```

`--test-threads=1` is *required* only by the Redis cluster suite (the
all-in-one cluster image binds fixed host ports 7000–7005, which race without
it). The remaining suites may use the flag locally to bound memory.

The command deliberately excludes Redshift (real cluster) and the PostgreSQL
SSH suite (needs SSH and PostgreSQL services you provide) — see
[Credential-gated suites](#credential-gated-suites).

## Per-driver suites

All suites run locally. This repository has no CI.

| Driver | Test binaries (`--test`) | Local backend | Notes |
|---|---|---|---|
| PostgreSQL | `live_integration`, `schema_columns_live`, `ddl_integration` *(local only)*, `relational_filter_live` *(local only)*, `instance_catalog` *(local only)* | `postgres:16`, `pgvector/pgvector:0.8.0-pg16` | The pgvector image backs one pgvector test in `live_integration`. SSH suite is separate (see below). |
| MySQL | `live_integration`, `ddl_integration` *(local only)*, `instance_catalog` *(local only)* | `mysql:8.4` | |
| MongoDB | `live_integration`, `instance_catalog` *(local only)* | `mongo:7` | |
| Redis | `live_integration`, `live_cluster`, `live_rdb`, `instance_catalog` *(local only)* | `redis:7`, `grokzen/redis-cluster:7.0.10` | `live_cluster` needs `--test-threads=1` (fixed ports 7000–7005). `live_rdb` exercises the RDB analyzer against the container's `dump.rdb` after a forced `SAVE`. |
| SQLite | `live_integration`, `ddl_integration` *(local only)* | temporary local file | No Docker. Not `#[ignore]`d — run without `-- --ignored`. |
| SQL Server | `live_integration`, `ddl_integration`, `instance_catalog` | `mcr.microsoft.com/mssql/server:2022-latest` | amd64 image; single-threaded (one ~2 GB container per test). See specifics below. |
| InfluxDB | `live_integration` | `influxdb:2.7` and `influxdb:1.8` | Covers both the v2 API and v1 (InfluxQL) code paths; single-threaded. |
| ClickHouse | `live_integration` | `clickhouse/clickhouse-server:25.8.30.16` | Single-threaded. See specifics below. |
| Turso | `live_integration` | `ghcr.io/tursodatabase/libsql-server` (pinned tag) or `TURSO_TEST_URL` | Single-threaded. |
| Redshift | `live_integration` | real Amazon Redshift cluster | Credential-gated; excluded from the local suite. See below. |

Representative per-suite commands:

```bash
cargo test --locked -p datatab_driver_postgres --test live_integration -- --ignored
cargo test --locked -p datatab_driver_redis    --test live_cluster  -- --ignored --test-threads=1
cargo test --locked -p datatab_driver_sqlite   --test live_integration
```

## MCP PostgreSQL disconnect regression

The ignored `datatab_mcp_server` unit test uses a disposable trust-authenticated
PostgreSQL instance. For example, with a locally available `postgres:17-alpine`
image, start a container with a loopback-only port, temporary data and a memory
limit, then pass the assigned port to the test:

```bash
docker run -d --name datatab-test-postgres --memory=512m \
  --tmpfs /var/lib/postgresql/data -p 127.0.0.1::5432 \
  -e POSTGRES_HOST_AUTH_METHOD=trust postgres:17-alpine
# Set PORT to the host port from: docker port datatab-test-postgres 5432
DATATAB_TEST_POSTGRES_PORT="$PORT" cargo test -p datatab_mcp_server --locked \
  --features postgres disconnect_drops_postgres_connection_outside_tokio -- --ignored
docker rm -f datatab-test-postgres
```

Wait for PostgreSQL to accept connections before running the test. The shared
container helper currently uses a different PostgreSQL image and settings.

## Environment variables

The container-backed suites need no environment variables. Four suites read
settings from the environment:

| Variables | Suite | Meaning |
|---|---|---|
| `DATATAB_TEST_POSTGRES_PORT` | MCP disconnect regression | Port of the disposable trust-authenticated PostgreSQL instance. |
| `TURSO_TEST_URL`, `TURSO_TEST_TOKEN` (optional) | Turso | Run against an external libSQL server instead of the pinned container. |
| `DATATAB_TEST_SSH_*`, `DATATAB_TEST_DB_*` | PostgreSQL SSH | Host and credentials for SSH + PostgreSQL services you provide (below). |
| `DATATAB_TEST_REDSHIFT_*` | Redshift | Connection details and fixture names for a real Redshift cluster (below). |

## Credential-gated suites

### PostgreSQL SSH suite (optional, local only)

`crates/datatab_driver_postgres/tests/ssh_live_integration.rs` exercises a real
SSH tunnel end to end. It is not run in CI: it needs an SSH server and a
PostgreSQL server that you provide. Required variables: `DATATAB_TEST_SSH_PORT`,
`DATATAB_TEST_SSH_USER`, `DATATAB_TEST_SSH_KEY_PATH`, `DATATAB_TEST_DB_HOST`,
`DATATAB_TEST_DB_USER`, `DATATAB_TEST_DB_NAME`, `DATATAB_TEST_DB_PASSWORD`.
Optional: `DATATAB_TEST_SSH_HOST` (default `127.0.0.1`),
`DATATAB_TEST_SSH_PASSPHRASE`, `DATATAB_TEST_DB_PORT` (default `5432`).

```bash
cargo test -p datatab_driver_postgres --test ssh_live_integration -- --ignored
```

### Redshift (credential-gated, no emulator)

LocalStack emulates Redshift's management/Data API only, not the wire
protocol, so the Redshift suite connects to a real cluster instead of a
container. Required: `DATATAB_TEST_REDSHIFT_HOST`, `DATATAB_TEST_REDSHIFT_PASSWORD`.
Defaults if unset: `DATATAB_TEST_REDSHIFT_PORT` (5439), `DATATAB_TEST_REDSHIFT_USER`
(`awsuser`), `DATATAB_TEST_REDSHIFT_DATABASE` (`dev`). The metadata-introspection
tests additionally need fixture variables: `DATATAB_TEST_REDSHIFT_SCHEMA`
(default `public`), `DATATAB_TEST_REDSHIFT_EMPTY_SCHEMA`, `DATATAB_TEST_REDSHIFT_TABLE`,
`DATATAB_TEST_REDSHIFT_PK_TABLE`, `DATATAB_TEST_REDSHIFT_SUPER_TABLE` /
`DATATAB_TEST_REDSHIFT_SUPER_COLUMN`, plus optional mTLS files
(`DATATAB_TEST_REDSHIFT_SSL_CLIENT_CERT`, `_SSL_CLIENT_KEY`, `_SSL_ROOT_CERT`).

CI runs the Redshift step only when the `DATATAB_TEST_REDSHIFT_HOST` secret is
configured (never true on fork PRs) and reports a clean skip otherwise. Never
add Redshift to the local full-suite command: the tests fail outright without
the variables, so selection — not the environment — is what keeps the local
suite safe.

## Coverage limits

- **SQL Server**: `ssl_mode = "required"` and named-instance routing are not
  covered by the live suite (self-signed test cert, no SQL Browser sidecar);
  both paths are covered by unit tests in
  `crates/datatab_driver_mssql/src/driver.rs::tests`. See the SQL Server
  specifics below.
- **Redshift**: the selected introspection tests that need fixtures (`DATATAB_TEST_REDSHIFT_EMPTY_SCHEMA`, `_TABLE`, `_PK_TABLE`, `_SUPER_TABLE`/`_SUPER_COLUMN`) **fail** — via `.expect(...)` panics, not skips — when those variables are absent; set them (or filter those tests out) before running the suite.
- Every suite exercises driver **source** under `cargo test`; none exercise
  built release artifacts (see Release gating below).

## Release gating

The private repository has no CI. The public release workflow only builds and
packages artifacts; it runs no tests. Run the relevant suites locally before
tagging an rc or stable.

## ClickHouse specifics

The ClickHouse suite pulls `clickhouse/clickhouse-server:25.8.30.16` (25.8
LTS). Every ignored test launches its own container through `testcontainers`,
maps HTTP port 8123 to a dynamic host port, and initializes database
`datatab_test` with user/password `datatab`/`datatab`.

## SQL Server specifics

The MSSQL suite pulls `mcr.microsoft.com/mssql/server:2022-latest` (amd64
only — emulate or substitute Azure SQL Edge on arm64). Each test launches a
fresh container, creates a `datatab_test` database, and runs against it.

Coverage includes:

- URI-mode connect (`use_uri = true`) for the bulk of the suite.
- Form-mode connect (`use_uri = false`, host/port/user + secret-fed password)
  in `mssql_form_mode_connect_query_and_select_db`.
- SSL modes `off` and `on` (with `trust_server_certificate = true`, since the
  stock image uses a self-signed cert) in `mssql_ssl_mode_off_connects` and
  `mssql_ssl_mode_on_trusts_self_signed`.
- CRUD via `OUTPUT INSERTED.*` / `OUTPUT DELETED.*`, schema introspection,
  `OFFSET ... FETCH NEXT` paging, cancellation via side-channel `KILL`.

`ssl_mode = "required"` and named-instance routing are not covered by the live
suite: the test image ships a self-signed cert (so strict-validation paths
would fail by design) and only the default `MSSQLSERVER` instance is exposed
(no SQL Browser sidecar). Both paths are covered by unit tests in
`crates/datatab_driver_mssql/src/driver.rs::tests`.
