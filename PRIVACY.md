# Privacy Policy

DataTab is published by PCPLab. See the [security policy](SECURITY.md) for its origin.

DataTab is a local-first desktop application. It does not collect, transmit, or
store any information about you or your usage. The project website sets no
cookies and loads no third-party scripts. This document says what that means
in practice.

## Quick path

1. The application sends data only to the database servers you configure, and
   only what is needed to run the queries you ask for.
2. There is no telemetry, no usage analytics, and no account. The one message
   that can reach us is a crash or error report, and only by hand: the app shows
   you every byte first and sends nothing until you press **Send**. See
   [Crash and error reports](#crash-and-error-reports).
3. The website and documentation are static pages. They set no cookies, load
   no third-party script, and keep no record of individual visitors.

## The application

| Question | Answer |
|----------|--------|
| Does DataTab send usage data anywhere? | No. There is no telemetry and no usage analytics. A crash or error report is sent only when you press **Send** in its window, one report at a time. |
| Does it check for updates? | No. |
| What network connections does it open? | Only the ones you configure: database servers, SSH tunnels, proxies, and cloud APIs for the drivers you use. A report you send yourself goes to `reports.datatab.pcplab.me` over HTTPS, and nothing else does. |
| Where does my data live? | On your machine, in one SQLite file plus the operating system keyring for secrets. |
| Does it keep a record of what I do? | Only the audit log, on your machine. It records connections, queries, and hook runs so you can review them, and it never leaves the computer it was written on. |
| Is anything shared with the project? | No. Bug reports and logs reach the project only when you send them yourself. |

Connections to a database, an SSH host, a proxy, or a cloud provider go
directly from your machine to the server you named. The project operates none
of those servers and has no visibility into that traffic.

## Crash and error reports

When DataTab panics, or an operation fails in a way it reports, it can offer you
a report. Nothing leaves your machine until you press **Send**, one report at a
time, and you can always decline.

### What you see first

The window shows the exact bytes that will be sent, so you can read the report
before deciding. **Don't send** is the default button. **Save as file** writes the
same bytes to a file you choose and sends nothing. Declining keeps the report on
your machine and the app does not ask you about it again.

A waiting report lives in the app's data folder, next to the database file, and
at most five are kept. You can review them, send them or decline them in
**Settings → About**; declining marks the report decided and leaves the file on
disk.

### What a report holds

| Field | Value |
|-------|-------|
| Report id | A random identifier, new for each report |
| Schema | The report format's version, `1` |
| Kind | Panic or error |
| App version and channel | The version you run, and whether it is stable, RC or Nightly |
| System | Your operating system's name, version where the platform reports one, and architecture |
| Time | The UTC hour the report was created, rounded down |
| Error | The kind and severity; the driver id and the server's error code when there is one |
| Panic message | Only when it is fixed text written into DataTab's own source; a formatted message is replaced with a note that text was dropped |
| Location | The `file:line` of the panic, shortened: a path inside DataTab's source keeps `crates/…`, a dependency keeps its crate folder, and anything else reads `<external>` |
| Frames | Up to 64 stack frames: function name when it can be read, source file name without its folder, and an offset |
| Recent activity | The last 10 audit events, as category, action, outcome, driver id and seconds before the report — never query text or data |

A report is at most 64 KiB, and unknown fields are refused by the receiver.

### What a report never holds

No query text, no rows or values you looked at, no table or column names, no
connection strings, no passwords or other secrets, no file paths outside the
shortened source location, and no host names or addresses it can recognise, nor
email addresses. The values above pass through a scrubber that drops anything
that looks like a credential, a host with a port, a path, an address or an email
before the report is written.

### Where it goes, and for how long

A report you send goes over HTTPS to `reports.datatab.pcplab.me`, a service the
project runs. It is kept for 90 days and then deleted automatically. Only the
project's maintainer reads reports. To have one deleted sooner, use Settings →
About and choose Delete from the server, which also works while reports are
off; if the report no longer has its delete token — for instance one sent
before DataTab kept tokens — write to `me@pcplab.me` with its report id.

### Turning it off

Any one of these is enough, and none of them can turn reports back on:

| Source | Value |
|--------|-------|
| Setting | **General → Offer to send a report**, off |
| Environment variable | `DATATAB_DISABLE_ERROR_REPORTS` set to anything other than empty, `0`, `false`, `no` or `off` |
| Managed file | An empty `error-reports-disabled` file in the app's data folder, `/etc/datatab/` on Linux, `%ProgramData%\DataTab\` on Windows, or `/Library/Application Support/DataTab/` on macOS |

With reports off, the app shows no Send button and sends nothing. A panic still
writes its report to the data folder, where the app keeps it and never offers
it while reports are off.

[Your data on this machine](#your-data-on-this-machine) documents the files
DataTab writes, what the audit log records, how secrets are stored, and how to
back up or fully reset the application.

## Your data on this machine

Where DataTab stores your data, how it protects your credentials, what the audit
log keeps, and how to back up or fully reset.

### At a glance

| Your data | Where it lives |
|-----------|----------------|
| Connection profiles, settings, history, saved charts/queries, audit log | One SQLite file: `datatab.db` in the data directory |
| Open tabs / session | The same `datatab.db`, plus script files and scratch/shadow files on disk |
| Passwords, passphrases, API secrets | Your **OS keyring** — never in `datatab.db` |
| IPC/MCP auth token | A `0600` file in the config directory |

DataTab keeps almost everything in a single SQLite database. Secrets are the
deliberate exception: they go to the operating system's keyring, and the database
only stores a *reference* to them.

### Data locations

DataTab uses your platform's standard directories.

| Platform | Data directory | Config directory |
|----------|----------------|------------------|
| **Linux** | `~/.local/share/datatab/` | `~/.config/datatab/` |
| **Windows** | `%APPDATA%\datatab\` | `%APPDATA%\datatab\` |
| **macOS** | `~/Library/Application Support/datatab/` | `~/Library/Application Support/datatab/` |

The data directory holds:

- **`datatab.db`** — the unified database (everything below in [What's in the
  database](#whats-in-the-database)).
- **`sessions/`** — scratch and shadow files for open editor tabs: untitled
  content, plus a recovery copy of unsaved edits.
- **`ipc_auth_token`** — the IPC/MCP auth token (see [below](#ipcmcp-auth-token)).
- **`ssh_known_hosts`** — accepted SSH host keys (TOFU).

DataTab no longer uses the config directory. Older versions stored the IPC auth
token and SSH known-hosts there; leftover files may remain after upgrading and
can be removed.

#### Stable vs. Nightly

A Nightly build uses a separate database file, `datatab-nightly.db`, so a
pre-release migration can never touch your stable data. Stable and release
candidate builds both use `datatab.db`.

You can make a Nightly build share the stable database via **Settings → General →
Storage → Use the stable database** (applies on next launch). Internally this
just drops an empty `use-stable-db` marker file in the data directory.

### What's in the database

`datatab.db` is a single SQLite file. Its tables are grouped by prefix:

| Prefix | Contains |
|--------|----------|
| `cfg_*` | Configuration: connection profiles, auth/proxy/SSH tunnel profiles, RPC services, connection hooks, MCP governance, and the General/Audit settings. (Secret *values* are **not** here — only keyring references.) |
| `st_*` | Workbench state: open sessions/tabs, **query history** (full query text), saved queries, recent items, schema cache, UI state. |
| `aud_*` | The audit log and saved audit filters. |
| `viz_*` | Saved charts and dashboards. |
| `qry_*` | Visual Query Builder saved queries. |
| `sys_*` | Internal: schema migration version, app metadata. |

> **Note on query history.** The workbench query history (`st_*`) stores the
> **full text** of queries you run, in the clear. This is separate from the audit
> log, which fingerprints query text by default (see below). If you don't want
> query text retained, lower **Max history entries** in Settings → General, or
> clear history from the editor's history view.

### Secrets and the OS keyring

Passwords, SSH passphrases, proxy credentials, and provider secrets are stored in
your operating system's keyring, **not** in `datatab.db`.

| Platform | Keyring backend |
|----------|-----------------|
| **Linux** | Secret Service (GNOME Keyring / KWallet, via libsecret) |
| **Windows** | Windows Credential Manager |
| **macOS** | Keychain |

All entries are stored under the service name **`datatab`**. The database holds
only a reference string per secret:

| Secret | Reference |
|--------|-----------|
| Connection password | `datatab:conn:<profile-id>` |
| Inline SSH password/passphrase | `datatab:ssh:<profile-id>` |
| Saved SSH tunnel | `datatab:ssh_tunnel:<tunnel-id>` |
| Proxy credential | `datatab:proxy:<proxy-id>` |
| Auth-profile field | `datatab:auth:<profile-id>:<field>` (one per field) |

#### When secrets are (and aren't) saved

- A connection password is only stored when you tick **Save password**; SSH and
  proxy secrets only when you tick their **Save** checkbox.
- If no keyring is available, DataTab hides the **Save** checkboxes and does not
  persist secrets — you re-enter them each session.
- A *locked* keyring still counts as available: writes may fail until you unlock
  it, but DataTab keeps secret support enabled.

### Session and tabs restore

Which tabs you have open — their kind, file paths, order, active tab, and pin
state — is recorded in `datatab.db` (`st_sessions` / `st_session_tabs`). The
scratch/shadow copies used to restore them live under `sessions/` in the data
directory. A script backed by a file is saved to that file itself: on the
auto-save interval, when you close its tab, and when you quit. Unsaved untitled
content is kept in the sessions folder. Those automatic writes never overwrite
a script file that changed outside datatab: such a write is refused, and the
pending edits stay in the editor (and in the sessions-folder copy). `Ctrl+s`
and **Save File As** are deliberate: they write the file as you asked. On
startup DataTab
restores this session when **Settings → General → Restore session on startup**
is on (the default).

### Audit and privacy

DataTab logs significant operations (queries, connections, hooks, scripts, config
changes, MCP/governance decisions) to the audit log in `datatab.db`. It's designed
to be privacy-preserving by default:

| Behavior | Default | Effect |
|----------|---------|--------|
| **Capture query text** | Off | Query text is replaced with a SHA-256 **fingerprint** plus its length — the full text is never stored in the audit row. |
| **Redact sensitive values** | On | Sensitive patterns (AWS keys, JWTs, connection strings with credentials, etc.) are replaced with `[REDACTED]`. |
| **Detail size cap** | 64 KiB | Oversized event payloads are truncated to a small partial envelope. |

Sensitive **JSON keys** (`password`, `token`, `secret`, `api_key`,
`access_key`, `session_token`, `connection_string`, `url`, …) are always redacted
— even if you turn off pattern-based redaction.

> Remember the [query-history caveat](#whats-in-the-database): the audit log
> fingerprints query text, but the *workbench history* stores it in full. They're
> two different stores.

For the complete event schema, categories, and the viewer, see
[Audit](docs/AUDIT.md), including its [Audit viewer](docs/AUDIT.md#audit-viewer)
section.

### IPC/MCP auth token

DataTab exposes a local IPC surface (used by the MCP server and external RPC
services). It authenticates callers with a token stored at:

```
<data dir>/datatab/ipc_auth_token
```

(on Linux, `~/.local/share/datatab/ipc_auth_token`). It's a random value regenerated on
each startup, written with owner-only `0600` permissions, and also exported to the
`DATATAB_IPC_TOKEN`, `DATATAB_DRIVER_IPC_TOKEN`, and `DATATAB_AUTH_PROVIDER_IPC_TOKEN`
environment variables for child processes.

This token is **process-identity only** — any local process that can read it can
connect. Do not expose the IPC/MCP surface beyond localhost without an additional
authentication layer. See [AI + MCP Integration](docs/MCP_AI_INTEGRATION.md) for the
trust model.

### Backup and reset

DataTab has no dedicated backup/restore command, but because everything lives in
one file, both are straightforward.

#### Back up

Copy the single database file while DataTab is closed:

```
~/.local/share/datatab/datatab.db        # Linux (adjust per platform)
```

That file contains your profiles, history, saved charts/queries, and audit log.
Your **secrets are not in it** — they stay in the OS keyring — so a copied
database on another machine will reference keyring entries that don't exist there
until you re-enter the secrets.

#### Full reset

To wipe DataTab's data:

1. Delete the **data directory** (`~/.local/share/datatab/` on Linux) — removes the
   database, session files, IPC auth token, and SSH known-hosts.
2. **Older versions only:** delete the legacy config directory
   (`~/.config/datatab/` on Linux) if it still exists — current versions no longer
   use it.
3. **Clear keyring entries manually.** Secrets under the `datatab` service remain
   in your OS keyring after deleting the directories; remove them with your
   platform's keyring tool if you want a complete wipe.

> Deleting the data directory is irreversible. Back up `datatab.db` first if you
> might want your profiles or history back.

## The website and documentation

`datatab.pcplab.me` is a static site built from DataTab's website source, kept in a separate repository. It:

- sets no cookies, first-party or third-party, and stores only one thing in your
  browser: the theme you pick (Auto, Light, or Dark), which stays in local
  storage and is never sent anywhere;
- loads no analytics, advertising, or tracking script;
- serves its fonts, assets, and scripts from the same host, so a page view
  contacts no other domain;
- runs a web server with its access log turned off, so it keeps no record of
  individual visitors.

The documentation search runs in your browser against an index file fetched
from the same host. The query never leaves the page.

## Changes to this policy

This file is versioned with the source code. The commit history of
`PRIVACY.md` is the change log. A change that makes the application or the
website collect anything it does not collect today will be announced in the
release notes of the version that introduces it.

## Contact

Questions go to `me@pcplab.me`.

## Related

- [Settings & Hooks](docs/SETTINGS.md) — the General/Audit/Storage controls referenced here.
- [Connecting → Advanced Setup](docs/CONNECTIONS.md) — where secrets are entered.
- [Audit](docs/AUDIT.md) — the full audit event schema and redaction details.
