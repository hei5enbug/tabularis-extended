<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
  <img src="https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/sqlserver-icon.svg" width="120" height="120" alt="SQL Server Plus plugin icon" />
</div>

# Tabularis SQL Server Plus

<p align="center">

![Release](https://img.shields.io/github/release/hei5enbug/tabularis-sqlserver-plus.svg?style=flat)
![Downloads](https://img.shields.io/github/downloads/hei5enbug/tabularis-sqlserver-plus/total.svg?style=flat)
![Build & Release](https://github.com/hei5enbug/tabularis-sqlserver-plus/actions/workflows/release.yml/badge.svg)
[![Discord](https://img.shields.io/discord/1502944695808950282?color=5865F2&logo=discord&logoColor=white)](https://discord.com/invite/K2hmhfHRSt)

</p>

A [Microsoft SQL Server](https://www.microsoft.com/sql-server) driver for [Tabularis Extended](https://github.com/hei5enbug/tabularis-host), the Tabularis desktop application fork.

This fork keeps the SQL Server driver features and adds Windows/Kerberos and Microsoft Entra user authentication through Azure CLI, plus server-confirmed query cancellation. It provides schema introspection, query execution, full CRUD, DDL, trigger and stored-routine management, BLOB handling, database-user management, and visual execution plans through JSON-RPC 2.0 over stdio. It is written in Rust on Microsoft's [`mssql-tds`](https://github.com/microsoft/mssql-rust) protocol implementation (via [`mssql-tiberius-bridge`](https://crates.io/crates/mssql-tiberius-bridge)) with [`deadpool`](https://crates.io/crates/deadpool) connection pooling.

> **Requires Tabularis Extended 0.27.0 or later.** The connection extension
> uses the host's password-only field visibility API for Entra user sign-in.

**Discord** — [Join our Discord server](https://discord.com/invite/K2hmhfHRSt) and chat with the maintainers.

## Table of Contents

- [Features](#features)
- [Screenshots](#screenshots)
- [Connection Configuration](#connection-configuration)
- [Plugin Settings](#plugin-settings)
- [Query Execution Semantics](#query-execution-semantics)
- [Supported Data Types](#supported-data-types)
- [Database Users and Privileges](#database-users-and-privileges)
- [Visual EXPLAIN](#visual-explain)
- [Installation](#installation)
- [How It Works](#how-it-works)
- [Supported Operations](#supported-operations)
- [Known Limitations](#known-limitations)
- [Building from Source](#building-from-source)
- [Development](#development)
- [Contributing](#contributing)
- [Changelog](#changelog)
- [Credits](#credits)
- [License](#license)

## Features

- Microsoft's `mssql-tds` protocol implementation through `mssql-tiberius-bridge`, with `deadpool` connection pooling, session reset (`sp_reset_connection`), startup scripts, and pool lifecycle handling
- Schema, table, column, PK/FK, index, view, routine, and trigger introspection
- Query execution with pagination, CTE/DML classification, multiple result sets, and session-preserving batches
- Driver-owned SELECT/UPDATE/DELETE previews on hosts supporting optional [SQL templates](docs/query-templates.md), including SQL Server `TOP` syntax
- Accurate affected rows, including multi-statement DML and DML `OUTPUT`
- INSERT/UPDATE/DELETE with composite primary keys and safe `IDENTITY_INSERT` recovery
- Table/view/index/foreign-key DDL and safe `ALTER COLUMN` generation
- Trigger creation, editing, and removal
- SQL-authenticated database-user, login, role, and privilege management
- Procedure/function management, typed `OUT`/`INOUT` variables, and table-valued functions
- Static and runtime execution plans through `SHOWPLAN_XML` / `STATISTICS XML`, rendered in Tabularis's Visual EXPLAIN
- JavaScript-safe `BIGINT` extraction and broad SQL Server type handling
- Release workflow targets for Linux x86_64 and ARM64, macOS x86_64 and Apple Silicon, and Windows x86_64

## Screenshots

<table>
<tr>
<td><img src="https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/assets/screenshots/02-database-picker.png" alt="SQL Server listed in the database picker" width="400" /><br />SQL Server in the database picker</td>
<td><img src="https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/assets/screenshots/03-connection-form.png" alt="SQL Server connection configuration form" width="400" /><br />Connection configuration</td>
</tr>
<tr>
<td><img src="https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/assets/screenshots/06-schema-browser.png" alt="SQL Server schema browser with tables, views, routines, and triggers" width="400" /><br />Multi-schema browsing</td>
<td><img src="https://raw.githubusercontent.com/hei5enbug/tabularis-sqlserver-plus/main/assets/screenshots/08-visual-explain.png" alt="Visual EXPLAIN graph of a SQL Server SHOWPLAN" width="400" /><br />Visual EXPLAIN for SHOWPLAN</td>
</tr>
</table>

## Connection Configuration

| Parameter | Default | Required | Description |
| --- | --- | --- | --- |
| `host` | `localhost` | Yes unless using `connection_string` | SQL Server hostname or IP address |
| `port` | `1433` | No | TDS port |
| `database` | — | Yes unless using `connection_string` | Database the pool connects to |
| `username` | `sa` | Yes, unless integrated authentication is enabled or `connection_string` is used | SQL-authenticated login |
| `password` | — | If required by the server | Login password; redacted from connection errors |
| `ssl_mode` | `prefer` | No | `disable`, `prefer`, `require`, or `verify-full` |
| `ssl_ca` | — | No | Rejected; strict TLS uses the system trust store |
| `ssl_cert` / `ssl_key` | — | No | Rejected; client-certificate authentication is not supported |
| `connection_string` | — | No | `sqlserver://…` URL or ADO.NET/ODBC keyword syntax |
| `startup_script` | — | No | SQL run on every new pooled connection, such as session `SET` options |

### Connection strings

The connection string accepts either URL syntax:

```text
sqlserver://sa:p%40ssword@localhost:1433/master?Encrypt=true&TrustServerCertificate=true
```

or ADO.NET/ODBC keyword syntax. Keyword names are case-insensitive, common
aliases (`Data Source`, `Initial Catalog`, `UID`, and `PWD`) are accepted, and
braces preserve semicolons inside values:

```text
Server=tcp:localhost,1433;Database=master;User Id=sa;Password={p;assword};Encrypt=true;TrustServerCertificate=true;
```

### Windows/Kerberos integrated authentication

The connection modal's "Use Windows Authentication" checkbox is a
[UI extension](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md#3b-ui-extensions)
this plugin contributes to the host's `connection-modal.extra_fields` slot
(`ui/`) — there is no dedicated connection field for it. Checking it writes
`extra.integrated_auth = "true"` (the host's generic, plugin-opaque field map)
and, on a host implementing [TabularisDB/tabularis#780](https://github.com/TabularisDB/tabularis/pull/780)
(Tabularis `0.24.1-2` or later), hides the username/password inputs — both when
the box is ticked and when a saved connection with the flag is reopened. The same flag can be set directly via
`Integrated Security=True` / `Trusted_Connection=True` in `connection_string`
on any host, with or without the UI extension mechanism; either source
rejects a combined username or password.

It uses SSPI on Windows (no extra setup) and GSSAPI on Linux/macOS, loaded at
runtime via `dlopen`. The binary builds and starts without it, but connecting
fails at runtime if `libgssapi_krb5` (package `libgssapi-krb5-2` on
Debian/Ubuntu, `krb5-libs` on RHEL/Alpine) is missing, or without a valid
Kerberos ticket (`kinit`) and `/etc/krb5.conf`.

A connection string may be combined with discrete fields. Values explicitly
present in the string are authoritative, while discrete fields fill only
fields the string omits. Repeating the same value is allowed; contradictory
values are rejected with an error that identifies the discrete and
connection-string values instead of silently choosing one. Password values
are redacted in contradiction errors.

`Encrypt=false` maps to `ssl_mode=disable`; encrypted connections with
`TrustServerCertificate=true` map to `require`; encrypted connections that
verify the certificate map to `verify-full`. Custom CA and client-certificate
keywords are rejected under the same limitations as their discrete-field
counterparts.

### Entra user authentication through Azure CLI

Choose **Entra user (Azure CLI)** in the connection form. Keep the Username
field set to the expected Entra user principal name, then provide `tenant_id`
and `entra_object_id` in the extension fields. The optional `azure_cli_path`
must be an absolute path to the Azure CLI executable; leave it blank to use
`az` from the plugin process's `PATH`. Sign in with `az login --tenant
<tenant-id>` before connecting and complete any required MFA in Azure CLI.
The plugin runs Azure CLI without a shell and does not store the returned
access token.

Entra user connections require a public Azure SQL hostname ending in
`.database.windows.net`, `ssl_mode=verify-full`, and
`trust_server_certificate=false`. Private Link hostnames, service principals,
managed identities, password login, and custom certificate overrides are not
part of this authentication mode. Each new physical SQL connection obtains a
fresh token and validates its tenant, object ID, username, audience, and
expiration before opening the SQL session.

### TLS modes

The standard Tabularis `ssl_mode` values map onto the TDS encryption policy:

| Mode | Behaviour |
|------|-----------|
| `disable` | Encryption off |
| `prefer` (default) | Encrypted, server certificate accepted |
| `require` | Encryption required, server certificate accepted |
| `verify-full` | Encryption required, certificate and hostname verified against the **system trust store** |
| `verify-ca` | Rejected — use `verify-full` |

Custom CA files and client certificates are rejected explicitly; strict verification uses the system trust store.

## Plugin Settings

Tabularis sends these process-wide settings through `initialize` when the
plugin starts:

| Setting | Default | Effect |
|---------|---------|--------|
| `max_pool_size` | `10` | Maximum physical SQL Server sessions in each connection pool |
| `connect_timeout_seconds` | `15` | Maximum time to establish and authenticate a new session |
| `query_timeout_seconds` | `0` | Maximum query duration in seconds; `0` disables the timeout |
| `application_name` | `Tabularis` | TDS application name visible to DBAs in SQL Server session metadata |
| `trust_server_certificate` | `false` | Forces acceptance of a self-signed certificate without validation; use only for trusted development servers |
| `pool_idle_eviction_minutes` | `10` | Interval for removing pools with no checked-out sessions |

Malformed values produce a warning in the plugin log and fall back to the
default; unknown settings are ignored for forward compatibility. Settings are
snapshotted when a pool is created. Changing a setting takes effect on the next
connection after the plugin is restarted, not on live pooled sessions.

`trust_server_certificate` is an explicit escape hatch for self-signed
certificates in a verifying TLS mode. The `prefer` and `require` modes already
accept the server certificate as described above.

## Query Execution Semantics

Tabularis sends `limit` and `page` when result paging is enabled. The plugin
adds pagination only to one top-level `SELECT` or `VALUES` statement, including
a CTE whose final operation is a `SELECT`. DML, `EXEC`, `SELECT ... INTO`, and
multi-statement SQL run without pagination metadata. `execute_query` and every
statement in `execute_query_batch` use this same classification and execution
path.

For a paginated query the plugin requests `page_size + 1` rows. It normally
returns at most `page_size`, sets `has_more` when the lookahead row exists, and
sets `truncated` to the same value because that lookahead row was omitted. A
statement-wide safety ceiling retains at most 10,000 rows across all result
sets, even when `limit` is absent or larger; crossing it also sets `truncated`
and, for paginated queries, `has_more`. This bounds the plugin's single-line
JSON response instead of buffering arbitrary row counts in process memory.
`pagination.total_rows` remains `null`: normal page fetches never run a hidden
count query. The Tabularis **Count rows** action obtains a total separately by
running a `SELECT COUNT(*)` wrapper with pagination disabled. That count can
scan the full query result on a large table, but its cost is incurred only when
the host explicitly requests it.

SQL Server requires `ORDER BY` with `OFFSET ... FETCH`. If the query has no
top-level `ORDER BY`, the plugin injects `ORDER BY (SELECT NULL)` so paging
still works. This deliberately does **not** promise stable page boundaries:
rows can move between or repeat across pages because SQL Server may choose any
order. Add a deterministic `ORDER BY`, ideally ending in a unique key, whenever
page-to-page stability matters. An `ORDER BY` inside a subquery or `OVER(...)`
does not order the outer result and therefore does not prevent this injection.

When one SQL statement produces multiple result sets, the first occupies the
normal `columns` and `rows` fields and only real subsequent result sets appear
in `additional_results`. Batch-RPC statements remain separate batch entries.
The private `@@ROWCOUNT` result set used to recover SQL Server DML affected-row
counts is always removed and never appears in `additional_results`.

## Supported Data Types

All common SQL Server types are supported for column creation and value extraction, including exact/approximate numerics (`TINYINT` … `BIGINT`, `DECIMAL`, `MONEY`, `FLOAT`), strings (`CHAR`/`VARCHAR`/`NVARCHAR` incl. `MAX`, `TEXT`/`NTEXT`), binary (`BINARY`/`VARBINARY`/`IMAGE`), date/time (`DATE`, `TIME`, `DATETIME`, `DATETIME2`, `SMALLDATETIME`, `DATETIMEOFFSET`), `BIT`, `UNIQUEIDENTIFIER`, `XML`, `SQL_VARIANT`, `ROWVERSION`, `HIERARCHYID`, and spatial (`GEOGRAPHY`, `GEOMETRY`).

Generic DDL types emitted by Tabularis map to SQL Server-native spellings. In
particular, generic `TIMESTAMP` maps to `DATETIME2`; SQL Server's own
`TIMESTAMP` type remains a deprecated `ROWVERSION` synonym, not a date/time.

`BIGINT` values outside JavaScript's safe integer range and all exact
`DECIMAL`, `NUMERIC`, `MONEY`, and `SMALLMONEY` values are delivered as
strings so they round-trip without precision loss. `TIME(7)`, `DATETIME2(7)`,
and `DATETIMEOFFSET(7)` preserve 100-nanosecond precision; legacy `DATETIME`
is rendered at SQL Server's `.000`, `.003`, or `.007` second granularity.
`SQL_VARIANT` is emitted using the JSON representation of its contained value.

Binary values in query grids use the host BLOB wire shape
`BLOB:<bytes>:<mime>:<base64>` and the same shape is accepted by insert and
update. SQL Server CLR UDTs (`HIERARCHYID`, `GEOGRAPHY`, and `GEOMETRY`) are
losslessly displayed in that opaque binary shape because the TDS bridge does
not expose their type-specific value APIs. Protocol clients can write those
columns with the explicit raw row-edit shape
`{"value":"<SQL expression>","is_raw":true}`; ordinary values remain bound
parameters. `ROWVERSION` and its deprecated `TIMESTAMP` synonym are read-only,
server-generated eight-byte values: omit them on insert and do not update
them.

The pinned client can decode the newer native TDS `JSON` and `VECTOR` wire
types, and unit tests protect those paths. They are not advertised for column
creation while SQL Server 2022 is the plugin's live-test and release baseline;
JSON documents remain supported through `NVARCHAR(MAX)` on that server.

### Binary export and preview

`BINARY`, `VARBINARY` including `VARBINARY(MAX)`, and legacy `IMAGE` values can
be exported as raw files or previewed with MIME detection. `NULL` returns a
clear error instead of creating an empty file. `ROWVERSION` and its deprecated
`TIMESTAMP` synonym are excluded because they are server-generated concurrency
tokens, not user BLOB data.

BLOB previews are bounded by `max_blob_size` (100 MiB when the host does not
provide a value). SQL Server checks `DATALENGTH` before returning the bytes; an
oversized value produces an error with the actual and configured sizes and can
still be exported directly to a file without passing through base64 or a
JSON-RPC response.

## Database Users and Privileges

For this plugin a **database user** means a database-scoped SQL user mapped to
a server-scoped SQL login. In Tabularis's account display, `user` is the
principal in the connected database and the host-shaped field after `@` is the
mapped login name; it is not a network host. Windows, Azure AD, certificate,
contained, orphaned, and login-less users are intentionally not listed or
managed. Creating an account creates the login first and then its mapped user;
dropping it drops the user first and then the login. SQL Server's own ownership
checks are preserved, so a user that owns a schema or object must have that
ownership transferred before it can be dropped.

The host protocol's three MySQL-named scope shapes map to SQL Server as follows:

| Host wire scope | SQL Server scope |
|-----------------|------------------|
| `database = null`, `table = null` | Connected database |
| `database = schema`, `table = null` | Schema |
| `database = schema`, `table = object` | Object |

The privilege catalog follows the same mapping: its `global` entries are the
extra database-only permissions, `database` entries are permissions shared by
database and schema scopes, and `table` entries are object permissions.
Tabularis computes a requested checkbox diff, and the plugin checks the current
direct permissions again before applying only the required `GRANT` or `REVOKE`
statements in a transaction.

The parsed checkbox view contains direct grants only. The raw grants view also
labels role memberships, permissions inherited through roles, grants with
grant option, and direct `DENY` entries, so inherited rights are never shown as
if they were direct grants. Because SQL Server `DENY` overrides `GRANT`, the
plugin refuses to alter a denied permission; remove that `DENY` explicitly in
SQL before managing the permission through Tabularis.

## Visual EXPLAIN

The Rust process safely captures estimated `SHOWPLAN_XML` or runtime
`STATISTICS XML`, restores the session option, and returns the untouched XML as
raw format `sqlserver-showplan-xml`. It does not parse plan trees itself.
Tabularis v0.23.0 or later reads `explain/dist/index.iife.js` from the installed
plugin and registers that isolated TypeScript parser with
`@tabularis/explain`.

The parser source builds the `explain/dist/index.iife.js` bundle shipped with
each plugin archive. The fork keeps the parser package private and does not
publish it to npm. The complete parser and wire contract is in
[`docs/explain-architecture.md`](docs/explain-architecture.md).

## Installation

### Release archive

The fork publishes platform ZIPs and SHA-256 sidecars to the
[SQL Server Plus GitHub releases](https://github.com/hei5enbug/tabularis-sqlserver-plus/releases).
It does not publish to the upstream plugin registry or npm. Use a compatible
Tabularis Extended profile; the installer will only target the explicit
profile data directory supplied by you.

### Manual Installation

Once release assets are available, download the ZIP and its `.sha256` sidecar.
The installer validates the checksum and archive layout first. It performs a
dry run by default. Run it from a checkout of this fork:

Close Tabularis Extended and any Tabularis MCP clients using the profile before
installing or restoring a package. Keep them closed until the operation is
complete.

```bash
node scripts/install.mjs \
  --archive sqlserver-plus-linux-x64.zip \
  --sha256 sqlserver-plus-linux-x64.zip.sha256 \
  --profile-data-dir "/absolute/path/to/Tabularis Extended data"
```

Review the printed target. To apply the same verified archive, add `--apply`.
The destination is `<profile-data-dir>/plugins/drivers/sqlserver-plus`. The
installer stages the complete plugin beside the destination, renames the
existing `sqlserver-plus` directory to a backup, activates the new directory,
and restores the old directory if activation fails. After a successful update,
the installer prints and retains the backup path for an explicit rollback.
Keep it until the new plugin has been verified. To restore it, close Tabularis
Extended and MCP clients, move the current
`<profile-data-dir>/plugins/drivers/sqlserver-plus` directory aside, then move
the printed backup directory back to that exact target. It does not change
other driver packages or profile files. Use the Extended profile's actual data
directory for your operating system; the installer does not infer a profile
or fall back to a different Tabularis installation.

For a macOS or Linux rollback, use the exact backup path printed by the
installer and keep the current directory aside until the restored version is
verified:

```bash
TARGET="/absolute/path/to/Tabularis Extended data/plugins/drivers/sqlserver-plus"
BACKUP="/absolute/path/printed/by/the/installer"
mv "$TARGET" "${TARGET}.before-rollback"
mv "$BACKUP" "$TARGET"
```

The ZIP contains the native executable, `.tabularium`, UI extension, EXPLAIN
parser, README, Apache license, and the upstream TDS bridge's MIT license.
SHA-256 checks accidental corruption against the downloaded sidecar; it is not
a digital signature.

## How It Works

The plugin is a standalone Rust binary that communicates with Tabularis through
**newline-delimited JSON-RPC 2.0 over stdio**:

1. Tabularis starts `sqlserver-plugin` as a child process and calls
   `initialize` with the manifest-backed process settings.
2. The plugin normalizes the discrete connection fields or connection string,
   then reuses a matching in-process `deadpool` pool.
3. New sessions connect through Microsoft's `mssql-tds` implementation, run
   the optional startup script, and are reset with `sp_reset_connection`
   before reuse.
4. Requests and responses stay on stdin and stdout; diagnostics go to stderr.
   The plugin opens no listening port and keeps no persistent state.

Pool identity includes every connection and TLS field that changes session
behaviour, plus the startup script. The idle-eviction task removes unused
pools at the configured interval, and the courtesy `shutdown` RPC drains all
remaining pools.

## Supported Operations

| Method group | Operations |
| --- | --- |
| Lifecycle and connection | `initialize`, `shutdown`, `ping`, `test_connection`, database discovery |
| Schema metadata | Schemas, tables, columns, keys, indexes, views, routines, triggers, snapshots, and batch metadata |
| Query execution | Paginated queries, session-preserving batches, affected rows, and Visual EXPLAIN |
| Row editing | Insert, update, and delete with composite primary keys and type-aware values |
| DDL | Table, column, index, foreign-key, view, routine, and trigger generation or lifecycle operations |
| BLOBs | Raw file export and bounded MIME-sniffed data-URL preview |
| Security | SQL login and mapped database-user lifecycle, password changes, privilege catalog, grants, roles, and inherited rights |

## Known Limitations

- SQL authentication, Windows/Kerberos integrated authentication (`integrated_auth`), and Entra user authentication through Azure CLI are supported. Entra is limited to public Azure SQL hostnames and the explicit TLS checks above.
- A cancelled query returns JSON-RPC error `-32800` with `server_termination_confirmed: true` only after the SQL Server TDS ATTENTION acknowledgement is received. If the connection ends before that acknowledgement, the query result keeps its original driver status and the uncertain connection is discarded. The plugin does not issue `KILL` statements or replay queries.
- Primary-key membership changes are disabled: the single-column alteration API cannot safely preserve composite PKs and referencing foreign keys.
- Custom CA files are rejected explicitly; strict verification uses the system trust store.
- SQL Server has indexed views, not materialized views. Indexed views are maintained synchronously and have no refresh operation, so `get_materialized_views`, `get_materialized_view_columns`, `get_materialized_view_definition`, and `refresh_materialized_view` deliberately return `-32601` rather than pretending the features are equivalent.
- All host RPC methods outside those four materialized-view operations are implemented, including the courtesy `shutdown` method even though the current host terminates the process directly. Truly unknown JSON-RPC methods return `-32601` with an error naming both the method and the SQL Server plugin.

## Building from Source

### Prerequisites

- Rust (stable, see `rust-toolchain.toml`)
- Node.js 24.21.0 and npm 11.6.2 for the connection extension; pnpm 11.23.0 for the bundled Visual EXPLAIN parser
- [`just`](https://github.com/casey/just) (optional, wraps the common build and test commands)

### Build

```bash
just build      # debug build
just release    # release build (what the GitHub Actions workflow ships)
```

Build commands use `cargo --locked`, `npm ci`, and `pnpm install
--frozen-lockfile` and always build both JavaScript bundles. Run
`just manifest-check` after the build to verify the manifest's fork identity,
version, host floor, and referenced bundle files.

### Prepare an archive

```bash
just test-packaging
node scripts/package.mjs \
  --binary target/release/sqlserver-plugin \
  --platform linux-x64 \
  --output dist/sqlserver-plus-linux-x64.zip
```

Windows packages use `target/release/sqlserver-plugin.exe` and
`--platform win-x64`. Packaging writes a sorted ZIP with a fixed timestamp and
file modes, plus a SHA-256 sidecar. See [`docs/build-release.md`](docs/build-release.md)
for the platform matrix, installer contract, and release checks. Local install
commands do not write into a real profile.

## Development

### Testing the Plugin

Unit tests need no database:

```bash
just test
just test-explain
just test-containers
just test-packaging
just lint
just fmt
```

You can drive the plugin directly over stdio:

```bash
echo '{"jsonrpc":"2.0","method":"get_create_table_sql","params":{"table_name":"users","schema":"dbo","columns":[{"name":"id","data_type":"INT","is_nullable":false,"is_pk":true,"is_auto_increment":true,"default_value":null}]},"id":1}' \
  | ./target/debug/sqlserver-plugin
```

or use the interactive REPL:

```bash
just repl
```

### SQL Server integration tests

```bash
just test-containers
```

The Testcontainers suite starts its own digest-pinned SQL Server container with
a generated password and random port, then removes it after the tests. It does
not use fixed ports, seeded databases, or existing database credentials. See
[`tests/containers/README.md`](tests/containers/README.md) for the exact
coverage and Docker requirements.

## Contributing

Pull-request titles must follow [Conventional Commits](https://www.conventionalcommits.org/):
`type: subject`, `type(scope): subject`, or `type!: subject` for a breaking
change. Add a `BREAKING CHANGE:` footer to the PR description when the title
cannot communicate the full impact.

Every PR must have exactly one `prerelease:alpha`, `prerelease:beta`,
`prerelease:rc`, or `prerelease:stable` label. CI uses the title and that label
to suggest the next version and release channel; there is no default channel,
so a missing or ambiguous label fails the version-suggestion check.

| PR title type | Version impact |
| --- | --- |
| `feat` | minor |
| `fix`, `refactor`, `perf` | patch |
| `docs`, `style`, `chore`, `test`, `ci`, `build` | none |
| any type with `!` or a `BREAKING CHANGE:` footer | major |

### Releasing

A release uses a `plus-v<version>` tag. The Release workflow builds platform
ZIPs and SHA-256 sidecars, then publishes those assets and `.tabularium` to a
GitHub release in `hei5enbug/tabularis-sqlserver-plus`. It does not publish to
npm or the upstream registry. `.tabularium`, `Cargo.toml`, `ui/package.json`,
and `explain/package.json` must all use the exact tag version; the workflow
checks the repository and version before building.

Before opening a PR, run:

```bash
just fmt
just lint
just test
npx markdownlint-cli "**/*.md"
```

## [Changelog](./CHANGELOG.md)

## Credits

The SQL Server driver implementation was contributed by [Fabio Malpezzi](https://github.com/FabioMalpezzi), originally developed as a built-in Tabularis driver and adapted here to the plugin architecture.

## License

Apache-2.0 — see [LICENSE](./LICENSE).
