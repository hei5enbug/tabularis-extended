# Tabularis Extended 0.32.0+extended.1

Tabularis Extended is a personal fork of Tabularis 0.32.0. It runs beside the official app and gives its MCP
server the database features that coding agents expect from a database IDE.

## Coexistence with the official app

The fork never reads or writes the official app's profile, keychain entries, or deep links.

| Item | Official Tabularis | Tabularis Extended |
|---|---|---|
| App name | Tabularis | Tabularis Extended |
| Bundle identifier | official identifier | `io.github.hei5enbug.tabularis.extended` |
| Profile directory and keychain service | `tabularis` | `tabularis-extended` |
| Deep-link scheme | `tabularis://` | `tabularis-extended://` |
| MCP client config key | `tabularis` | `tabularis-extended` |
| Updates | official feed | disabled |

## Moving from the official app

1. Install the PostgreSQL Plus and SQL Server Plus packages into the fork's plugin directory.
2. Start Tabularis Extended. On first run it offers to import the official profile.
3. The import backs up the official profile, then copies connections, groups, saved queries, notebooks, themes,
   settings, and plugin packages. Connection IDs stay the same.
4. Saved passwords are copied from the official keychain service to the fork's service and read back to verify.
   macOS asks to allow access to each official keychain item.
5. Restart when the result screen asks. After a verified Plus package is installed, `postgresql` connections move
   to `postgresql-plus`, and `sqlserver` and `sqlserver-entra` connections move to `sqlserver-plus`.
6. On each start, the fork adds every verified Plus driver that a connection uses to `activeExternalDrivers`, so
   the imported driver list cannot leave those drivers stopped.

The import refuses to run when the fork profile already holds data. The backup encryption password of the backup
feature is not copied.

## Driver packages

| Package | ID | Version | Notes |
|---|---|---|---|
| PostgreSQL Plus | `postgresql-plus` | `1.0.0-rc.6+plus.1` | Service protocol; reads run in read-only transactions |
| SQL Server Plus | `sqlserver-plus` | `1.0.0-beta.3+plus.1` | Service protocol; Entra users through Azure CLI |
| Cosmos DB for NoSQL | `cosmos-nosql` | `0.1.0` | Documents, paged queries, and connection metadata |
| Spatial | `spatial` | `0.1.0` | UI plugin for PostGIS maps; it has no driver process |

Plugin UIs run in the upstream sandbox frame. Spatial and Cosmos DB for NoSQL run in the main window instead,
because they need host hooks, assets, and workers that the sandbox does not offer. Sandboxed UIs built for the
`0.4.0` guest API also load, and they can hide the password field when a connection needs no password.

Spatial finds the schema that holds the PostGIS extension, so PostGIS installed outside `public` also works.

SQL Server Plus acquires Entra tokens itself with `az` for each new physical connection. Run `az login` outside the
app. The host never handles Entra tokens for this driver.

A SQL Server Plus connection without `auth_mode` uses SQL password authentication, or integrated authentication
when `integrated_auth` is on. The host then sends the saved keychain password, the same way it does for an
explicit `sql_password`.

## MCP server

`tabularis-extended --mcp` serves 58 tools: the five legacy tools and 53 service tools. The service tools cover
connections, the catalog, queries and stored results, single-row and document writes, sessions, jobs, spatial
features and saved maps, Entra sign-in, and server activity.

- `query_execute` returns a job ID right away. A job that finishes within `wait_ms` returns its first page. A job
  may run for up to one hour, and time spent waiting for approval in the app does not count against it.
- One stored result keeps at most 10,000 rows. A longer read is cut there and reported with `truncated: true` and
  `truncation_reason: row_limit` (`byte_limit` for the size cap), and the app shows the cut in the result header.
  A write whose result is longer fails with `RESOURCE_LIMIT`, and the message names the limit.
- `result_get` pages a stored result without running the SQL again. Each reply stays within 64 KiB.
- Database errors come back with the code `DATABASE_ERROR` and keep the database's own message.
- `job_cancel` reports `cancelled` only when the driver confirms it. Otherwise it reports `CANCEL_NOT_CONFIRMED`.
- `job_list` returns running jobs and the query history. The history keeps SQL text and status, never rows or
  credentials.
