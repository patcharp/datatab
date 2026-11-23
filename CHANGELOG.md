# Changelog

All notable changes to DataTab will be documented in this file.

## [0.1.4] - 2026-11-24

### Added

* **Crash and error reports** — When DataTab crashes or an operation fails, it can offer you a report. The window shows every byte it holds, **Don't send** is the default, and nothing leaves your machine until you press Send, one report at a time. A sent report can be deleted from Settings → About, and a setting, an environment variable or a managed file turns the offer off.
* **Cassandra and ScyllaDB** — DataTab connects to Apache Cassandra and ScyllaDB, with a connection form that carries the TLS settings, and the driver ships in every build, the MCP server included.
* **Cassandra and ScyllaDB schema** — The sidebar lists keyspaces, tables, materialized views, user types, indexes and CQL key roles.
* **Cassandra and ScyllaDB** — CQL statements are classified by the driver, so a `SELECT` is a read, `GRANT` and `REVOKE` count as administration, `DROP`, `TRUNCATE` and `ALTER` ask for confirmation, and a `BEGIN BATCH … APPLY BATCH` block runs as one statement.
* **Cassandra and ScyllaDB `DELETE`** — A `DELETE` is classified against the table's primary key, so one that does not name every key column with `=` asks for confirmation before it runs.
* **Cassandra and ScyllaDB tables** — Browsing a table pages through the server's paging state, and a row can be edited, inserted and deleted through a lightweight transaction (`IF EXISTS` / `IF NOT EXISTS`). A table whose database cannot sort offers no header sort and says why.
* **About** — The About page links to the public source repository.
* **Column names** — Copy a column or field name from the schema tree, or the active column name from the grid header or the table menu.
* **Schema diff** — Create and drop tables on DuckDB and ClickHouse, including a ClickHouse Cloud table's shared engine.
* **ClickHouse TLS** — A custom CA, a client certificate for mutual TLS and a self-signed server, with a mode for each: the modes that do not check the certificate say so before you connect.
* **Format SQL** — Format the whole buffer or the selection from a command, `Alt+Shift+F`, the command palette or the editor toolbar, on one undo step.
* **Editor** — The split between the editor and its results is remembered per arrangement, so it comes back at the same place next time.
* **Editor** — A failed query moves the cursor to the position the database named, where the driver reports one.
* **Results** — A free-form result that cannot be edited says why, and points at the visual query builder.
* **Thai data** — The grid, the editor and every surface that shows data fall back to a bundled Noto Sans Thai, so Thai text renders at the size of the text around it instead of a system font.
* **Schema tree** — MySQL, SQL Server, SQLite and DuckDB load every column of a schema in one catalog read, so a large schema opens with far fewer round trips.
* **macOS releases** — Signing runs in its own job on a fresh machine and the temporary keychain is removed as soon as the signature is verified.

### Changed

* **Editor** — The editor's context bar shows the last run time once.

### Fixed

* **Passwords** — A connection whose password comes from a value source, or whose Save password is off, no longer unlocks the system keyring. A broken value source says the saved password was not read instead of blaming a locked keyring, and a connection whose driver has no password field can still be saved.
* **Editing a connection** — Editing a connection no longer drops the value sources of fields the form has no selector for, and a saved source for a field an external driver no longer declares is dropped when the connection loads.
* **External drivers** — Connections made with an external (RPC) driver keep their settings after a restart.
* **Auth profile form** — A dropdown that asks an RPC provider for its options sends one request at a time, does not ask a failing provider again on every repaint, and offers Retry on a field that failed.
* **External auth providers** — An auth provider that runs as an RPC service and never answers its handshake no longer holds up app start, and several providers are probed at the same time instead of one after another.
* **Settings** — The login dialog, with its Cancel button, opens however Settings was opened.
* **Import** — Importing a bundle saves the auth profile you chose in the import panel, and a choice that names a deleted profile fails that connection with the reason. A second import cannot start while one runs, and a refused import is reported once and stays readable after the window closes.
* **Connections** — Saving a connection and importing a bundle no longer freeze the window while secrets are written to the system keyring.
* **Connection manager** — A connection has one edit window, and it cannot be deleted while that window is open.
* **SQL Server** — Cancelling a query no longer interrupts or kills the session that recovery opened, a query cancelled before it was sent is not sent, and a cancel that stalls gives up after 10 seconds. The window no longer freezes while the connection recovers after a cancel.
* **Instance metrics** — MongoDB resident memory and SQL Server total server memory show the right size, and InfluxDB BoltDB and HTTP request counts show a rate.
* **Charts** — A chart saved in a query language DataTab no longer supports is refused at save with the reason, instead of disappearing at the next start.
* **Redis Sentinel** — Connection and authentication errors name the Sentinel node that failed.
* **SQL Server** — A foreign key's `ON DELETE` and `ON UPDATE` actions no longer show exchanged in table details; the actions now match the server's catalog.
* **Linux** — The window's client inset matches the shadow it draws.

