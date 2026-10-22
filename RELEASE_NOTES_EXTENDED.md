# Tabularis Extended 0.27.0+extended.1

Tabularis Extended is a personal fork of Tabularis 0.27.0. It runs beside the official app and gives its MCP
server the database features of the DataGrip MCP server.

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

SQL Server Plus acquires Entra tokens itself with `az` for each new physical connection. Run `az login` outside the
app. The host never handles Entra tokens for this driver.

A SQL Server Plus connection without `auth_mode` uses SQL password authentication, or integrated authentication
when `integrated_auth` is on. The host then sends the saved keychain password, the same way it does for an
explicit `sql_password`.

## MCP server

`tabularis-extended --mcp` serves 54 tools: the five legacy tools with unchanged output, 47 service tools, and
`table_preview` and `job_list`.

- `query_execute` returns a job ID right away. A job that finishes within `wait_ms` returns its first page.
- `result_get` pages a stored result of up to 10,000 rows without running the SQL again. Each reply stays within
  64 KiB.
- `job_cancel` reports `cancelled` only when the driver confirms it. Otherwise it reports `CANCEL_NOT_CONFIRMED`.
- `job_list` returns running jobs and the query history. The history keeps SQL text and status, never rows or
  credentials.
- Catalog tools list databases, schemas, tables, views, materialized views, routines, and triggers, and accept
  `offset` for long lists.

## Write policy

- Read-only mode (`mcp_readonly_default`, `mcp_readonly_connections`) and the approval mode (`mcp_approval_mode`)
  apply to both `run_query` and the service tools.
- A statement counts as a read only when the classifier proves it is one read. `SELECT … INTO`, `EXEC`, `CALL`,
  several statements, and a write keyword after a read without a semicolon count as writes.
- Service writes need `write: true`. Without it a non-read fails with `WRITE_INTENT_REQUIRED`.
- When a driver gives no database-level read-only guarantee, reads run on the classifier's result and report
  `readonly_guarantee: classification_only`.
- After an approval, the host checks read-only mode and the connection revision again before it runs the request.
- `connection_create` and `connection_update` reject secrets with `SECRET_NOT_ACCEPTED` and always wait for GUI
  approval. `credential_import` and `connection_delete` are not available through MCP.

## Verification status

Automated checks ran on macOS arm64 with deterministic fake drivers and fixtures. The host Rust library tests ran
on 2026-10-22; the other suites ran on 2026-10-21.

- Host: 2,137 Rust library tests and 5,380 frontend tests. In one full parallel run, the Spatial test
  `명시_export는_원본32mib이하_snapshot을_전체32mib이상_geojson으로_스트리밍한다` failed once. It passed in three
  runs on its own, so the failure is likely a timeout under load.
- Contracts: 447 Node tests and 268 Rust tests.
- PostgreSQL Plus: 415 tests. SQL Server Plus: 325 driver tests, 3 conformance tests, and 33 offline harness tests.
- Spatial: 68 Rust, 124 UI, and 128 install tests. Azure: 26, 646, 14, and 126 tests in its suites.
- A locked release build produced the macOS app bundle.

Checks against real systems on 2026-10-22, macOS arm64, read-only:

| Check | Scope | Result |
|---|---|---|
| Official profile import | 14 connections | Imported with keychain passwords; 6 moved to `postgresql-plus`, 8 to `sqlserver-plus` |
| `connection_test` and `SELECT 1` | 7 nonproduction connections: 3 PostgreSQL Plus, 4 SQL Server Plus | All succeeded |
| Entra login through Azure CLI | The 4 SQL Server Plus connections above | Succeeded |
| `readonly_guarantee` | Same 7 connections | `read_only_transaction` on PostgreSQL Plus, `classification_only` on SQL Server Plus |
| Routine and trigger catalog | Same 7 connections | PostgreSQL counts match `pg_proc` and `pg_trigger` in `public`, without extension functions |
| Server-side cancellation | Local Docker PostgreSQL 16 and SQL Server 2022 | `job_cancel` returned `cancelled`, and the server no longer listed the query |

The production connections were not used. Not yet verified: Windows bundles, install and rollback of the packaged
plugins through the plugin manager, and any write path on a real database.
