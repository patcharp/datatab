# Changelog

All notable changes to DataTab will be documented in this file.

## [Unreleased]

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