### Security

* **PostgreSQL and Redshift secrets** — Connect errors and logs mask every libpq secret key, a misspelled or unknown key, a key nested in quotes, keys with non-ASCII letters, and the escaped apostrophe of an unclosed value.
* **External drivers** — The secret fields an external (RPC) driver declares besides the password are kept in the OS keyring instead of DataTab's database. Values that older versions stored are moved there at startup, and they never reach an export or a duplicate.
* **Saved values** — A literal value reference on a secret field is no longer written to the database or to an export. It moves to the system keyring, or the export asks for it on import.
* **Value sources** — When a driver's connection form cannot be loaded, a new literal value source on a secret field is refused, with the reason, instead of being stored.
* **Redis Sentinel** — A Sentinel connection in TLS mode checks the certificate chain against your private CAs, presents your client certificate on discovery and master connections, and keeps doing so after a failover, with no plaintext fallback.
* **MCP governance** — A batch that mixes a destructive statement with an administrative one is governed as the worse of the two, and a governance ceiling reads the data-risk ladder as well as the permission ladder, so a level that admits only administration no longer lets a statement that removes rows through.
* **MCP governance** — A `DELETE` or `UPDATE` with no `WHERE` is destructive, so a policy that allows only `write` no longer runs a whole-table write without approval.
* **MCP policies** — A policy may no longer allow an execution class while it decides a class below it more strictly. Saving such a policy is refused with an error that names the pair, and a stored one grants nothing.
* **Query classification** — A `DELETE` or `UPDATE` with no `WHERE` is destructive, inside a stored procedure, a data-modifying `WITH` or an `EXPLAIN ANALYZE`, and the confirmation dialog names the pattern the policy acted on.
* **MCP approvals** — An approval request carries the operation's own class, so a removal is never queued as an administrative change.

### Upgrading

* **Before you upgrade** — DataTab can now send a crash or error report, and only when you press **Send**. Nothing is sent automatically. **Settings → General → Offer to send a report** turns the offer off, and so does `DATATAB_DISABLE_ERROR_REPORTS` or a managed `error-reports-disabled` file for an organization.
* **Windows** — Close DataTab, then run the new `datatab-windows-amd64-setup.exe`. It installs over 0.1.3 and keeps your data.
* **Debian and Ubuntu** — Close DataTab, then run `sudo apt install ./datatab-linux-amd64.deb` with the new package.
* **macOS** — Close DataTab, open the disk image for your Mac and drag `DataTab.app` to Applications, replacing the old one. Run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. The app is signed with the PCPLab certificate, so macOS may ask once more for access to a saved password: choose **Always Allow**. Your data stays in its folder.

### Rollback

* **Windows** — Uninstall DataTab in Settings, Installed apps, then run the 0.1.3 installer. Your data folder stays.
* **Debian and Ubuntu** — Run `sudo apt install --allow-downgrades ./datatab-linux-amd64.deb` with the 0.1.3 package.
* **macOS** — Close DataTab, then replace `DataTab.app` with the 0.1.3 one, and run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. Your data stays.

