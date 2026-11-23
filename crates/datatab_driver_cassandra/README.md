# datatab_driver_cassandra

Built-in driver for Apache Cassandra and ScyllaDB. One driver serves both: the
two speak the same CQL wire protocol, as one MySQL driver serves MySQL and
MariaDB.

## Features

- Connection by one or more contact points, with a separate Port field
  (default 9042), an optional keyspace and an optional local datacenter. The
  datacenter sets the driver's preferred datacenter so requests go to the
  nodes nearest the client.
- Password authentication through the secret path every profile uses: the
  password goes to the OS keyring and is never written into the profile.
  Clusters that allow anonymous access work with an empty user and password.
- Keyspaces are the database layer, so the driver reports
  `SchemaLoadingStrategy::LazyPerDatabase` and can switch keyspace with `USE`.
- Schema browsing from `system_schema`, read lazily per keyspace: the cluster's
  keyspaces (the server's own listed last), then the tables, materialized views
  and user types of one keyspace, then the columns, key roles and indexes of one
  table. A keyspace's columns and indexes also load in one bulk call, so the
  tree does not run one statement per table.
- The CQL primary key is shown as it is defined: the partition key first, then
  the clustering columns in clustering order, all marked as primary key. The
  roles and any descending clustering order are reported as table storage hints
  ("Partition key", "Clustering key", "Clustering order", "Static column"), so
  the tree and the details pane show them without driver-specific code. A
  materialized view names its base table in a "Base table" hint, and an index
  names the column it covers.
- TLS with the same mode words as the other drivers: `disable`, `require`,
  `verify-ca` and `verify-full`. A private CA file replaces the platform roots
  for the two verifying modes. Mutual TLS is supported: a client certificate
  and its key authenticate DataTab to the cluster, and both paths must be set
  together.
- Statement execution over CQL, with bound values for text, integers,
  booleans, blobs and date, time and timestamp. Results are collected page by
  page, and a request's row limit bounds both the walk and the page size: the
  driver asks for the budget, plus one row when the budget is met, which is
  what decides whether the answer was longer. A result that holds more rows
  than the limit is marked truncated.
- Structured errors: the server's numeric CQL code and its reason are shown
  apart from driver and network failures, and syntax, permission and
  authentication failures keep their own `DbError` variants.
- A CQL language service decides what a statement does before it runs. A
  `SELECT` is a read, a `SELECT` of a system keyspace, `DESCRIBE` and `USE`
  are metadata, `INSERT` and `UPDATE` are writes, `CREATE` is safe
  administration, `ALTER`, `DROP INDEX`, `GRANT` and `REVOKE` are
  administration, and `DROP` of any other object and `TRUNCATE` are
  destructive administration. A `DELETE` is classified against the table's
  primary key: it is a write only when its `WHERE` constrains every key column
  with `=`, which addresses one row, and destructive — confirmed before it
  runs — when a key column is missing or compared any other way (`>`, `>=`,
  `<`, `<=`, `IN`, `CONTAINS`, a slice or a tuple relation), because that shape
  removes a range of rows or a whole partition. The key columns come from the
  `system_schema.columns` rows the schema browse has already read, and a table
  whose details this session has not read is flagged too. A delete that names
  a bare table is judged in the keyspace the session runs in, and only there;
  a schema change run in the session drops what has been read, so a table that
  replaced a dropped one is never judged by the old key.
- A `BEGIN BATCH … APPLY BATCH` block runs as one statement: the editor splits
  a buffer at its `;` but keeps a batch whole, and an unclosed batch is
  reported before it runs.
- A table browse pages through the server's paging state. CQL has no
  `OFFSET`, so the grid's page numbers are answered from the state each page
  starts at: the statement carries no `LIMIT` (a CQL `LIMIT` bounds the whole
  query, so page 2 would be empty), and a page the server ends early is filled
  with the rows that follow. A per-connection cache keeps each page's start
  position per relation, filter and page size — 64 of them, with the oldest
  dropped — so next, previous and a refresh reuse it, and a forward jump walks
  the pages in between once, up to a thousand pages.
- Row edits are prepared lightweight transactions: `UPDATE … WHERE <full key>
  IF EXISTS`, `INSERT … IF NOT EXISTS`, `DELETE … WHERE <full key> IF EXISTS`.
  A plain CQL `INSERT` or `UPDATE` is an upsert, so the guard is what keeps an
  edit of a row someone else deleted from creating it again; when the server
  answers `[applied] = false`, the grid reports an error instead of a success.
  A row is addressed by its whole primary key, and the driver refuses an
  identity that stops at the partition key, because that `WHERE` would write
  or remove a whole partition. A materialized view is read-only, and so is a
  counter table: a counter changes by increment, never by assignment.
- A generator writes the CQL for reads and edits: structured reads for MCP's
  `select_data`, the select-by-key and edit templates, and copy-as-query. It
  quotes identifiers with double quotes, binds with `?` and writes no
  `OFFSET`; a join, a grouping, a sort or an offset is refused with a message
  that names the CQL limit. The edit text carries the same `IF EXISTS` /
  `IF NOT EXISTS` guard the grid runs.
- The grid offers no header sort for this driver's tables: the driver declares
  no `SORTING`, so the header carries no sort and its tooltip says the database
  cannot sort this view.

## Limitations

- The live suites run on throwaway containers of both servers: `cassandra:5.0`
  and `scylladb/scylla:6.2`, one container at a time, removed when the test
  ends. They cover the password path (a port that refuses both an anonymous
  connection and a wrong password), the lazy schema with key roles, static
  columns, indexes and a view's base table, every CQL type round-tripping, three
  pages forward and back over one partition, edits by key with the
  lightweight-transaction refusal, a view and a counter table refusing an edit,
  the classification of dangerous statements through the live connection's own
  service, and a formatted server error with the server's own code. The TLS test
  runs on ScyllaDB in all three encrypting modes — `require`, `verify-ca` and
  `verify-full`, the last one against a certificate the test issues with an IP
  SAN for the address it dials — with an authority generated when it runs; the
  Cassandra and ScyllaDB containers share one client TLS path. All of them pass
  on `cassandra:5.0` and `scylladb/scylla:6.2`: the two suites, and the TLS test
  in its three modes.
- A row count is refused: Cassandra has no `COUNT(*)` outside a partition, so
  counting a table means reading all of it. The grid pages by row count
  instead, so the last page is the one whose rows run out.
- There is no server-side sort. CQL orders a partition by its clustering
  columns and nothing else, and an `ORDER BY` on another column needs the
  partition pinned first, so the grid offers no sort for a table rather than
  claim one it cannot express.
- Each row edit is a lightweight transaction, which costs a Paxos round: an
  edit is slower than a plain write, and two edits of the same row from two
  sessions can make one of them wait. That is what buys the "no silent
  upsert" rule.
- Schema diff does not offer a Cassandra connection: that workflow builds and
  applies SQL DDL, so it needs a relational driver whose language is SQL.
- Schema browsing reads one keyspace at a time. There is no "load everything"
  path, which is what keeps a cluster with hundreds of keyspaces responsive.
- SSH tunnels and proxies are refused. The driver cannot yet prove that a
  tunnelled session stays pinned to the one forwarded node, and an unpinned
  session would reach the cluster's other nodes directly, around the tunnel. A
  tunnel, when it lands, keeps the session on that one node, so a tunnelled
  cluster is served by one coordinator for every request rather than by the
  node that owns each partition. The client crate speaks no proxy protocol at
  all, so proxy access is not a matter of a missing setting.
- `verify-ca` is the practical verifying mode: it checks that each node's
  certificate chains to the trusted root but not that the name matches,
  because the driver dials every node by its IP address. `verify-full` also
  checks the name, so every node needs a certificate carrying an IP SAN for
  the address DataTab dials. `require` encrypts without verifying the cluster
  at all, and is the mode to avoid outside a test.
- The keyspace is selected with a quoted `USE`, so a name is used exactly as
  written. A keyspace stored in mixed case must be typed in that case; an
  unquoted name in CQL would otherwise fold to lower case.
- CQL has no transactions (`BEGIN`/`COMMIT`), so the driver advertises none:
  every row edit stands alone, and a run of them is not atomic.
- An executed statement reports no affected-row count: CQL answers a write
  with a void result. A row edit reports one row, because the lightweight
  transaction's `[applied]` answer says whether it happened.
- The key-aware rule reads a cache the schema browse fills, so a cold cache
  means a confirmation on the first keyed delete of a session: until a table's
  details have been read, a `DELETE` on it confirms as a destructive one.
  Opening the table in the tree is enough to fill its entry, and any schema
  change the session runs empties the cache, so the first delete after a
  `CREATE`, `ALTER`, `DROP`, `GRANT` or `TRUNCATE` confirms again. CQL-only
  keywords such as `BATCH` and `IF NOT EXISTS` are not offered by completion
  yet.
- Bound decimals, collections and user-defined types are refused. The driver,
  not the protocol, is the limit here: row edits prepare their statements, so
  the server would know the column type, but the driver still chooses the
  bound CQL type from the `Value` alone.
- A bound integer is sent as a CQL `int` whenever it fits, so binding into a
  `bigint` or `counter` column fails the type check until prepared statements
  let the driver choose the bound type from the column.
- A `float` column needs a cast when bound, because a bound floating-point
  value is sent as a CQL `double`.
- The consistency level is the driver's default (`LOCAL_QUORUM`); a profile
  field for it is not there yet. A row edit's lightweight transaction runs at
  `LOCAL_SERIAL`. The connect timeout is fixed at 10 seconds.
- A running statement cannot be cancelled, client-side or otherwise: CQL gives
  the driver no request id to stop, so a query that is already at the server
  runs to completion or to its timeout.
- Type names are reported as CQL writes them (`frozen<map<text, bigint>>`),
  and a result column always shows as nullable: CQL result metadata carries
  neither nullability nor key information.
