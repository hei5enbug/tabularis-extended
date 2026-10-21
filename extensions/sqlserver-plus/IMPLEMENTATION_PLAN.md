# SQL Server Plus implementation plan

This document defines future implementation work. This commit changes documentation only.
The driver preserves the currently used SQL Server authentication and adds the driver support required by
Tabularis Extended's saved-result paging, query cancellation, and query history.

## Intent and scope

Maintain the local SQL Server improvements as a reproducible fork with a distinct package identity.
Use the host plan as the canonical owner of MCP tools, managed-query states, paging, history, and cancellation
semantics: [Tabularis Extended plan](https://github.com/hei5enbug/tabularis-extended/blob/main/IMPLEMENTATION_PLAN.md#managed-query-contract).
The driver remains a JSON-RPC subprocess; it does not become a second MCP server.

| Item | Baseline or target |
|---|---|
| Repository | [hei5enbug/tabularis-sqlserver-plus](https://github.com/hei5enbug/tabularis-sqlserver-plus) |
| Current fork code baseline | `885ea261edd424078a59aaa584c2cfc25f2969ae` |
| Local source base | Official `v1.0.0-beta.3` |
| Installed original package | `sqlserver`, version `1.0.0-beta.3` |
| Installed custom package | `sqlserver-entra`, version `1.0.0-beta.3+local.entra.1` |
| Target package ID | `sqlserver-plus` |
| Target display name | `SQL Server Plus` |
| Target host | The fork and profile defined by the companion plan |
| Verification boundary | Existing database tests submit SELECT statements only |

In scope:

- Port the working Azure CLI/Entra authentication implementation and its tests onto current fork main.
- Preserve SQL-password and Windows/Kerberos authentication, verified TLS, connection strings, and pool behavior.
- Add request-scoped SQL Server cancellation using the existing TDS library's cancellation handle.
- Make row limits and truncation usable by the host's saved-result store without replaying SQL for its pages.
- Provide an auth UI, plugin manifest, UI assets, EXPLAIN parser, build/release flow, and compatibility evidence.
- Retain the upstream fixes that landed after the local source baseline.

Non-goals are a separate MCP server, driver-owned persistent query history, host query-ID allocation,
unbounded export/server cursors, new database writes, service-principal/device-code authentication, and changing
the behavior of other database engines. Those features are not prerequisites for this release.

## Source inventory and preservation

The locally installed custom binary was built from a separate source copy. Its source archive and deployment
receipt are recovery inputs, not publishable configuration bundles. The published repository must contain the
source changes and safe fixtures, not passwords, tokens, live connections, identity values, or private endpoints.
Use `sqlserver-entra-source.tar.gz` and `deployment.json` from the platform application-support sibling
`tabularis-local-deployments/connection-repair-*/` directory, following the host plan's provenance checks.

| Existing local surface | Behavior to port | Preservation check |
|---|---|---|
| `src/driver/azure_cli_auth.rs` | Azure CLI command, auth validation, JWT metadata checks, pure fixtures | Port this added module and its tests; keep errors free of token/stdout/stderr content |
| `src/driver/pool.rs` | Acquire a token for each new physical connection and apply it to a local config clone | Keep ordinary SQL/integrated paths and existing reset/discard behavior |
| `src/pool_manager.rs` | Entra identity, CLI path, host, and port participate in pool identity | Distinct identities/targets cannot reuse one pool |
| `src/driver/mod.rs` | Register the new auth module | Preserve current result conversion and bounded collection |
| `src/driver/pool/tests.rs` | Config and manager regression checks | Preserve existing TLS and pool tests |
| Installed `.tabularium` | Separate custom driver identity and adjusted UI extension routing | Generate the manifest from maintained source rather than an installation-only rewrite |

The local copy predates upstream's `d7e40cd` compatibility fix. Current main uses `CONVERT` rather than
`TRY_CONVERT` for metadata comments and `.value('.', 'nvarchar(max)')` in trigger XML extraction.
Keep `src/driver/introspection.rs`, `src/driver/triggers/mod.rs`, and their current tests unless this plan requires
a specific change. Copying the whole older tree would regress these fixes and is prohibited.

The 239 passing tests and successful locked release build recorded during the local repair are historical
evidence for that source copy. Run the affected tests again after porting to this different baseline.
The prior live result covers eight SQL Server connections: six Entra connections and two SQL-password
connections. It establishes basic SELECT connectivity, not arbitrary workloads or cancellation behavior.

## Implementation strategy

### Authentication contract

Preserve `ConnectionParams` and its `extra: HashMap<String, String>` extension mechanism.
Use the following fields for Entra user authentication; examples must contain placeholders only.

| Field | Required behavior |
|---|---|
| `extra.auth_mode` | `entra_user` enables Entra; absent means the existing SQL/integrated-auth path |
| `extra.auth_source` | Must equal `azure_cli` for Entra |
| `username` | Expected user principal; required for Entra |
| `extra.tenant_id` | Expected tenant; required for Entra |
| `extra.entra_object_id` | Expected principal object ID; required for Entra |
| `extra.azure_cli_path` | Optional absolute executable path; otherwise resolve `az` through PATH |
| `ssl_mode` | Normalize `verify_identity` to `verify-full`; Entra requires `verify-full` |
| `trust_server_certificate` | Must be false for Entra |

Do not infer Entra from an email-shaped username. Reject unsupported modes/sources, missing identity fields,
a nonempty password combined with Entra, or integrated authentication combined with Entra.
Entra targets must be public-cloud Azure SQL hosts ending in `.database.windows.net`; supporting sovereign
clouds or other resources is outside this release. Keep existing SQL-password and integrated behavior unchanged.

Start Azure CLI without a shell and request `https://database.windows.net/` for the configured tenant.
Capture its output privately. Use process cancellation/kill-on-drop and include token acquisition within the
connection deadline. Never print CLI output, persist tokens, pass tokens in process arguments, or fall back to
SQL authentication after Entra failure.

Validate JWT shape and metadata before using the token: tenant and object ID must match; any present UPN,
unique-name, or preferred-username claim must match the expected username without case sensitivity;
the audience must be the SQL resource; expiry must exceed the current time by more than 120 seconds.
Azure SQL performs the cryptographic signature validation. The local payload checks must not be described as
signature verification. A near-expiry or mismatched token fails with a sanitized error.

Acquire a token for every new physical connection, including reconnection and a new pool member.
Existing authenticated sessions can continue; do not claim that established sessions are reauthenticated.
An Azure CLI account change must fail the configured identity checks rather than silently using another user.
CLI login/MFA remains a user action. Do not change subscription, tenant, login state, or credential storage
automatically.

Include auth mode/source, tenant, object ID, username, CLI path, host, port, database, and relevant TLS settings
in pool identity. Preserve password-rotation behavior unless a separately demonstrated defect blocks this plan.
Never put access tokens in pool keys, Debug output, or persisted settings.

### Connection UI and package identity

Set `.tabularium.id` to `sqlserver-plus`, its display name to `SQL Server Plus`, and its executable to
`sqlserver-plugin`. Update the UI extension's driver selector to `sqlserver-plus`.
Repository naming and package identity are independent; the package identity is explicit and stable.

Add an authentication selector for SQL password, Windows/Kerberos, and Entra user through Azure CLI.
For Entra, show the expected username, tenant ID, object ID, and optional CLI executable path. Hide only the
password field using the host's planned `setPasswordFieldHidden(boolean)` extension. The username stays visible.
Do not clear existing identity fields when an editor loads. Switching to SQL/integrated auth clears the Entra-only
mode/source/identity/path fields before saving, with the user's active form choice controlling the change.
Switching to Entra requires the incompatible password/integrated fields to be cleared intentionally.

The companion host owns profile migration. It installs the new package first, then maps existing `sqlserver`
and `sqlserver-entra` connections in the imported fork profile to `sqlserver-plus`, preserving IDs, targets, and
their chosen auth mode. The official app/profile and old plugin packages remain available for rollback.
Do not rename an installed directory behind an active process or modify live connection files from the build script.

Keep `.tabularium`, `Cargo.toml`, and `explain/package.json` versions aligned, including required lockfile updates.
The initial fork version is `1.0.0-beta.3+plus.1`, with a manifest runtime floor of `0.27.0`.
The UI must also check for `setPasswordFieldHidden`; a version number alone cannot prove that the host has it.
If it is missing, show a compatible-host requirement instead of failing silently or clearing credentials.
The backend's existing RPCs remain usable by compatible hosts; publish the supported host/package pair explicitly.

### Cancellation protocol

Implement the host plan's existing JSON-RPC cancel notification without creating a driver-owned MCP API:

```json
{"jsonrpc":"2.0","method":"cancel","params":{"id":42}}
```

The numeric ID identifies an in-flight `execute_query` or supported query-batch request in this plugin process.
Do not accept arbitrary server session IDs. Notifications have no top-level ID and receive no response.
Late, duplicate, unknown, or completed IDs are no-ops. A malformed notification must not crash the process
or produce an unsolicited stdout response.

Keep the stdin reader responsive and route cancel notifications directly to an in-flight registry before the
ordinary bounded worker queue. Do not enqueue cancellation behind four long-running queries.
Register a request-scoped cancellation handle before pool acquisition so a queued checkout can also be cancelled.
Use a generation/guard to remove only the matching execution entry on every return path.

Create a parent `mssql_tds::core::CancelHandle` for each execution and pass its child handle into the actual
TDS execution path. The registry owns the one-shot parent. Sending cancellation must reach the TDS ATTENTION
path and completion handling; dropping a host future alone is not a successful database cancellation.
Propagate the confirmed cancellation outcome through the original query response using the exact error code
and data shape defined in the host plan's cancellation contract. Distinguish SQL failure, deadline expiry,
and confirmed cancellation. Preserve existing non-cancellation error text/code behavior.

After cancellation, either complete the driver's documented drain/reset sequence or discard that connection.
Do not return a client with an open result stream or unknown transaction state to the pool.
Prefer discarding an uncertain cancelled connection over running recovery SQL on a damaged stream.
Never cancel neighboring requests, terminate another user's session, or automatically replay SQL.

Touch `src/main.rs`, `src/rpc.rs`, `src/handlers/query.rs`, `src/driver/mod.rs`, and a focused cancellation module.
Use the currently pinned `mssql-tiberius-bridge`/`mssql-tds-preview` APIs.
Dependency upgrades are not part of this plan.
Preserve the bridge's native TLS and integrated-auth support.

### Results and history boundary

The driver returns query results to the host; the host owns the retained snapshot, query ID, paging tool,
expiry, release, and history. Preserve the existing `QueryResult`, positional rows, column order,
additional-result metadata, and explicit `truncated` flag.

Retain `MAX_RESULT_ROWS = 10_000` across result sets for the first release and make truncation unambiguous.
The host's managed-query path executes once and pages the returned snapshot. It must not use the driver's
existing `page`/OFFSET path to claim that it is reading the same saved result.
Keep legacy SQL pagination available for existing GUI and MCP consumers. Preserve explicit TOP/LIMIT-like
clauses, CTE classification, and the existing restriction on statements that can be paginated.
An unordered query's synthetic `ORDER BY (SELECT NULL)` cannot guarantee stable page boundaries; document this
for the legacy path rather than presenting it as snapshot paging.

Do not add a persistent result or query-history store in the plugin. Do not claim it can retrieve rows beyond
the retained 10,000-row limit. A future streaming/cursor contract is a separate, coordinated host/driver change.

### Preserve plugin features and packaging

Maintain catalog discovery, tables/views/routines/triggers, comments, query templates, BLOB conversion,
empty-result column metadata, multi-result conversion, and current SQL dialect/type mappings.
Keep `.tabularium` data types synchronized with `src/driver/types.rs`.
Preserve the raw `sqlserver-showplan-xml` EXPLAIN payload, plugin-owned parser registration,
`explain/dist/index.iife.js`, and the required UI bundle. Existing mutation APIs remain in the driver,
but this work neither adds mutation capabilities nor tests them against live databases.

Fix packaging/install paths to the host's kind-scoped `plugins/drivers/sqlserver-plus` directory.
Make the destination profile explicit; the default fork installation targets the Extended profile, not the
official profile. Provide a dry-run/manifest-only preflight and a package hash check before installation.
Keep all UI/parser assets relative to the package and avoid machine-specific executable paths in shipped files.

The release workflow must publish to this fork's GitHub releases. Do not publish to the upstream npm scope
using inherited release jobs. Bundle the existing EXPLAIN parser locally; publishing a new npm package or
configuring registry/signing credentials is outside this release. Review inherited push, schedule, and tag jobs
before enabling fork releases. Do not automatically run the upstream database-seeding test job.

## Execution slices

Main ownership retains the coordinator's current model and effort. Backend implementation uses the coding
host's pinned worker role/model/effort. New UI structure uses its pinned designer route.
The coordinator owns all cross-repository contracts and final integration decisions.

| Slice / owner | Result and allowed paths | Protected paths and prerequisites | Shared resources / parallel condition | Completion evidence |
|---|---|---|---|---|
| D1 / implementation worker | Port Entra module, pool wiring/identity, and auth tests; `src/driver/{azure_cli_auth.rs,mod.rs,pool.rs,pool/tests.rs}`, `src/pool_manager.rs` | Preserve newer introspection/trigger fixes, actual credentials, and unrelated handlers; use the frozen host contract | Parallel with host H1; own driver Rust build directory | Auth and pool fixtures, upstream-delta review, locked backend build |
| D2 / implementation worker | Request registry, responsive cancel dispatch, TDS cancellation, safe connection discard; `src/main.rs`, `src/rpc.rs`, query handlers/driver and focused tests | D1 accepted; preserve JSON-RPC response shape and non-query handlers | Serial with D1 because runtime files/build output overlap; may run alongside host H3 | Deterministic saturation, late/duplicate/race, error-redaction, and handle-lifetime checks |
| D3 / designer route | Auth UI and component tests under `ui/` | D1 accepted and host H3 UI contract available; backend, root manifest, and packaging files protected | Can proceed while D2 edits backend, using separate frontend outputs | UI type/component tests and reviewed identity/credential-field behavior |
| D4 / coordinator | Package identity, versions, UI/parser bundles, fork install/release recipes, and integrated acceptance; root manifests/locks, build/release metadata and docs | D2/D3 and host H2/H3 accepted; UI/backend implementations accepted before integration edits | Serial shared installation/host integration; owns package metadata and final outputs | Complete package/host compatibility, approved SELECT-only integration, reproducibility, artifact hash, migration/rollback evidence |

The host can use a fake driver for H2 before D2 is finished; real cancellation acceptance waits for both sides.
D4 and host H4 share one coordinator and one integration run. Reuse its results in both repositories.
Do not build or package while a worker is still editing the relevant inputs.

## Verification and completion

| Acceptance criterion | Required evidence |
|---|---|
| P1: existing authentication survives | Ported pure tests cover accepted tokens, bad shape, wrong identity/audience, expiry, empty config, incompatible password/integrated mode, TLS downgrade, and CLI failure without secret output |
| P2: renewal is connected to the pool | Fake CLI/process and connector tests prove acquisition on each new physical connection, bounded timeouts, child cleanup, account-change rejection, and unchanged SQL/integrated paths |
| P3: native cancellation is real | A blocked driver fixture proves the cancel notification bypasses the queue; approved nonproduction SELECT execution confirms server termination and safe reuse/discard |
| P4: cancellation is scoped | Concurrent requests, cancellation before checkout, completion races, duplicate/late IDs, and generation cleanup cannot affect another request |
| P5: results remain compatible | Empty/typed/multi-result and 10,000/10,001-row fixture cases preserve shapes and explicit truncation; snapshot pages trigger no second query |
| P6: upstream fixes remain | Existing introspection, trigger, parser, type-mapping, TLS, connection-string, and conformance fixtures pass |
| P7: package is complete | Both UI and EXPLAIN bundles load under the target host; manifest IDs/versions/floors and install paths match; no secrets or machine-specific paths are shipped |
| P8: deployment is reproducible | Clean locked build, fork-owned artifact/hash, migration of six Entra plus two SQL-password connections, fresh `SELECT 1`, and rollback evidence |

Use focused Rust tests during D1/D2, then `cargo test --locked --bins --test conformance`,
`cargo clippy --locked --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.
Build the release binary with `cargo build --locked --release --bin sqlserver-plugin`.
Use the versions and frozen package lockfiles in `ui/` and `explain/` for their type, test, and build scripts.
`just build`/`just release` currently build those assets, but their install steps must be made reproducible
before using them as a release gate. Do not treat a backend-only build as a complete plugin package.

No test in this work may create, seed, mutate, or drop a live database. Do not run `just seed-sqlserver`,
`tests/live_db.rs`, or inherited CI jobs that perform SQL writes. Use recorded protocol fixtures for mutation
and error cases. Use only explicit SELECT statements for authorized existing-connection checks and an approved
nonproduction owned SELECT for cancellation. TDS ATTENTION is the cancellation mechanism; do not issue KILL.

Keep tests independent of real Azure CLI credentials by using controlled process/connector fixtures.
Real Entra smoke tests are opt-in, use the already authorized identity, and never print/store tokens.
Token validation does not justify adding a second account or granting new database permissions.

Stop on a source provenance mismatch, unresolved host contract, authentication regression, leaked secret,
uncertain cancellation/connection state, package mismatch, or failed required check. Do not weaken TLS,
substitute identities, recycle an uncertain client, or replay SQL to make a check pass.
Reuse earlier evidence only for unchanged inputs. Implementation is complete when P1–P8 and the host's integrated
criteria pass; upstream PR acceptance and optional streaming, additional auth modes, or npm publication are
not completion gates.