## [0.1.3] - 2026-11-16

### Added

* **macOS disk images** — Each release has a disk image for Apple Silicon and one for Intel. Drag DataTab to Applications to install it.

### Changed

* **Title bar** — DataTab draws its own themed window title bar on every supported desktop.
* **macOS signing** — macOS builds are signed with the PCPLab project certificate instead of an ad-hoc signature.
* **Release notes** — Each GitHub release lists the version's changes, with upgrade and rollback steps taken from this changelog.
* **Building from source** — The Debian package is built with a pinned nfpm container by default. Set `DATATAB_NFPM_LOCAL=1` to use a local `nfpm` instead.

### Removed

* **AWS** — DynamoDB, CloudWatch Logs, Amazon S3, AWS Secrets Manager, Parameter Store, AWS SSO and shared-credential sign-in, and SSM Port Forwarding are gone from DataTab and from its source. Saved DynamoDB, CloudWatch Logs, S3 and AWS Secrets Manager connections are no longer listed, and a bundle no longer imports them. Another connection that uses SSM access, an AWS sign-in profile, or a password or other value from AWS Secrets Manager or Parameter Store stays in the list but no longer connects until you change that setting. RPC secret providers still work.
* **AWS-only features** — The secret-store browser, the "Open AWS SSO wizard" command and its Ctrl+Shift+O shortcut (Cmd+Shift+O on macOS), the CloudWatch metric catalog and the "From metric" tab of Add Panel, remote dashboards, "Import dashboard from JSON…" and the event stream viewer are gone. Instance metrics, the Instance Overview and local dashboards are unchanged. A saved CloudWatch metric chart is skipped when charts load. Older audit rows in the Secret Store category still load.
* **Scripts** — `.cwli` and `.ppl` files open as SQL. A saved chart or an open tab in a CloudWatch or OpenSearch query language is no longer restored in that language.

### Fixed

