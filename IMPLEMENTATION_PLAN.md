# Tabularis Extended implementation plan

This document defines future implementation work. This commit changes documentation only.
The approved scope is the existing working setup plus saved-result paging, query cancellation, and query history.
The coordinator owns this plan and the cross-repository contracts.

## Intent and scope

Build and maintain Tabularis Extended as a reproducible fork that can use the existing database connections
and expose the three additional MCP capabilities without waiting for an upstream pull request.

| Item | Decision |
|---|---|
| Host repository | [hei5enbug/tabularis-extended](https://github.com/hei5enbug/tabularis-extended) |
| Driver repository | [hei5enbug/tabularis-sqlserver-plus](https://github.com/hei5enbug/tabularis-sqlserver-plus) |
| Host code baseline | `313ef093408a15c38f1df9342c1f779aa3e7d27c`, upstream `0.27.0` |
| Driver code baseline | `885ea261edd424078a59aaa584c2cfc25f2969ae` |
| Current installed host | `0.26.0+local.1`, based on `5d7e27408907c0faafc8cc6b940725f4a1258f42` |
| Companion implementation | [SQL Server Plus plan](https://github.com/hei5enbug/tabularis-sqlserver-plus/blob/main/IMPLEMENTATION_PLAN.md) |
| Document language | English, following `.rules/general.md` |

In scope:

- Recover the behavior of the installed host patch and the locally extended SQL Server driver as maintained source.
- Preserve connection IDs, names, selected databases, authentication, TLS, keychain storage, and plugin settings.
- Add MCP tools for asynchronous execution, saved-result pages, cancellation, execution status, and history.
- Preserve the five existing MCP tools and their JSON/TOON output contracts.
- Give the fork a distinct application identity and a repeatable build, installation, update, and rollback path.
- Preserve installed PostgreSQL, SQL Server, Spatial, and Cosmos plugin compatibility at their verified level.
- Prevent an old connection editor from overwriting a newer TLS or authentication setting.

Non-goals:

- Reproduce DataGrip's IDE, file editing, refactoring, build, connection-management, or all catalog tools.
- Add new database write operations, service-principal authentication, or automatic privileged access.
- Port the entire archived spatial service host, add MCP map control, or complete Cosmos CRUD/authentication parity.
- Add unbounded result export, server cursors, or retrieval of rows discarded by a driver limit in the first release.
- Publish connection files, passwords, tokens, tenant/user/object identifiers, private endpoints, or live query logs.

The three new capabilities initially target PostgreSQL and SQL Server Plus. Other drivers retain the existing
MCP path. Their managed-query support requires a separate verified capability adapter.
All numerical limits and fork identities below are proposed implementation decisions, not current capabilities.

## Evidence and behavior to preserve

The installation inventory and prior read-only checks establish this baseline. They are not new tests of the fork.

| Surface | Observed state | Implementation consequence |
|---|---|---|
| Saved connections | 14 connections: six PostgreSQL, six Entra SQL, two SQL-password connections | Preserve all IDs and routing; repeat `SELECT 1` after migration |
| Live verification | Every connection eventually returned `1`; metadata checks covered representative PostgreSQL and SQL connections | This proves basic connectivity, not all workloads or new cancellation/paging behavior |
| PostgreSQL | Official plugin `1.0.0-rc.6`; verified TLS and existing password credentials | Keep the official plugin and reuse its request-cancellation protocol |
| SQL Server | Official `1.0.0-beta.3` plus a local `sqlserver-entra` package at `1.0.0-beta.3+local.entra.1` | Port the authentication delta through the companion plan; do not copy an old source tree over current main |
| Entra authentication | Azure CLI token acquisition for each new physical connection, with identity/resource/expiry checks | Preserve the same account and tenant; never persist access or refresh tokens in the fork |
| TLS | PostgreSQL `verify-full`; SQL Server `verify-full` or normalized `verify_identity` | Preserve certificate and hostname validation; do not substitute trust-all certificates |
| Credential storage | Keychain-backed connections work; manual edits also demonstrated inline-password storage | Import credentials into the fork keychain without publishing or logging them |
| Spatial | Installed UI plugin `0.1.0`, runtime floor `0.26.0` | Preserve the basic PostGIS table-map path, UI slots, relative assets, and host plugin API |
| Cosmos | Installed `cosmos-nosql` `0.1.0`, runtime floor `0.26.0` | Preserve its currently supported connection, metadata, and read-only query paths |
| EXPLAIN | PostgreSQL plans and SQL Server `sqlserver-showplan-xml` parser/bundle | Preserve parser registration, module paths, and raw result shape; no live EXPLAIN ANALYZE tests |

The Spatial baseline has table-map limits of 1,000 rows, 100,000 coordinates, and 8 MiB of GeoJSON.
Saved maps, result-snapshot maps, and MCP map control require a different service host and are not established
capabilities of the installed `0.26.0+local.1` build. Cosmos continuation, document CRUD, full authentication
refresh, and GUI/CLI/MCP parity likewise remain unverified in the archived validation records.

### Source provenance

- The installed host patch is recorded in
  [local-host pins](https://github.com/hei5enbug/tabularis-spatial/blob/6ab07f389990ee241ae67fba1cdc1d2b110a9738/integration/local-host/pins.json)
  and [its patch](https://github.com/hei5enbug/tabularis-spatial/blob/6ab07f389990ee241ae67fba1cdc1d2b110a9738/integration/local-host/plugin-drivers-only.patch).
  Its SHA-256 is `607a39712d162aeaa3983673594bc0ab2362696d60069e435ce13cf1dc3762ee`.
- The larger archived host patch targets `0.26.1-spatial.1`, not either current baseline.
  [Integration pins](https://github.com/hei5enbug/tabularis-spatial/blob/6ab07f389990ee241ae67fba1cdc1d2b110a9738/integration/upstreams.json)
  identify its source as `04f85fce003383a8f489ad7d7ac05626583e2361` and its patch SHA-256 as
  `06a4847ff21a79a31a73d9121d8fe745969ca7046de34b22410ed73269e63b02`.
  `services/results.rs`, `services/jobs.rs`, and `services/runtime.rs` in that patch are reuse candidates.
  They contain real result storage and job code, but SQL output is still bounded and native continuation is
  document-driver-specific. Extract only the pieces required by this plan.
- Existing plugin behavior is described in the
  [Spatial usage guide](https://github.com/hei5enbug/tabularis-spatial/blob/6ab07f389990ee241ae67fba1cdc1d2b110a9738/docs/usage.md)
  and [Cosmos verification record](https://github.com/hei5enbug/tabularis-azure/blob/1f7e3f5367b065df077383c313a48fadeaf1f5bf/docs/verification.md).
- The installed custom SQL source archive and deployment receipt are local recovery inputs.
  Locate `sqlserver-entra-source.tar.gz` and `deployment.json` under the platform application-support sibling
  `tabularis-local-deployments/connection-repair-*/` directory. Verify the archive's baseline and the receipt's
  installed-binary checksum before extracting the five source deltas listed in the driver plan.
  Port their source changes through the driver repository. Do not commit their private configuration backups
  or machine-specific repair scripts. Preserve the existing 239-test result only as historical evidence.

## Implementation strategy

### Recover the host behavior on current main

Port the local patch semantically rather than applying its old version hunks wholesale:

1. Keep only MySQL and SQLite registered as built-in drivers in GUI and standalone MCP startup.
   PostgreSQL uses the official `postgresql` plugin. Retain PostgreSQL dialect and geometry helpers.
2. Make foreign imports resolve PostgreSQL to `postgresql` and SQL Server to `sqlserver`.
   Keep `mssql` as a SQL dialect name, not a plugin ID. A plain import must not infer Entra authentication.
3. Retain upstream's existing plugin discovery, lazy MCP registry reload, and driver-migration UI.
   Update built-in fallback lists and importer/parser fixtures consistently.
4. Restore `--list-drivers` as a manifest-only diagnostic. It must not open database connections, resolve
   credentials, migrate connection files, or write application configuration.

Primary surfaces are `src-tauri/src/{lib.rs,cli.rs,mcp/mod.rs}`, `src-tauri/src/drivers/registry.rs`,
`src-tauri/src/connection_import/driver_map.rs`, `src/hooks/useDrivers.ts`, and `src/utils/connections.ts`.
Preserve newer upstream fixes, including read-only/EXPLAIN handling, routine review, plugin loading,
data-grid behavior, and connection migration. Do not downgrade the source version to `0.26.0`.

### Application identity and profile migration

Use display name `Tabularis Extended`, executable name `tabularis-extended`, and bundle identifier
`io.github.hei5enbug.tabularis.extended`. Use `tabularis-extended` for the fork's platform profile directory
and keychain service. Keep the original profile and application available for rollback.

The first-run import is explicit and local. Read the existing profile through its configured storage-location
resolver, take a private backup, then atomically create the fork profile. Preserve connection IDs, names,
database selections, groups, tags, saved queries, notebooks, themes, and settings. Copy platform-specific plugin
packages to the fork's plugin directory rather than copying binaries through a synced configuration folder.
Transfer saved and inline passwords directly into the fork keychain; remove those values from destination JSON
only after keychain verification succeeds. Preserve source credentials and report unavailable credentials for
manual entry. Never extract secrets into command-line arguments, reports, fixtures, or repository files.

Keep PostgreSQL connections on `postgresql`. In the imported fork profile, migrate existing SQL Server and
`sqlserver-entra` connections to `sqlserver-plus` after the companion package is installed and verified.
Preserve the six Entra configurations and the two SQL-password configurations as distinct authentication modes.
Keep original profile IDs and original plugin packages intact. Update the MCP client command to the verified
fork executable only after the new profile passes its acceptance checks; do not edit unrelated MCP servers.

Implement this in `src-tauri/src/{paths.rs,storage_location.rs,persistence.rs,keychain_utils.rs,commands.rs}`,
the plugin installer, application metadata, and the first-run/import UI. Handle custom storage locations and
existing destination profiles explicitly; never overwrite a nonempty destination profile silently.

### Connection edits must preserve current settings

Use the existing `fs2` dependency for one profile-level connection-file lock, atomic replacement, and a
per-connection revision. A loaded editor receives a revision and supplies it when saving. Under the lock,
reload the current file and reject a stale revision before credential or file mutation. Return a conflict
that lets the user reload the connection; do not silently merge contradictory known-field values.
Preserve unknown fields and all other connections. Apply this protocol to every connection-file writer,
including database-selection reconciliation and imports, not only the modal's save button.
Test file/keychain failures together: a failed secret update leaves the file untouched, and a failed file commit
restores the prior secret or reports an explicit recovery requirement. Never report a partially saved edit as success.

A password-only edit must preserve `ssl_mode`, `driver`, database selection, and `extra` unless those fields
were explicitly edited in the same current revision. Invalidate GUI caches after a successful save and refresh
on an external file change. This addresses the observed case where an older editor restored an empty TLS mode.

Extend the `connection-modal.extra_fields` context in `packages/plugin-api/src/slots.ts` with
`setPasswordFieldHidden(boolean)`. It hides and clears only the password field; the username remains visible.
Keep `setCredentialFieldsHidden(boolean)` backward compatible for existing plugins. Reset both flags when the
driver changes and preserve plugin `extra` fields through load, test, and save. SQL Server Plus owns its auth UI.

### Managed-query contract

This section is the canonical cross-repository contract. The driver plan references it rather than redefining it.
Keep `list_connections`, `list_databases`, `list_tables`, `describe_table`, and `run_query` compatible.
Support the existing `output_format` option on the new tools as well.

| New MCP tool | Inputs and behavior |
|---|---|
| `start_query` | `connection_id`, `query`, optional `max_rows` (default 10,000; 1–10,000), optional `timeout_ms` (default 60,000; 1,000–300,000). Accept one read-classified SELECT statement, including a SELECT CTE. Return `query_id` and `queued` after registration. |
| `get_query_status` | `query_id`; return state, connection ID, timestamps, elapsed milliseconds, and terminal result metadata or a sanitized error. |
| `fetch_query_result` | `query_id`, optional `result_set_index=0`, `offset=0`, `limit=100` (1–1,000). Read the stored result without executing SQL again. |
| `cancel_query` | `query_id`; request cancellation of that owned execution and report whether it was accepted or already terminal. Acceptance is not proof of database termination. |
| `list_query_history` | Optional connection/status/time filters, `limit=50` (1–200), and an opaque continuation cursor. Return MCP execution metadata, not row data. |
| `release_query_result` | `query_id`; idempotently free a completed snapshot. Keep its history entry and reject release of an active execution. |

Reject writes, DDL, SELECT INTO, data-modifying CTEs, and multiple statements in `start_query`.
Use the host's existing classification and approval policy, with fail-closed regression fixtures.
Classification is not a substitute for database permissions; untrusted query execution requires an appropriate
read-only database principal. Tests on existing real connections submit only explicit SELECT statements.

Change the MCP stdin loop to keep reading while jobs execute. Serialize stdout writes through one writer.
Control requests must remain responsive while a query is running. Use a bounded queue of 16 managed jobs and
at most two running managed jobs per plugin process. Bound all ordinary RPC calls to the pinned PostgreSQL
plugin to three concurrent requests so its four-worker pool retains capacity for a cancellation notification.
Cancellation and status handling do not wait for a query execution slot. Reject excess work with `QUEUE_FULL`.

New modules under `src-tauri/src/mcp/` own the query manager, snapshots, and history adapter. The manager keeps
an unguessable query ID, owner instance, connection/configuration fingerprint, driver request handle, and state.
Use `queued`, `running`, `cancel_requested`, `succeeded`, `failed`, and `cancelled` as execution states.
Completion that wins a cancel race remains successful. Mark `cancelled` only after the executing driver call
returns a cancellation outcome; a dropped future or sent notification is insufficient.

Connection fingerprints cover the target database, driver, and non-secret identity settings. Recheck ownership
and current connection access for fetch/cancel/status calls. A changed authentication or target configuration
invalidates the old snapshot for retrieval. Never accept a client-supplied backend PID as a cancellation target.

### Saved-result paging

The first implementation uses bounded in-memory snapshots owned by the MCP server process. Execute the query
once, retain the driver's original columns and positional rows, and serve subsequent pages from that snapshot.
Keep result-set boundaries, duplicate column names, value types, and truncation information.
Do not implement this tool by adding OFFSET and rerunning the SQL.

Limit each snapshot to 10,000 retained rows across result sets and 16 MiB of serialized UTF-8 result data.
Limit retained snapshots to 64 and 128 MiB in total. These limits do not describe peak driver or decoder memory.
Reserve capacity before dispatch and reject additional work with `RESULT_CAPACITY_EXCEEDED` when the reservation
cannot be made. Expire a snapshot 15 minutes after
completion, or release it explicitly. Process exit invalidates snapshots; persistent history records that the
result is unavailable. Concurrent fetch and release must produce a complete page or `RESULT_EXPIRED`.

Return `RESULT_PENDING` before execution completes; do not expose an incomplete snapshot as complete.
After completion, an offset at or beyond the stored row count returns an empty final page.
Negative offsets, invalid result-set indexes, and invalid limits return `INVALID_ARGUMENT`.

Return `columns`, `rows`, `offset`, `next_offset`, `has_more`, `stored_row_count`, `truncated`, and
`truncation_reason`. `has_more` describes retained rows only. A driver row limit must be visible as truncation;
it must not imply that discarded rows can be fetched later. Reject an oversized byte result explicitly rather
than claiming a complete result. Disk-backed snapshots and continuation beyond driver caps are optional follow-ups.

### Cancellation and driver boundary

Expose a cancellable RPC handle from `PluginProcess`/`RpcDriver` without changing the existing query payload:
`execute_query` still carries params, SQL, row limit, page, and schema. Bind the host query ID to the exact
numeric JSON-RPC request ID and a per-execution generation. Send the existing id-less notification
`{"jsonrpc":"2.0","method":"cancel","params":{"id":<request-id>}}` for the active request only.

The pinned PostgreSQL plugin already registers a cancel action per request and uses its native cancel token.
SQL Server Plus must implement the same notification and bind it to its TDS `CancelHandle`; that work belongs
to the companion plan. Unsupported drivers must report `CANCELLATION_UNSUPPORTED`, not success.
Late, duplicate, unknown, and already-completed cancellation requests must be harmless.
For SQL Server Plus, the original query response uses error code `-32800`, message `Query cancelled`, and
`data: {kind: "query_cancelled", request_id: <numeric-id>, server_termination_confirmed: true}` only after
termination is confirmed. Preserve optional error data through the host RPC adapter. The PostgreSQL adapter
recognizes the pinned plugin's server cancellation response and waits for the original call to finish;
an acknowledgement of its id-less notification alone never confirms termination.

Apply `timeout_ms` as an end-to-end managed-query deadline. At expiry, request native cancellation and wait up
to five seconds for the execution to finish. Record `CANCEL_NOT_CONFIRMED` if termination cannot be established;
keep tracking the in-flight call and its slot until it actually finishes. Do not recycle its connection, kill
unrelated sessions, or replay SQL. A server shutdown requests cancellation for owned jobs and records any
unconfirmed termination instead of fabricating a successful cancellation.

### Query history

Reuse the existing MCP audit data model and its privacy setting. Record start and terminal events using the
same query ID so active executions can be listed. Retain SQL text, connection ID, status, timestamps, elapsed
milliseconds, row count, and sanitized error details; never persist credentials or result rows.
Honor `aiAuditEnabled=false` for persistent recording and the existing retention settings.
Keep enough in-memory state for the running process to manage its own jobs even when persistence is disabled.

Use the existing `fs2` lock and atomic rotation pattern for multi-process history writes. Treat older
`ai_activity.jsonl` records as completed legacy MCP history, deduplicated by their event IDs. Never invent
missing start times or running states. GUI query-history files remain intact; merging GUI and MCP execution
control is outside this release. An expired or restarted result can remain visible in history with
`result_available=false` and `cancellable=false`.
Only the current server instance can report a job as actively controllable. A historical start event without
a terminal event has an unknown outcome; do not label it cancelled or assume that the database stopped.

### Build, release, and compatibility

Track source changes, tests, locks, packaging, and build instructions in the two forks. Record an exact driver
release/checksum in each host release. Keep the separate Spatial and Cosmos repositories as their source of truth.
Their manifests, IIFE modules, relative assets, React/plugin API globals, and EXPLAIN parsers must remain usable.
Do not claim that an advertised service-protocol capability is verified merely because the plugin loads.

Use fork versions such as `0.27.0+extended.1`, matching all host version files, with a fork-owned release tag.
Disable the official update feed in the fork until a separately signed fork feed is configured. The first release
uses explicit artifact installation; it must not silently replace the app with an official build.
Publish no release in this documentation-only task. Future releases require passing gates below, artifact hashes,
and a rollback test using the original app/profile. Keep signing keys and production configuration out of Git.

## Execution slices

Main ownership retains the coordinator's selected model and effort. Delegated implementation uses the coding
host's pinned implementation-worker role, model, and effort. UI work uses its pinned designer route.
The coordinator makes contract and scope decisions; workers do not widen their assignments.

| Slice / owner | Result and allowed paths | Protected paths and prerequisites | Shared resources / parallel condition | Completion evidence |
|---|---|---|---|---|
| H1 / implementation worker | Port plugin registration/import/diagnostic behavior; host identity, profile migration, revisioned persistence; relevant Rust modules and tests, app metadata | Preserve unrelated upstream code, real profiles, credentials, Spatial/Cosmos source; use the frozen baseline and this contract | Can run alongside driver D1; owns host Rust/config paths and its build directory | Importer/registry/CLI tests, profile and stale-save fixtures, source provenance report |
| H2 / implementation worker | Add MCP dispatch concurrency, query manager, snapshot paging, cancellation handles, and history; `src-tauri/src/mcp/`, driver trait/RPC adapter, audit tests | H1 accepted; preserve legacy tool payloads and existing write policy | One owner for the cohesive backend; do not overlap H1 or another host Rust build | Deterministic transport, race, snapshot, history, and compatibility tests |
| H3 / designer route | Distinct app labels, explicit profile import, stale-editor conflict, password-only visibility; modal/context and plugin API UI files/tests | H1 and the shared UI contract accepted; driver auth semantics fixed | Can run with D2 backend work; separate frontend output, no edits to H2 files | Component/type checks and reviewed UI behavior without real credentials |
| H4 / coordinator | Integrate the pinned SQL Server Plus build, package fork releases, update build/release docs and CI | H2, H3, and driver D2/D3 accepted; maintain all plugin contracts | Serial integration owner; no build while workers mutate its inputs | Integrated acceptance matrix, reproducible artifact hashes, upgrade and rollback evidence |

H2 may implement against a deterministic driver fixture while SQL Server cancellation is being developed.
Real end-to-end cancellation acceptance waits for driver D2. This breaks the dependency without weakening the gate.
Host H4 and driver D4 are one coordinated integration run. They share one installation owner and reuse its
evidence instead of repeating the same checks in each repository.

## Integrated verification

| Acceptance criterion | Required evidence |
|---|---|
| A1: current setup is preserved | Isolated migration fixtures retain IDs/settings/keychain references; opt-in fresh-connection `SELECT 1` succeeds for all 14 existing connections after migration |
| A2: TLS cannot regress on save | A stale password editor receives a conflict; a current password-only save preserves `verify-full`, auth `extra`, and other connections |
| A3: one execution, repeatable pages | A recording driver counts exactly one SQL execution; page boundaries have no duplicates/omissions and remain identical after the backing fixture changes |
| A4: bounded result lifecycle | Tests cover empty results, duplicate column names, exact limits, truncated output, byte budgets, expiry, release/fetch races, and process restart |
| A5: real cancellation | Control requests remain responsive under saturation; queued/running/late/duplicate cases pass; approved nonproduction SELECT tests prove the target execution ends and a neighboring execution survives |
| A6: authentication is preserved | Companion auth tests plus new physical-connection checks cover SQL passwords, Entra token refresh, identity mismatch, TLS rejection, and keychain-backed secrets |
| A7: useful and private history | Running/terminal/legacy history, filters, paging, restart, retention, concurrent writers, audit-disabled behavior, and secret redaction pass |
| A8: existing surfaces remain usable | Five legacy MCP tools, JSON/TOON, GUI connections, Spatial basic maps, Cosmos baseline read paths, and EXPLAIN parser fixtures pass; unsupported advanced features stay explicitly unsupported |
| A9: maintained release | Source-only clean checkout builds with locked dependencies; packaged plugins include UI/parser assets; install, update isolation, and rollback are checked |

Run narrow Rust tests for changed modules, then the frontend component/type checks and plugin API sync check.
Use the repository's `pnpm` version, `pnpm typecheck`, `pnpm lint`, `pnpm build`, and
`cargo test --manifest-path src-tauri/Cargo.toml --locked --lib` as applicable to implemented changes.
Use separate build directories for concurrent workers. Run broader suites only for cross-surface changes or failures.

All live validation remains SELECT-only. Do not run seed scripts, database-creating integration suites,
DDL/DML, KILL, or administrative termination commands. Use recorded/fake driver fixtures for destructive and
failure scenarios. Real cancellation uses protocol cancellation of an owned SELECT on an approved nonproduction
connection; production connectivity checks use only `SELECT 1`. Adding a write-based test requires a new user decision.

The existing 14-connection smoke evidence and 239 custom-driver unit-test result are reusable only while their
covered inputs remain unchanged. They do not prove this future fork's migration, paging, or cancellation.
Invalidate affected evidence after changing source, manifests, authentication, profile routing, or packaging.

Stop release work on missing credentials, a source/archive mismatch, stale profile conflicts, incompatible
plugin contracts, unconfirmed cancellation, exposed secrets, or failed required checks. Preserve completed work
and resolve the cause without silently weakening TLS, changing identity, or retrying SQL.
Optional follow-ups are upstream PRs, larger streaming/disk result stores, more drivers, and the broader DataGrip
DB-tool parity that the user explicitly left outside this implementation scope.
