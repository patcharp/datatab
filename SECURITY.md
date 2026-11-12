# Security Policy

## Reporting a vulnerability

Report suspected vulnerabilities privately to [me@pcplab.me](mailto:me@pcplab.me). Do not open a public issue. Include the DataTab version, platform, driver, and steps to reproduce the problem.

DataTab is published by PCPLab. It is derived from DBFlux and is not an official DBFlux build.

## Supported versions

Security fixes are considered for the current release.

## Known limitations, by design

These are documented behaviours rather than vulnerabilities. A report about them
is welcome as a design discussion, but they are not treated as an undisclosed
risk.

- **MCP authentication is process identity only.** Presenting `--client-id` is
  the sole authentication signal, so any local process that knows the client id
  can connect. It is not a cryptographic guarantee, and the MCP server should not
  be exposed beyond localhost without an additional authentication layer. See
  [AI + MCP integration](docs/MCP_AI_INTEGRATION.md).
- **Connection hooks and Lua scripts run code you configured.** They execute with
  the privileges of the DataTab process by design; that is what a hook is. See
  [Settings and hooks](docs/SETTINGS.md) and [Lua scripting](docs/LUA.md).
- **The audit log is local.** It records what happened on that machine and is
  readable by anything that can read your data directory. See [data and
  privacy](PRIVACY.md#your-data-on-this-machine).

## Where secrets live

Credentials are held in the operating system keyring, never in a connection
profile file, and the audit log stores a fingerprint of query text rather than
the text itself. [Data and privacy](PRIVACY.md#your-data-on-this-machine) describes what is
written where, and how to inspect or remove it.
