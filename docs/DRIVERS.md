# DataTab Drivers

This document is a comparative overview of the database drivers shipped with
DataTab. For per-driver details, follow the link to each driver crate's
`README.md`. For the internal driver architecture (traits, registration, the
`DbDriver`/`Connection` seam), see the **Driver System** section of
[`ARCHITECTURE.md`](../ARCHITECTURE.md). Contributors implementing a driver
should start with the [Driver Authoring Guide](DRIVER_AUTHORING.md).

## How drivers are abstracted

Every driver exposes a `DriverMetadata` value (defined in
`crates/datatab_core/src/driver/capabilities.rs`). The UI is driver-agnostic and
adapts purely from this metadata. The relevant fields are:

- **`DatabaseCategory`** — selects the view model and terminology. Values:
  `Relational`, `Document`, `KeyValue`, `TimeSeries`, `ObjectStorage`. No
  built-in driver uses `ObjectStorage`; an RPC driver can.
- **`QueryLanguage`** — drives editor mode, placeholder text, and query parsing.
  Values include `Sql`, `MongoQuery`, `RedisCommands`, `Cypher`, `InfluxQuery`,
  `Flux`, `Cql`, the script languages `Lua` / `Python` / `Bash`, and
  `Custom(String)`.
- **Reserved slots** — `DbKind`, `Icon`, `QueryLanguage` and `DatabaseCategory`
  also hold hidden `Reserved*` variants. They only keep the wire positions of
  retired variants, and a driver must not declare them. See
  [Reserved enum slots](DRIVER_RPC_PROTOCOL.md#reserved-enum-slots).
- **`DriverCapabilities`** — a `u128` bitflag set declaring supported features
  (transactions, pagination, schemas, key-value operations, etc.). Convenience
  bases `RELATIONAL_BASE`, `DOCUMENT_BASE`, and `KEYVALUE_BASE` group the common
  flags for each category.

The capability flags listed below are exactly the ones each driver's
`DriverMetadata` sets in code; nothing is inferred.

## Comparison

| Driver | Category | Query language | Key capabilities | Notes / limitations |
| --- | --- | --- | --- | --- |
| PostgreSQL | Relational | SQL | Relational base + schemas, SSH tunnel, SSL, auth, foreign keys, check/unique constraints, custom types, `RETURNING`, transactional DDL, routines, multi-statement | Full SQL driver; routine viewer is read-only; transactional DDL except `CREATE INDEX CONCURRENTLY`. |
| Amazon Redshift | Relational | SQL | Multiple databases, schemas, views, SSH tunnel, SSL/client certificates, auth, query cancellation, prepared statements, pagination, sorting, filtering, CSV/JSON export | Read-only over the PostgreSQL wire protocol; single-statement; exposes Redshift storage hints; no writes/DDL, IAM/SSO, or indexes. |
| MySQL | Relational | SQL | Relational base + SSH tunnel, SSL, auth, foreign keys, check/unique constraints, routines, multi-statement | DDL is non-transactional; multi-statement scripts split text-based and run sequentially; routine listing covers FUNCTION/PROCEDURE only. |
| MariaDB | Relational | SQL | Same crate and capabilities as MySQL | Registered as a separate `mariadb` metadata sharing the MySQL implementation. |
| SQLite | Relational | SQL | Views, indexes, foreign keys, check/unique constraints, prepared statements, insert/update/delete, pagination, sorting, filtering, CSV/JSON export, query cancellation, transactional DDL, multi-statement | Embedded file driver: no network, SSH tunnel, or TLS; no multi-schema namespace. |
| SQL Server | Relational | SQL | Relational base + schemas, SSH tunnel, SSL, auth, foreign keys, check/unique constraints, transactional DDL, routines, multi-statement | Built on `tiberius`; named-instance lookup unavailable through SSH tunnel; multi-result-set batches return the last set as primary. |
| MongoDB | Document | MongoQuery | Document base + aggregation, SSH tunnel, indexes | MongoDB shell-style syntax only (no SQL); no query cancellation; parser scoped to supported command patterns. |
| Redis | Key-Value | RedisCommands | Key-value base + multiple databases, TTL, key types, value size, rename, bulk get, stream range/add/delete, auth, SSH tunnel, SSL | Redis command syntax only (no SQL); no query cancellation; SSH tunneling unavailable in URI mode. |
| InfluxDB | Time Series | InfluxQuery | Auth, multiple databases, pagination, CSV/JSON export | v1 and v2 in one crate; InfluxQL on both, Flux on v2 only; read-only (no INSERT/UPDATE/DELETE); no transactions. |
| ClickHouse | Relational | SQL | TLS modes (disable, require, verify-ca, verify-full), private CA and mTLS, multiple databases, views, auth, pagination, sorting, filtering, grouping, joins, CTEs, windows, CSV/JSON export | HTTP(S), including ClickHouse Cloud; read-oriented DataTab integration with no structured mutations, DDL, transactions, SSH tunneling, or query parameters. |
| TursoDB | Relational | SQL | Auth token, views, indexes, foreign keys, check/unique constraints, prepared statements, insert/update/delete, pagination, sorting, filtering, CSV/JSON export, transactions, transactional DDL, multi-statement | Remote Turso / libSQL over HTTP (`libsql://`, or `http://` for a local `sqld`); interactive transactions run on per-document server streams; no query cancellation, SSH tunnel, replicas, or database switching. |
| DuckDB | Relational | SQL | Multiple databases, schemas, views, indexes, foreign keys, check/unique constraints, prepared statements, insert/update/delete with RETURNING, pagination, sorting, filtering, CSV/JSON export, transactions, transactional DDL, query cancellation, row limits, read-only enforcement, multi-statement | Embedded file or in-memory database; attaches a DuckLake catalog on connect; only one process can hold a file for writing; no statement timeouts. |
| Cassandra / ScyllaDB | Relational | CQL | Auth, multiple databases (keyspaces), schema browsing (tables, views, user types, indexes, key roles), TLS with private CA and mutual TLS, row limits, paging through the server's paging state, filtering, prepared statements, lightweight-transaction row edits | One driver for Cassandra and ScyllaDB over CQL, with live suites on throwaway containers of both servers. There is no server-side sorting, no row count, no OFFSET, no statement cancellation, no transactions, no SSH tunnelling or proxy access, and no profile field for the consistency level yet. `verify-full` needs an IP SAN on every node. |

## Per-driver summary

### PostgreSQL

Full SQL driver with schema discovery, stored routines (read-only viewer), SSL,
SSH tunneling, query cancellation via cancel tokens, transactional DDL, and
PostgreSQL-specific code generation. Multi-statement scripts run as a batch via
the simple query protocol. See
[`crates/datatab_driver_postgres/README.md`](../crates/datatab_driver_postgres/README.md).

### Amazon Redshift

Read-only relational SQL driver using the PostgreSQL wire protocol. It supports
schema, table, view, and column introspection; SSH tunneling; TLS and client
certificates; query cancellation; and Redshift distribution/sort-key storage
hints. It does not support writes or DDL, IAM/SSO authentication,
multi-statement queries, or indexes. See
[`crates/datatab_driver_redshift/README.md`](../crates/datatab_driver_redshift/README.md).

### MySQL / MariaDB

Bulk schema column reads use one catalog query per database; schema-wide index reads retain functional index expressions when provided by the server.

One crate implements both MySQL and MariaDB. Supports SQL execution, schema
discovery, query cancellation via `KILL QUERY`, code generation, and routine
discovery for functions and procedures. DDL is not transactional and
multi-statement splitting is text-based. See
[`crates/datatab_driver_mysql/README.md`](../crates/datatab_driver_mysql/README.md).

### SQLite

Bulk schema column reads use one catalog query without a table-count parameter limit.

Embedded, file-based driver with schema discovery, query cancellation via
interrupt handles, transactional DDL, and code generation. No network transport,
SSH tunneling, or TLS, and no multi-schema namespace. See
[`crates/datatab_driver_sqlite/README.md`](../crates/datatab_driver_sqlite/README.md).

### SQL Server

Built on the `tiberius` TDS client. Supports SQL Server / Azure SQL, TLS modes,
named instances (resolved via SQL Browser), SSH tunneling, per-tab database
switching, and multi-result-set batches. Bulk schema column reads use one catalog
query per schema; no table-count batching is needed. See
[`crates/datatab_driver_mssql/README.md`](../crates/datatab_driver_mssql/README.md).

### MongoDB

Document driver with collection browsing, document CRUD, MongoDB shell-style
query parsing, aggregation, and document-focused schema metadata. SQL is not
supported and query cancellation is unavailable. See
[`crates/datatab_driver_mongodb/README.md`](../crates/datatab_driver_mongodb/README.md).

### Redis

Key-value driver covering strings, hashes, lists, sets, sorted sets, and
streams, plus key scanning, TTL operations, rename, bulk get, and multiple
logical databases. SQL is not supported and SSH tunneling is unavailable in URI
mode. See
[`crates/datatab_driver_redis/README.md`](../crates/datatab_driver_redis/README.md).

### InfluxDB

Time-series driver supporting both InfluxDB v1 and v2 in one crate. InfluxQL runs
on both versions; Flux runs on v2 only. The query API is read-only (no
INSERT/UPDATE/DELETE, no transactions), with optional default bucket/database and
per-query bucket routing. See
[`crates/datatab_driver_influxdb/README.md`](../crates/datatab_driver_influxdb/README.md).

### ClickHouse

Schema diff generates CREATE and DROP; a ClickHouse reference keeps its sorting key, and recognized Cloud Shared engines are emitted as their underlying engines: MergeTree, ReplacingMergeTree, SummingMergeTree, AggregatingMergeTree, CollapsingMergeTree, VersionedCollapsingMergeTree, GraphiteMergeTree, and their Replicated forms. Engine arguments are not preserved and may need hand editing. Unknown `Shared*` names and other unsupported engines are refused by name. Primary keys become ORDER BY without uniqueness enforcement, and unsupported constraints are refused by name. Cross-engine diffs copy source type names, which may need hand editing.

TLS modes are `disable (unverified)` (plain HTTP), `require (unverified)`
(HTTPS without server certificate verification), `verify-ca` (CA chain) and
`verify-full` (CA chain and hostname). Verification modes require a CA PEM path
and trust only the supplied roots, including private and self-signed CAs.
Mutual TLS accepts a certificate and matching unencrypted key PEM pair in all
three TLS modes. Encrypted private keys and certificate-store identities are
not supported. Unknown or ambiguous modes, mode/URL conflicts, missing required
CA paths, unused CA paths and incomplete identities are refused before network
access. Paths cannot be secret references. Legacy saved profiles without a mode
keep system-root verification on HTTPS until saved; new and edited profiles
must choose a mode. Files are limited to 1 MiB each.

Relational SQL driver for self-hosted ClickHouse and ClickHouse Cloud over
HTTP(S). It discovers databases, tables, views, columns, and engine metadata,
and supports read-oriented SQL workflows with pagination and visual SELECT
generation. Structured mutations, DDL, transactions, SSH tunneling, and generic
query parameters are not supported in this initial scope. See
[`crates/datatab_driver_clickhouse/README.md`](../crates/datatab_driver_clickhouse/README.md).

### TursoDB

Relational SQL driver for Turso Cloud and self-hosted `sqld` over HTTP, built on
the `turso_serverless` SDK. It speaks the SQLite dialect, discovers tables,
views, columns, indexes, foreign keys, and constraints through `sqlite_master`
and PRAGMAs, and supports typed CRUD, bound parameters, batched scripts, and
interactive transactions on per-document server streams, one control
statement per editor run. Query cancellation, savepoints, SSH tunneling,
embedded replicas, and database switching are not supported. See
[`crates/datatab_driver_turso/README.md`](../crates/datatab_driver_turso/README.md).

### DuckDB

Schema diff generates CREATE and DROP; foreign keys and other constraints the generator cannot include are refused by name.

Bulk schema column reads use one catalog query per schema, including primary-key membership.

Embedded analytical SQL driver built on the bundled DuckDB engine. A profile
opens a database file, or an in-memory database when the path is empty, and can
attach a DuckLake catalog (a local metadata file or a PostgreSQL, SQLite, or
MySQL catalog, with data on local disk or object storage) that becomes the
default database. Every attached catalog is a database in the sidebar, with its
schemas, tables, and views loaded lazily. It supports bound parameters,
multi-statement scripts, row limits, cancellation, typed CRUD with `RETURNING`,
and read-only enforcement through a `READ ONLY` transaction that also refuses
file and network reads through table functions named in the statement; views
and macros created earlier, and scalar functions added by extensions, are not
checked. Statement timeouts are not supported. See
[`crates/datatab_driver_duckdb/README.md`](../crates/datatab_driver_duckdb/README.md).

### Cassandra / ScyllaDB

Relational driver for Apache Cassandra and ScyllaDB over CQL, built on the
`scylla` crate; one driver serves both, as one MySQL driver serves MySQL and
MariaDB. A profile names one or more contact points, a port, an optional local
datacenter and an optional keyspace, authenticates with an optional user and
keyring-stored password, and can encrypt with TLS in the `disable`, `require`,
`verify-ca` or `verify-full` modes, with a private CA file and mutual TLS. It
executes CQL statements with bound scalar values and stops collecting at a
request's row limit. It browses the schema lazily from `system_schema`:
keyspaces (the server's own last), then tables, materialized views and user
types, then each table's columns with the partition and clustering key marked,
its clustering order, static columns, indexes and, for a view, its base table.
CQL statements are classified by the driver's own language service: a `SELECT`
is a read, a system-keyspace catalog read is metadata, `INSERT` and `UPDATE`
are writes, `CREATE` is safe administration, `ALTER`, `DROP INDEX`, `GRANT`
and `REVOKE` are administration, and `DROP` of anything else and `TRUNCATE`
are destructive administration. A `DELETE` is classified against the table's
primary key, read from the `system_schema` rows the schema browse already
fetched: it is a write when its `WHERE` constrains every key column with `=`,
and destructive — confirmed before it runs — when a key column is missing or
compared any other way, or when the table's details have not been read in this
session, which costs one confirmation on a cold cache. A bare table name is
judged in the keyspace the session runs in, and a schema change the session
runs empties what has been read. A
`BEGIN BATCH … APPLY BATCH` block runs as one statement.
A table browse pages through the server's paging state, because CQL has no
`OFFSET`: the statement carries no `LIMIT`, each page continues from the state
the one before it ended at, and a per-connection cache keeps those positions so
next, previous and a refresh reuse them. A row count is refused, since a CQL
count is a full scan. Row edits are prepared lightweight transactions —
`UPDATE … IF EXISTS`, `INSERT … IF NOT EXISTS`, `DELETE … WHERE <full key> IF
EXISTS` — so an edit cannot upsert and a refusal surfaces as an error; a
materialized view and a counter table are read-only. A generator writes the
CQL for generated reads (MCP's `select_data`), the select-by-key and edit
templates, and copy-as-query. The driver declares no `SORTING`, so the grid
offers no header sort for its tables and says why in the header's tooltip.
CQL is not SQL, so the import/export wizard does not offer Cassandra as a
target and schema diff does not offer it as a compare target. Live suites cover
these behaviours on `cassandra:5.0` and `scylladb/scylla:6.2` throwaway
containers, one at a time, with a TLS test on ScyllaDB in the `require`,
`verify-ca` and `verify-full` modes; all of them pass. See
[`crates/datatab_driver_cassandra/README.md`](../crates/datatab_driver_cassandra/README.md).

## External RPC drivers

DataTab can load drivers that run out-of-process and communicate over local IPC,
implemented through `datatab_driver_ipc` and hosted via `datatab_driver_host`.
These drivers register with the synthetic ID format `rpc:<socket_id>` and supply
their own `DriverMetadata` (category, query language, capabilities) over the
wire, so the UI treats them exactly like built-in drivers. For the discovery
handshake, service lifecycle, and protocol details, see
[`docs/DRIVER_RPC_PROTOCOL.md`](DRIVER_RPC_PROTOCOL.md).