* **SSH tunnels** — An idle tunnel stays open: DataTab sends an SSH keepalive every 60 seconds, so a NAT gateway or firewall does not drop the session.
* **SQL Server** — A session that the server, a firewall or a NAT gateway dropped is reconnected on the next action, with the active database restored. A query you ran is never sent twice.
* **DuckDB** — A connection refresh keeps the catalog that Init SQL switched to.
* **Instance metrics** — Counters such as transactions per second, PostgreSQL disk block reads, MongoDB pages evicted, MySQL buffer-pool reads and network bytes, Redis commands and keyspace hits, and ClickHouse rows read and inserted show a per-second rate instead of an ever-rising total.
* **Charts** — Time axes, the hover readout, the point inspector and the stats window show local time, with ticks on local hour and calendar boundaries, DST changes included. Neighbouring y-axis ticks no longer repeat a label, and the PostgreSQL activity inspector no longer shows negative query ages.
* **Charts** — Changing a chart's data source no longer shows a result from the previous source, and a cancelled chart run no longer stays "running".
* **Responsiveness** — Opening or copying the Instance Overview, and the instance inspector's row actions and kill confirmations, no longer freeze the window while the server's permissions are checked. A reconnect during that check drops the stale result.
* **Command line** — `datatab --help`, `--version` and mistyped arguments answer in the terminal instead of starting the app, and a path that is not a file is refused. `datatab <file>` no longer waits forever when a running DataTab does not answer, waits up to 30 seconds for an app it just started, and opens each script once, also when you open several at once.
* **Windows paths** — File save, read and spreadsheet errors, and scripts opened from another application, no longer show the `\\?\` prefix.
* **Redshift** — Root CA paths saved for a TLS mode that does not check the certificate chain are ignored.
* **Auth profiles** — Editing a stored auth profile in Settings saves again, and its Enabled switch, Export and Delete are back.
* **External auth providers** — The window no longer freezes while an auth provider that runs as an RPC service signs in or checks a session. The sign-in link and Cancel appear while the provider waits for the browser. Cancel gives the Login button back at once, the sign-in dialog closes when the sign-in ends, and a sign-in that finishes after Cancel, or after you switched to another profile or provider, no longer changes the form. A sign-in may now take as long as the sign-in dialog waits (5 minutes), and a provider that never answers ends the request at its deadline.
* **External auth providers** — A dynamic dropdown of an RPC auth provider (protocol v1.2 or later) now asks the provider for its options instead of answering "not supported". The request carries only the profile fields that provider's form declares, and no secret values unless the provider opted in.
* **MCP server** — The server keeps listing your other connections when one stored connection has no usable driver configuration.
* **Connections** — A connection whose driver declares a database kind DataTab no longer supports is refused at save and at import, with the reason, instead of disappearing at the next start.

### Security

* **Connections** — A connection that its driver cannot open safely (for example a Redis Sentinel setup with an unsupported TLS mode) is refused before DataTab runs pre-connect hooks, reads saved passwords or proxy and SSH secrets, asks for a password, or opens the network.
* **TLS modes** — Every TLS mode that does not check the server certificate says so in the connection form, and the certificate pickers show only for the modes that use them. Unknown or ambiguous TLS modes are refused instead of silently weakening the connection.
* **PostgreSQL and Redshift TLS** — The TLS mode of keyword connection strings and of URLs is enforced. URLs whose credentials hide a query (an unencoded `?` before `@`) or whose query keys are percent-encoded are refused, and a rejected `sslmode` value is not repeated in errors.
* **Redshift** — In `prefer` mode, DataTab no longer retries the login without TLS after an authentication error, a timeout or another non-TLS failure. Editing a saved Redshift connection no longer resets its TLS mode to `disable`.
* **MongoDB** — Client certificate files are not read in TLS modes that do not use them.
* **Saved values** — A connection whose saved password, host, user or database reference cannot be resolved, or has an unknown kind, no longer falls back to another value. It refuses to connect, names the broken field, and cannot be saved until you choose a replacement or turn Save password off. The MCP server rejects only that connection and keeps serving the others.
* **Bundles** — Importing a bundle no longer drops a value source it cannot read. That connection is skipped and reported with its field names, instead of being imported with the plain field text.
* **Keyring** — Connecting reads only the selected SSH tunnel or proxy secret from the keyring, not every stored one.
* **Error messages** — A failed PostgreSQL or Redshift connection that uses a keyword connection string (`host=… password=…`) no longer shows the password or other secret values in the error or the log.

### Upgrading

* **Before you upgrade** — DataTab no longer supports AWS services. Connections to DynamoDB, CloudWatch Logs, Amazon S3 or AWS Secrets Manager disappear from the list after the upgrade. A connection that uses SSM access, an AWS sign-in profile, or a value from AWS Secrets Manager or Parameter Store stays, but no longer connects until you replace that setting. Note these settings first if you still need them.
* **Windows** — Close DataTab, then run the new `datatab-windows-amd64-setup.exe`. It installs over 0.1.2 and keeps your data.
* **Debian and Ubuntu** — Close DataTab, then run `sudo apt install ./datatab-linux-amd64.deb` with the new package.
* **macOS** — Close DataTab, open the disk image for your Mac and drag `DataTab.app` to Applications, replacing the old one. Run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. The app is now signed with the PCPLab certificate, so macOS may ask once more for access to a saved password: choose **Always Allow**. Your data stays in its folder.

### Rollback

* **Windows** — Uninstall DataTab in Settings, Installed apps, then run the 0.1.2 installer. Your data folder stays.
* **Debian and Ubuntu** — Run `sudo apt install --allow-downgrades ./datatab-linux-amd64.deb` with the 0.1.2 package.
* **macOS** — Close DataTab, then replace `DataTab.app` with the 0.1.2 one, and run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. Your data stays in its folder.

## [0.1.2] - 2026-11-13

### Added

* **ClickHouse over HTTP** — A per-connection "Allow password over unencrypted HTTP" option, off by default, for servers that have only plain HTTP. Without it, the connection error now says how to fix the problem.

### Changed

* **macOS** — macOS is a fully supported platform, on Intel and Apple Silicon, like Windows and Linux.
* **Error toasts** — With the toast delay on, an error toast closes on its own after twice the delay, unless it shows progress or an action that needs you. A repeat of the same error merges into the visible toast with a count. Fatal errors stay until dismissed. Reported errors stay in the status-bar badge and the audit log after their toast closes.

### Fixed

* **Charts** — The hover marker is a diamond on every platform, and the crosshair and readout clear when the pointer leaves the chart.
* **Tabs** — Document tabs keep a readable title in a crowded row, and Windows script paths no longer show the `\\?\` prefix.

### Upgrading

* **Windows** — Close DataTab, then run the new `datatab-windows-amd64-setup.exe`. It installs over 0.1.1 and keeps your data.
* **Debian and Ubuntu** — Close DataTab, then run `sudo apt install ./datatab-linux-amd64.deb` with the new package.
* **macOS** — Close DataTab, then replace `DataTab.app` with the new one, and run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. Your data stays in its folder.

### Rollback

* **Windows** — Uninstall DataTab in Settings, Installed apps, then run the 0.1.1 installer. Your data folder stays.
* **Debian and Ubuntu** — Run `sudo apt install --allow-downgrades ./datatab-linux-amd64.deb` with the 0.1.1 package.
* **macOS** — Close DataTab, then replace `DataTab.app` with the 0.1.1 one, and run `xattr -dr com.apple.quarantine /Applications/DataTab.app` before you start it. Your data stays in its folder.

## [0.1.1] - 2026-11-12

### Added

* **Downloads on GitHub** — Each release is published at github.com/patcharp/datatab with a Linux `.deb`, a Windows installer and SHA-256 checksums.
* **macOS build** — An experimental `DataTab.app` for macOS on Intel and Apple Silicon. It is not signed: before the first start of a downloaded copy, run `xattr -dr com.apple.quarantine DataTab.app`.

### Changed

* **Public source** — The public repository holds the source you need to build DataTab. See [Building from source](docs/BUILDING.md).

### Upgrading

* **Windows** — Close DataTab, then run the new `datatab-windows-amd64-setup.exe`. It installs over 0.1.0 and keeps your data.
* **Debian and Ubuntu** — Close DataTab, then run `sudo apt install ./datatab-linux-amd64.deb` with the new package.
* **macOS** — Close DataTab, then replace `DataTab.app` with the new one. Your data stays in its folder.

### Rollback

* **Windows** — Uninstall DataTab in Settings, Installed apps, then run the 0.1.0 installer. Your data folder stays.
* **Debian and Ubuntu** — Run `sudo apt install --allow-downgrades ./datatab-linux-amd64.deb` with the 0.1.0 package.
* **macOS** — 0.1.1 is the first macOS build, so there is no earlier version to go back to. Delete `DataTab.app` to remove it.

## [0.1.0] - 2026-11-12

### Added

* **First DataTab build** — Derived from DBFlux. Not the official DBFlux.
* **New name, logo and identity** — DataTab has its own name, logo, publisher, installer identity and contact address.
* **Build from source** — `scripts/build-linux.sh` and `scripts/build-windows.ps1` build the app and its installer. [Building from source](docs/BUILDING.md) explains the steps and the common failures.

### Changed

* **DBFlux data is not read** — DataTab does not read data, settings or keyring entries from DBFlux, so it starts without your DBFlux connections and secrets.
* **Deep Tide palette** — Refresh the app colors with the Deep Tide palette.
* **Section labels** — Read section labels in mixed case with clearer, larger text.
* **Loading spinner** — Show a walking cell spinner while loading.
* **One cut** — Controls and panes now use a single cut at the top right.
* **English only** — DataTab ships in English (US) only. The Language setting and the other four catalogs are removed, and a stored language choice is ignored.
* **Interface font** — The interface uses IBM Plex Sans Thai, so Thai and Latin text share one family. Data and code stay in JetBrains Mono.
* **Welcome screen** — The first-run screen teaches three keys and offers to add a connection. The changelog stays in Settings › Updates.
* **Joined tabs** — The active document tab joins its content.
* **Wash contrast** — Status chips, menu rows, grid states and controls are easier to read in the dark and light themes.

### Fixed

* **Website drivers** — See all supported databases and services on the home page.
* **SQLite completion** — Completion no longer offers `public.<table>` after a query runs.
* **Typing in text fields** — Printable keys stay in a focused text field while results, audit or inspector views are open.
* **Double click to connect** — A double click on a connection connects once. A second connect request while the first is pending is ignored without an error.
* **Dropdown scrolling** — Scrolling an open dropdown no longer moves the highlighted row.
* **SQL Server idle sessions** — Idle sessions stay open. TCP keepalive sends its first probe after 60 s idle, so a NAT gateway or firewall does not drop the session.
* **Sidebar resize** — The seam between the sidebar and the document resizes the sidebar.
* **Larger click targets** — Small tab, tree and delete controls have larger click targets.
* **Grid range selection** — After a click on a cell, Shift+arrow keys extend the selection again.
* **Selected grid text** — Muted and missing values stay readable in a selected range.
* **Separate profiles** — DataTab instances with separate data folders now run side by side. Close DataTab before you upgrade: the new build does not find an older one that is still running.
* **MCP disconnect** — Prevent a panic when an MCP client disconnects a PostgreSQL connection.
* **PostgreSQL query cancel over TLS** — Cancelling a query now works on connections with `sslmode` set to `require`, `verify-ca` or `verify-full`, where it failed and the query kept running. These modes never send a cancel request without TLS. With `prefer`, the default, a cancel request now attempts TLS, but it still goes out in clear text if the server refuses TLS on the cancel connection, even when the session is encrypted. We recommend `verify-full`.
* **Redshift query cancel over TLS** — Cancelling a query now works on connections with `sslmode` set to `require`, `verify-ca` or `verify-full`, where it failed and the query kept running. These modes never send a cancel request without TLS. With `prefer`, the default, a cancel request now attempts TLS, but it still goes out in clear text if the server refuses TLS on the cancel connection, even when the session is encrypted. We recommend `verify-full`.
* **Redis TLS mode "on"** — The mode now connects, encrypted and without checking the server's certificate, and the form says so. Before, every connection in this mode failed with an error about a missing feature. A `rediss://` URI with `#insecure` now connects the same way. Both stay refused with the Sentinel topology. Use `verify` to check the server's identity.
* **Debian package** — The menu entry shows the name DataTab and starts the app. The package requires glibc 2.39, so it installs on Ubuntu 24.04, Debian 13 or later.

