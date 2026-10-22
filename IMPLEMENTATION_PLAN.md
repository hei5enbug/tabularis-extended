# Tabularis Extended implementation plan

This document defines future implementation work. It changes no code.
The coordinator owns this plan and every cross-component contract in this repository.

## Intent and scope

Make Tabularis Extended's MCP server cover the database work that a database IDE's MCP server offers, while keeping
the existing connections, authentication, and plugins working. All components live in this repository.

| Item | Decision |
|---|---|
| Repository | [hei5enbug/tabularis-extended](https://github.com/hei5enbug/tabularis-extended), branch `feature/monorepo-extensions` |
| Host code baseline | `313ef093408a15c38f1df9342c1f779aa3e7d27c`, upstream `0.27.0` |
| Extensions | `extensions/sqlserver-plus`, `extensions/spatial`, `extensions/azure`, imported with history by `git subtree` |
| PostgreSQL plugin | `extensions/postgresql`, to be imported from the official `tabularis-postgresql-plugin` |
| Service layer source | `extensions/spatial/integration/patches/{host,postgresql,sqlserver}.patch` and `upstreams.json` |
| Current installed host | `0.26.0+local.1`, based on `5d7e27408907c0faafc8cc6b940725f4a1258f42` |
| Driver companion plan | [SQL Server Plus plan](extensions/sqlserver-plus/IMPLEMENTATION_PLAN.md) |

### Coverage target

A reference database IDE's MCP server was connected on 2026-10-21 and listed these database tools.
IDE tools such as file search, refactoring, run configurations, and builds are outside this plan.
The new tools target PostgreSQL and SQL Server Plus connections. Cosmos uses its document tools.

| Reference tool | Tabularis Extended tool after this plan |
|---|---|
| `list_database_connections` | `list_connections` (existing) |
| `test_database_connection` | `connection_test` |
| `create_database_connection` / `edit_database_connection` | `connection_create` / `connection_update` |
| `list_database_schemas` | `catalog_databases`, `catalog_schemas` |
| `list_schema_object_kinds` / `list_schema_objects` | `catalog_objects` with a `kind` filter and the driver's supported kinds |
| `get_database_object_description` | `catalog_describe` for tables, views, materialized views, routines, and triggers |
| `introspect_schema` | Not needed: Tabularis reads catalog metadata live on every call |
| `preview_table_data` | `table_preview` |
| `execute_sql_query` | `run_query` (existing) and `query_execute` with optional `database` and `schema` |
| `fetch_query_result` | `result_get` over a stored result |
| `cancel_sql_query` | `job_cancel` |
| `list_recent_sql_queries` | `job_list`, including running jobs |

### In scope

- Port the archived common service layer onto the `0.27.0` host as maintained source.
- Add the service driver protocol to the PostgreSQL plugin fork and to SQL Server Plus.
- Expose service operations as MCP tools with valid names, flat arguments, and useful descriptions.
- Add confirmed database cancellation, query history, and per-call database and schema selection.
- Let MCP create, update, and test connections without ever receiving a secret.
- Allow writes through asynchronous execution under one write policy shared with `run_query`.
- Recover the installed host patch, give the fork its own identity and profile, and make connection saves safe.
- Port the SQL Server Entra authentication described in the companion plan.
- Enable the Spatial and Azure features that depend on the service layer: saved maps, result maps, MCP map
  control, Cosmos continuation, document CRUD, and in-app Entra authentication for Cosmos.
- Run every extension's CI from the repository root and retire the patch-based integration directory.

### Non-goals

- IDE features: file editing, code search, refactoring, builds, and run configurations.
- Deleting connections or importing credentials through MCP, or delivering any secret through MCP arguments.
- Service tools for MySQL and SQLite connections; they keep the five existing tools.
- Service-principal or host-managed authentication for SQL Server, automatic privileged access, or new database
  permissions.
- Result storage beyond the service layer's bounded private cache, server cursors, or fetching rows that a
  driver limit discarded.
- Publishing connection files, passwords, tokens, tenant or object identifiers, private endpoints, or live logs.

### Constraints and protected surfaces

- Keep the five existing tools (`list_connections`, `list_databases`, `list_tables`, `describe_table`,
  `run_query`) and their JSON and TOON output. New optional arguments are allowed; changed defaults are not.
- Keep connection IDs, names, database selections, authentication, TLS, and keychain storage.
- Never weaken TLS, substitute identities, replay SQL, or report an unconfirmed cancellation as confirmed.
- Keep the official app, its profile, and the official plugin packages untouched for rollback.
- Every MCP tool name must match `^[a-zA-Z0-9_-]{1,64}$`. The patch's dotted names do not.
- Follow `.rules/*.md` for code and English documentation.

### Acceptance criteria

The verification table at the end maps each criterion to evidence. The work is complete when A1 to A15 pass.
Optional follow-ups are listed in the last section and are not completion gates.

### Stop conditions

Stop the affected work and report on a source or archive mismatch, missing credentials, an exposed secret,
an incompatible plugin contract, a stale profile conflict, unconfirmed cancellation presented as confirmed,
or a failed required check. Preserve completed work. Never retry SQL or weaken a check to pass it.

## Evidence

### Installed baseline

These read-only observations predate the fork. They are baseline facts, not tests of the fork.

| Surface | Observed state | Consequence |
|---|---|---|
| Saved connections | 14 connections: six PostgreSQL, six Entra SQL, two SQL-password | Preserve all IDs; repeat `SELECT 1` after migration |
| PostgreSQL | Official plugin `1.0.0-rc.6`, `verify-full` TLS | The fork plugin must keep its connection and TLS behavior |
| SQL Server | Official `1.0.0-beta.3` plus local `sqlserver-entra` `1.0.0-beta.3+local.entra.1` | Port the auth delta through the companion plan |
| Spatial / Cosmos | Installed `0.1.0` packages, runtime floor `0.26.0` | Keep their basic paths; enable service features after the port |
| EXPLAIN | PostgreSQL plans and the SQL Server `sqlserver-showplan-xml` parser | Keep parser registration and raw result shape |

### Current host behavior

Read from the `313ef093` source on 2026-10-21.

- The MCP server is the same binary started with `--mcp`. It shares state with the GUI only through files.
  Its stdin loop handles one request at a time (`src-tauri/src/mcp/mod.rs`, `run_mcp_server`).
- `run_query` classifies SQL as `select`, `write`, `ddl`, or `unknown` (`ai_activity::classify_query_kind`).
  `SELECT … INTO` is classified as `select`. A read-only connection rejects anything but `select`.
  `mcp_approval_mode` is `off`, `writes_only` (default), or `all`. Approval travels as files in
  `pending_approvals/` and is shown by `AiApprovalModal`, which can edit the query.
- `run_query` always executes on the saved connection's first database with `schema = None`.
  Only `insert_record`, `update_record`, and `delete_record` accept a per-call `database` override.
- The GUI `cancel_query` aborts the host task only. The plugin `cancel` notification is sent only after a call
  timeout (`src-tauri/src/plugins/driver.rs`, `send_request`).
- Twenty-odd code paths write `connections.json` through `persistence::save_connections_file` without a lock.
  `extra` is stored verbatim and is not treated as a secret.
- The GUI `test_connection` accepts arbitrary `params` plus a `connection_id` and fills the password from the
  keychain of that saved connection without comparing targets (`commands.rs`, `resolve_test_connection_password`).

### Archived service layer

`host.patch` adds 202 files (+35,743 / -353 lines): service contracts, a runtime with jobs, results, sessions,
policy and approvals, local IPC, a native CLI, an MCP transport, the Spatial domain, and frontend hooks.
It defines 50 operations: `connection.*`, `catalog.*`, `query.execute`, `query.next`, `result.get`,
`result.release`, `row.*`, `document.*`, `session.*`, `spatial.*`, `map.*`, `job.*`, `auth.*`,
and `credential.import`. Its MCP transport rejects `credential.import`.

On 2026-10-21, `git apply --check` against `313ef093` failed for six files only: five version files
(`package.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, `src-tauri/tauri.conf.json`,
`src/version.ts`) and `src/pages/Editor.tsx`. The other 196 files apply.
`sqlserver.patch` applies to `extensions/sqlserver-plus` without failures; its base `58c47b3` is three commits
behind the imported head. `postgresql.patch` targets the official PostgreSQL plugin at base `963f358`.

Facts the plan must handle:

- The service runtime calls a driver only when its `initialize` result has `service_capabilities` with
  `service_protocol: 1` (`parse_service_capabilities` in `host.patch`). `postgresql.patch` advertises
  `exact_query`, `spatial_v1`, `sessions`, `cancel_v1`, `catalog`, `rows`, and
  `readonly_guarantee: read_only_transaction`. `sqlserver.patch` adds a service module whose read-only probe
  succeeds only for a principal without write permissions.
- The runtime rejects every read when the driver's read-only guarantee is unavailable (`WriteNotAllowed`).
- Connection validation accepts `auth_source: azure_cli` only for drivers `sqlserver` and `cosmos-nosql`,
  requires the Azure CLI `client_id`, and rejects `extra.username`. The companion's driver-owned Entra contract
  uses `username`, `tenant_id`, and `entra_object_id` on driver `sqlserver-plus`.
- Cancellation reaches drivers through the service `cancel_request` method. The PostgreSQL patch implements it.
- Results are cached as private files under the profile's `results-v1` directory: at most 10,000 rows and
  32 MiB per result, kept 15 minutes. MCP output is cut to a 12 KiB data budget inside a 64 KiB response.
- The MCP path `run_one` waits for a job to reach a terminal status before replying.
- Tool names contain dots, arguments are a full request envelope, and every description is
  `Common service {name}`.
- The job store keeps at most 256 records for 15 minutes, so it is not a query history.
- The patch's own validation records leave Cosmos CRUD, continuation, and in-app login unverified against Azure.

## Implementation strategy

### Repository layout

Keep upstream's root layout so `git merge upstream/main` keeps working. Keep each extension as its own pnpm
and Cargo workspace under `extensions/`; do not add them to the root workspace files.
Move each extension's `.github/workflows/*` into root workflows with `paths:` filters and a matching
`working-directory`. Use `git subtree pull` with the extension prefix to take upstream plugin changes.
After the host and driver ports, delete `extensions/spatial/integration/` and
`extensions/spatial/scripts/bootstrap.mjs`, and replace version references to `0.26.1-spatial.1`.

### Port the service layer

Apply `host.patch` with `git apply --3way` on top of `313ef093`. Resolve the six failing files by keeping the
`0.27.0` content and re-adding only the patch's functional hunks; version strings follow the fork version.
Resolve the `service_native`, `service_commands`, and `service_platform` module references to real modules.
Run `cargo fmt` on the ported Rust and the repository formatter on ported TypeScript so the code matches the
surrounding style. Keep the patch's tests and make them pass before any behavior change.
The legacy MCP loop stays until the MCP surface slice switches it; the legacy tools must not change.

### Driver service protocol

Import the official PostgreSQL plugin with `git subtree add --prefix=extensions/postgresql` at `963f358`,
apply `postgresql.patch`, then merge the upstream release that matches the installed `1.0.0-rc.6` or newer.
Give the package its own ID, `postgresql-plus`, and display name `PostgreSQL Plus`, as SQL Server Plus does.
Keep its existing connection, TLS, and cancel behavior; its `cancel_request` handler is the service cancel path.

Apply `sqlserver.patch` to `extensions/sqlserver-plus` as part of the companion plan. Its service path must
authenticate with the companion's driver-owned Azure CLI flow, not with host-supplied tokens.

### Host recovery and identity

Port the installed local patch semantically:

1. Keep only MySQL and SQLite as built-in drivers in GUI and standalone MCP startup.
2. Map foreign imports to `postgresql` and `sqlserver`. Keep `mssql` as a dialect name. Never infer Entra.
3. Keep upstream plugin discovery, lazy MCP registry reload, and driver-migration UI.
4. Restore `--list-drivers` as a manifest-only diagnostic that opens no connection and writes nothing.

Use display name `Tabularis Extended`, executable `tabularis-extended`, bundle identifier
`io.github.hei5enbug.tabularis.extended`, and `tabularis-extended` for the profile directory and keychain
service. Use version `0.27.0+extended.1` in every host version file. Disable the official update feed.

First-run import is explicit. Back up the source profile, then atomically create the fork profile with the same
connection IDs, names, database selections, groups, tags, saved queries, notebooks, themes, and settings.
Copy platform plugin packages into the fork's plugin directory. Move saved and inline passwords into the fork
keychain and remove them from destination JSON only after the keychain write is verified. Never overwrite a
nonempty destination profile. In the fork profile, after each package is installed and verified, map
`postgresql` connections to `postgresql-plus` and `sqlserver` and `sqlserver-entra` connections to
`sqlserver-plus`, keeping their authentication mode.

### Safe connection saves

Add one profile-level lock with `fs2`, atomic replacement, and a per-connection `revision` to every writer of
`connections.json`, including migrations, imports, group and tag commands, and the service layer.
An editor or MCP call sends the revision it read. Under the lock, reload the file and reject a stale revision
with `CONNECTION_CONFLICT` before any keychain or file change. Preserve unknown fields and other connections.
A failed keychain write leaves the file untouched; a failed file write restores the previous secret or reports
an explicit recovery requirement.

A password-only edit keeps `ssl_mode`, `driver`, the database selection, and `extra`.
Add `setPasswordFieldHidden(boolean)` to the `connection-modal.extra_fields` context in
`packages/plugin-api/src/slots.ts`; keep `setCredentialFieldsHidden(boolean)` compatible.

Change the service connection validation so `sqlserver-plus` accepts the companion's Entra fields
(`auth_mode: entra_user`, `auth_source: azure_cli`, `username`, `tenant_id`, `entra_object_id`, optional
`azure_cli_path`) without `client_id`, and so the host never acquires tokens for it. Keep the patch's
validation for Cosmos.

### One write policy

Both `run_query` and the service query tools use the existing settings as the single source:
`mcp_readonly_default`, `mcp_readonly_connections`, and `mcp_approval_mode`. Map the service layer's
`connection.allow_writes` and `permission.allow_writes` checks onto `config::is_connection_readonly`.

Fix the shared classifier first: `SELECT … INTO`, `EXEC`, `EXECUTE`, `CALL`, and multiple statements are not
`select`. Any statement the classifier cannot prove is a read counts as a write.

| Rule | `run_query` | `query_execute` and other service writes |
|---|---|---|
| Write detection | Classification; `unknown` counts as a write | Explicit `write: true`, and classification must also pass |
| Missing intent | Not applicable | A non-`select` statement without `write: true` fails with `WRITE_INTENT_REQUIRED` |
| Read guarantee | Classification only | Driver guarantee when available; otherwise classification, reported as `classification_only` |
| Read-only connection | Rejects non-`select` | Rejects every write before queueing |
| Approval | `mcp_approval_mode`; GUI may edit the query | `mcp_approval_mode`; single-use, bound to the request hash, no edits |
| After cancel or timeout | Never retried | Never retried; outcome is `unknown` unless the driver confirms |

Replace the runtime's blanket read rejection with this rule: when the driver reports no read-only guarantee,
a read runs only if the classifier returns `select`, and every result reports `readonly_guarantee`.
Row, document, and session-commit operations are writes. Keep `run_query`'s approval channel unchanged.
The service approvals use the ported backend and must appear in the same GUI approval gate.

### MCP surface

Switch `--mcp` to the ported service transport, which must read requests concurrently and serialize stdout.
Control calls such as `job_get` and `job_cancel` must not wait behind a running query or an approval.

Expose every service operation except `credential.import` and `connection.delete` as one tool, and reject
direct calls to those two. Replace dots with underscores (`connection.test` becomes `connection_test`).
Accept the operation's input fields as top-level arguments plus `connection_id`, `session_id`, `map_id`,
`expected_version`, and `deadline_ms` where the operation uses them. The transport generates
`protocol_version` and `request_id`. Write a one- or two-sentence description for every tool that states what
it returns and whether it can write. Keep the five legacy tools.

`query_execute` returns as soon as the job is registered. Its optional `wait_ms` (default 5,000, 0–60,000)
lets a fast job return its first page in the same reply. Otherwise the reply holds `job_id`,
`owner_instance_id`, and `status` (`queued`, `awaiting_approval`, or `running`). Approval never blocks the reply.

Add these tools, which the service layer lacks:

| Tool | Behavior |
|---|---|
| `table_preview` | `connection_id`, `table`, optional `database`, `schema`, `limit` (default 10, 1–1,000). Read-only. |
| `job_list` | Running and recent jobs from this process plus persisted history; filters by connection, status, time; `limit` 1–200 with an opaque cursor. No row data. |

Add optional `database` and `schema` to `run_query`, `query_execute`, `list_tables`, `describe_table`, and the
catalog tools. A `database` value must be one the driver lists for the connection. The host applies it by
setting `params.database` to that single database for the call, as `insert_record` already does. Report the
effective database and schema in every result.

`catalog_objects` and `catalog_describe` return `UNSUPPORTED_OPERATION` with the object kind for a kind the
driver lacks, never an empty success. Read the supported kinds from driver capabilities and connection metadata.

`result_get` returns `columns`, `rows`, `offset`, `next_offset`, `has_more`, `stored_row_count`, `truncated`,
`truncation_reason`, and `readonly_guarantee`. `limit` is 1–1,000 rows. When rows exceed the 64 KiB response
limit, return fewer rows and a correct `next_offset` instead of cutting values. Never page by re-running SQL.

Keep the private `results-v1` cache. Create its files with owner-only permissions, delete a result on release
or expiry, and remove leftover files at startup. Query history never stores rows.

### Connection tools

`connection_create` and `connection_update` accept only non-secret fields. Reject `password`,
`ssh_password`, `ssh_key_passphrase`, `connection_uri`, and any `extra` key whose name contains `password`,
`secret`, `token`, or `key` with `SECRET_NOT_ACCEPTED`. The user enters secrets in the GUI.

Create and update always require GUI approval, regardless of `mcp_approval_mode`. The approval shows the full
non-secret change. When an update changes the target or identity (`driver`, `host`, `port`, `username`,
`ssl_mode`, SSH, Kubernetes, SSM, proxy, or `extra`) of a connection that has stored secrets, the approval
states that stored credentials will be sent to the new target. Updates use the revision protocol above.

`connection_test` takes only `connection_id` and tests the saved connection through the GUI expansion path for
SSH, Kubernetes, IAM, keychain, and URI restore. It accepts no parameter overrides.

### Cancellation

`job_cancel` sends the service `cancel_request` for the job's driver request. The PostgreSQL patch implements
it; the companion plan implements it in SQL Server Plus with the TDS cancel handle. A job becomes `cancelled`
only when the driver returns a cancellation outcome. Otherwise record `CANCEL_NOT_CONFIRMED`, keep the slot until
the call ends, and never recycle its connection or replay SQL. A completion that wins the race stays
successful. A cancelled write has an `unknown` outcome. `deadline_ms` is end-to-end: at expiry, request
cancellation and wait up to five seconds. `run_query` keeps its current cancellation behavior.

### Query history

Record a start event and a terminal event for every job with the same job ID, reusing the MCP audit model, its
privacy setting `ai_audit_enabled`, and its retention settings. Store SQL text, connection ID, status,
timestamps, elapsed milliseconds, row count, and a sanitized error; never store credentials or rows.
Write with the `fs2` lock and atomic rotation. Read older `ai_activity.jsonl` records as completed legacy history.
A start without a terminal event has an unknown outcome. Only the current process reports a job as cancellable.

### Spatial and Azure

Point both extensions at the fork host. Set their manifests' runtime floor to `0.27.0+extended.1` for service
features and keep the `0.26.0` basic path. Verify that the frozen SDK snapshots in
`extensions/*/build-support/sdk` match the root `packages/plugin-api` and `packages/service-contracts`.

## Execution slices

Main-owned slices keep the coordinator's model and effort. Delegated implementation slices use the
`hei5enbug-agent-setup:sonnet-worker` route pinned by the host agent rules. UI slices use the designer route.
Host Rust slices run one at a time because they share `src-tauri` files and its build directory.
Plugin slices use their own extension directory and build directory, so they may run beside host slices.

| Slice / owner | Result and allowed paths | Depends on | Parallel condition | Completion evidence |
|---|---|---|---|---|
| M1 / main | Root CI workflows for extensions; `.github/`, `extensions/*/.github/` | None | Parallel with any slice | Workflow syntax check; extension tests unchanged |
| S1 / worker | Service layer ported and formatted; `src-tauri/`, `src/`, `packages/`, root `package.json`, `pnpm-lock.yaml` | None | Exclusive host build | Patch tests, `cargo test --lib`, `pnpm typecheck`, legacy MCP tool tests |
| P1 / worker | PostgreSQL plugin imported, patched, renamed; `extensions/postgresql/` | None | Own build directory | Plugin tests, `service_capabilities` fixture, TLS and cancel fixtures |
| H1 / worker | Host recovery, identity, profile import, locked revisioned saves, Entra validation for `sqlserver-plus`; host Rust, config, metadata | S1 | Exclusive host build | Importer, registry, CLI, profile, stale-save, and validation fixtures |
| S2 / worker | Classifier fix, write policy, MCP surface, connection tools, database/schema selection, catalog, preview, history, results cache rules; `src-tauri/src/{mcp,services}/`, `ai_activity.rs`, approvals | H1 | Exclusive host build | A3–A10 fixture tests |
| D0–D2 / worker | Companion plan: `sqlserver.patch`, Entra port, service cancellation; `extensions/sqlserver-plus/src` | None | Own build directory | Companion P1–P4 |
| U1 / designer | App labels, profile import, stale-edit conflict, password-only field, connection-change and write approvals; `src/` UI files | H1, S2 | No edits to Rust | Component and type checks |
| D3 / designer | Companion plan auth UI; `extensions/sqlserver-plus/ui` | D1, U1 slot contract | Separate frontend output | Companion D3 evidence |
| X1 / main | Spatial and Azure re-pointed; SDK equality check; `integration/` removed | S1, P1, D0 | Not during a host build | Extension test suites, SDK equality |
| I1 / main | Integration, packaging (including companion D4), migration, release notes, live checks | All slices | Serial | A1–A15 |

S1 runs before H1 because the patch applies cleanly only to files that H1 has not yet changed.
S2 tests against deterministic driver fixtures; real cancellation acceptance waits for P1 and D2.
Each worker assignment states that other workers may edit the repository and that their changes must be kept.

## Integrated verification

| Criterion | Required evidence |
|---|---|
| A1: setup is preserved | Migration fixtures keep IDs, settings, and keychain references; `SELECT 1` passes on all 14 connections |
| A2: saves cannot regress | A stale editor or MCP update gets `CONNECTION_CONFLICT`; a password-only save keeps `verify-full` and `extra` |
| A3: tool surface is valid | Names match the pattern; every tool has a description; `credential_import` and `connection_delete` are absent and direct calls fail; legacy output is byte-compatible |
| A4: write policy holds | Fixture matrix: read-only, approval modes, missing intent, `unknown`, `SELECT INTO`, `EXEC`, multi-statement, row/document/session writes, guarantee label |
| A5: secrets never enter MCP | Secret fields and secret-like `extra` keys are rejected; no secret in tool output, audit, or history |
| A6: connection writes are approved | Create and update always wait for GUI approval; target changes show the credential warning; `connection_test` rejects overrides |
| A7: one execution, stable pages | A recording driver counts one execution; pages have no gaps or duplicates under 64 KiB; cache files are owner-only and deleted on release, expiry, and startup |
| A8: catalog is complete or explicit | Per-driver fixtures return objects or `UNSUPPORTED_OPERATION` for each kind |
| A9: database selection works | PostgreSQL Plus and SQL Server Plus fixtures run on the requested database and report it |
| A10: history and async replies | `query_execute` replies before execution and approval finish; running, terminal, legacy, filtered, restarted, and audit-disabled history cases pass |
| A11: cancellation is real | Control calls stay responsive under load; approved nonproduction SELECTs prove termination and that a neighbor survives |
| A12: authentication is preserved | Companion P1–P2; SQL password, Entra refresh, identity mismatch, TLS rejection; service create/update/query fixtures with the companion Entra fields |
| A13: drivers speak the service protocol | PostgreSQL Plus and SQL Server Plus `initialize` fixtures report `service_capabilities`; existing plugin tests pass |
| A14: extensions work | Spatial and Azure test suites; basic maps and Cosmos reads; service features on the fork host |
| A15: release is reproducible | Clean locked build; packaged plugins with UI and parser assets; install, update isolation, rollback |

Run narrow tests for each slice, then `pnpm typecheck`, `pnpm lint`, `pnpm build`, and
`cargo test --manifest-path src-tauri/Cargo.toml --locked --lib`. Run each extension's own suite after any
change under `extensions/`. Use Node `24.21.0` for the Spatial and Azure scripts.

Live checks are SELECT-only. Do not run seed scripts, database-creating suites, DDL, DML, KILL, or
administrative termination. Use recorded or fake drivers for writes and failures. Live verification of Cosmos
document CRUD, row writes, or connection creation against a real server needs a separate user decision that
names a nonproduction target. Until then, A14 covers those features with fixtures only and says so.

Evidence stays valid only while its inputs are unchanged. The 14-connection smoke result and the 239-test driver
result are historical and do not prove this fork's behavior.

## Optional follow-ups

- Upstream pull requests for generic fixes.
- Result storage and continuation beyond driver row limits.
- A single approval channel for `run_query` and service operations.
- Consuming root `packages/` directly from the extensions instead of frozen SDK snapshots.
- Service protocol and cancellation for built-in MySQL and SQLite.