- Catalog tools list databases, schemas, and objects such as tables, views, materialized views, foreign tables,
  routines, aggregates, triggers, sequences, synonyms, types, collations, operators, rules, and security policies,
  and accept `offset` for long lists. `catalog_object_kinds` tells which kinds a connection's driver offers.
- `catalog_describe` returns columns, keys, foreign keys, indexes, check constraints, the definition, and
  kind-specific properties. An object that does not exist fails with `INVALID_ARGUMENT`.
- A trigger can be picked by its owning table with `owner_table`. Sequence values outside JavaScript's safe integer
  range come back as text.
- `query_explain` returns the estimated plan of one statement and never runs it. PostgreSQL Plus runs
  `EXPLAIN (FORMAT JSON)` in a read-only transaction. SQL Server Plus uses `SHOWPLAN_XML` on a connection that is
  thrown away afterwards. The reply sums up the total cost, estimated rows, operators, full scans, warnings, and
  missing indexes, and carries the raw plan when it is small enough.
- `server_activity` lists the queries running on the server, from any client. `server_cancel` stops one of them by
  its process id. On PostgreSQL it cancels only the query and the session stays. On SQL Server it runs `KILL`, which
  ends the session and rolls back its open transaction.
- A driver process starts when a connection that uses it is first called, not when the server starts. It stays
  running by default. To stop an idle one, set `pluginIdleStopSeconds` in `config.json`, or
  `plugins.<id>.idleStopSeconds` for one driver, which overrides the global value. `0` turns it off. After that
  many seconds without a call, the driver process is stopped, and the next call starts it again and repeats its
  `initialize` request. A process with a call in progress or an open session is never stopped. Once `run_query`
  sends a write outside a service session, such as with approval mode `all`, that driver is never stopped until the
  MCP server exits, because the write may have left a transaction open. The desktop app and
  `tabularis web` start their drivers at launch and never stop them for being idle.
- `run_query` cancels a read on the database server when `timeout_ms` passes, keeps the database's error text, and
  pages UNION queries.
- Calls whose params carry a `_meta` object, as Claude Code sends them, are accepted.

## Write policy

- Read-only mode (`mcp_readonly_default`, `mcp_readonly_connections`) and the approval mode (`mcp_approval_mode`)
  apply to both `run_query` and the service tools.
- A statement counts as a read only when the classifier proves it is one read. `SELECT … INTO`, `EXEC`, `CALL`,
  and a write keyword after a read without a semicolon count as writes. A keyword right after AS is read as an
  alias. When read-only mode refuses a query that could not be proven a read, the message suggests quoting
  identifiers that are SQL keywords.
- Two kinds of read batch also count as reads. The `query_execute` description lists the exact rules.
  - PostgreSQL Plus: one read wrapped in BEGIN, SET [LOCAL] of a listed setting such as `statement_timeout`, and
    COMMIT or ROLLBACK, outside a session.
  - SQL Server Plus: a T-SQL batch of reads that may use DECLARE, SET, IF, WHILE, TRY…CATCH, THROW, PRINT, and
    WAITFOR DELAY.
- Apart from that PostgreSQL Plus wrapper, text that begins with transaction control, such as BEGIN TRAN, COMMIT,
  ROLLBACK, or SAVE TRAN, fails with `UNSUPPORTED_OPERATION`.
- A write that must run as one transaction uses a session:
  - `session_open` returns a `session_id`.
  - Each `query_execute` call passes that `session_id`.
  - `session_commit` or `session_rollback` ends the session.
- Inside a write session, a proven read needs no `write: true`. SQL Server Plus keeps the session's temporary
  tables and `SET` options and reports affected rows. When user SQL ends or replaces the session's transaction,
  SQL Server Plus closes the session without committing and says so in the reply.
- A `SELECT` that calls a function that changes server or session state, such as `pg_cancel_backend`,
  `set_config`, `nextval`, advisory locks, or `dblink`, counts as a write. A statement with a MySQL executable
  comment (`/*! … */`) never counts as a read.
- Service writes need `write: true`. Without it a non-read fails with `WRITE_INTENT_REQUIRED`. `server_cancel` is
  always a write.
- When a driver gives no database-level read-only guarantee, reads run on the classifier's result and report
  `readonly_guarantee: classification_only`.