### Security

* **Redis Sentinel with TLS** — A Redis profile with the Sentinel topology and TLS now stops before it connects, with a message that says why. This covers TLS modes `on` and `verify` and every `rediss://` URI. Before, with `verify` or a `rediss://` URI, DataTab sent the password and every command to the Sentinel master in clear text, while the form said the connection was verified. DataTab cannot reach the Sentinel master over TLS yet. To keep using Sentinel, set the TLS mode to `off` or use a `redis://` URI.

### Upgrading

* **First release** — 0.1.0 is the first DataTab release, so there is no earlier DataTab to upgrade from. Your connections are not carried over: add them again.
* **Later upgrades on Windows** — Close DataTab, then run the new `datatab-windows-amd64-setup.exe`. It installs over the old version and keeps your data.
* **Later upgrades on Debian and Ubuntu** — Close DataTab, then run `sudo apt install ./datatab-linux-amd64.deb` with the new package.

### Rollback

* **Windows** — Uninstall DataTab in Settings, Installed apps. Your data folder stays. To go back to an older version later, run its installer.
* **Debian and Ubuntu** — Run `sudo apt remove datatab`. Your data folder stays. To go back to an older version later, run `sudo apt install --allow-downgrades ./datatab-linux-amd64.deb` with the older package.
* **Data** — Removing the app does not remove your data. [Privacy](PRIVACY.md) lists the data folders.