- After an approval, the host checks read-only mode and the connection revision again before it runs the request.
- A refused write names the setting or check that refused it, such as read-only mode or a denied approval.
- `connection_create` and `connection_update` reject secrets with `SECRET_NOT_ACCEPTED` and always wait for GUI
  approval. `credential_import` and `connection_delete` are not available through MCP.

## Following upstream releases

`node scripts/upstream-sync.mjs status` shows the newest upstream Tabularis release and the files a merge would
conflict on, without touching the work tree. `sync` merges it on a new branch and `finish` sets the fork version.
`UPSTREAM_SYNC.md` describes the steps, and `scripts/check-fork-identity.mjs` checks that the fork keeps its own
name, identifiers, and version.

## Verification status

### 0.32.0+extended.1

Checks on 2026-11-15, macOS arm64:

- GitHub CI passed on commit `beccc3ee` of the main branch: CI, Format, Packages, Design & Accessibility, MySQL
  and PostgreSQL integration, and Extended checks.
- The PostgreSQL Plus and SQL Server Plus extension CI passed on commit `f7782149`.
- Host: 3,243 Rust library tests and 8,846 frontend tests passed.
- A live test with the real PostgreSQL Plus driver and PostgreSQL 17 read 10,001 rows, started the driver on the
  first call, stopped it when idle, and read the same rows again from a new process.
- The installed app ran with `--mcp` in a separate profile against a local PostgreSQL. No driver process ran at
  start, one ran after the first `query_execute`, none ran after the idle period, and the second query returned the
  same rows from a new process.

Not checked: the Cosmos DB driver restarting after an idle stop against a real Cosmos DB account.

### 0.27.0+extended.1

Automated checks ran on macOS arm64 with deterministic fake drivers and fixtures. The Spatial and Azure suites and
the frontend tests ran on 2026-10-21; the others ran on 2026-10-22.

- Host: 2,181 Rust library tests passed in a full run, and 5,380 frontend tests. In an earlier full parallel run the
  Spatial test `명시_export는_원본32mib이하_snapshot을_전체32mib이상_geojson으로_스트리밍한다` failed under load and
  passed on its own, so it is likely a timeout under load.
- Contracts: 456 Node tests and 284 Rust tests.
- PostgreSQL Plus: 421 unit tests. SQL Server Plus: 334 unit tests; its 3 conformance tests and 33 offline harness
  tests last ran on 2026-10-21.
- Upstream sync scripts: 14 tests.
- Spatial: 68 Rust, 124 UI, and 128 install tests. Azure: 26, 646, 14, and 126 tests in its suites.
- A locked release build produced the macOS app bundle.

Checks against real systems on 2026-10-22, macOS arm64. Saved connections were used read-only; cancellation ran only
on local Docker databases:

| Check | Scope | Result |
|---|---|---|
| Official profile import | 14 connections | Imported with keychain passwords; 6 moved to `postgresql-plus`, 8 to `sqlserver-plus` |
| `connection_test` and `SELECT 1` | 7 nonproduction connections: 3 PostgreSQL Plus, 4 SQL Server Plus | All succeeded |
| Entra login through Azure CLI | The 4 SQL Server Plus connections above | Succeeded |
| `readonly_guarantee` | Same 7 connections | `read_only_transaction` on PostgreSQL Plus, `classification_only` on SQL Server Plus |
| Routine and trigger catalog | Same 7 connections | PostgreSQL counts match `pg_proc` and `pg_trigger` in `public`, without extension functions |
| Server-side cancellation | Local Docker PostgreSQL 16 and SQL Server 2022 | `job_cancel` returned `cancelled`, and the server no longer listed the query |
| `query_explain` | Same Docker databases, 200,000-row table | Plans for `SELECT` and `DELETE` with `executed: false`; the row count did not change; two statements and `EXPLAIN ANALYZE` were refused |
| `server_activity` and `server_cancel` | Same Docker databases, a query from another client | The query was listed; cancel needed `write: true`, waited for approval in the default mode, was refused in read-only mode, and with approval off stopped the query (PostgreSQL) or killed its session (SQL Server) |

The production connections were not used. Not yet verified: Windows bundles, install and rollback of the packaged
plugins through the plugin manager, and any write path on a real database.
