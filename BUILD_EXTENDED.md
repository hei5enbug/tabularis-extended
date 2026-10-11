# Build and install Tabularis Extended

This guide builds the fork and its pinned SQL Server Plus companion from source.
It keeps the original application and profile available for rollback.

## Toolchain and checks

Use Node 24.21.0, the host's pnpm 10.30.3, a Rust toolchain compatible with the lockfile, and Tauri build tools.
SQL Server Plus uses npm 11.6.2 for its UI lockfile and pnpm 11.23.0 for its EXPLAIN package.
Docker is required only for the owned database integration tests.

Run from the host repository:

```sh
corepack pnpm install --frozen-lockfile
corepack pnpm typecheck
corepack pnpm lint
corepack pnpm build
corepack pnpm --filter @tabularis/plugin-api check:sync
corepack pnpm test:release
corepack pnpm exec vitest run
corepack pnpm test:rust
```

The Rust test runner uses temporary profile and plugin directories and removes them after the run.
On macOS it also uses `TMPDIR=/private/tmp`: inherited filesystem tests reject symlink ancestors such as `/var`.
The application itself supports the normal macOS temporary directory.

The Node release-tool tests and owned container tests are separate from the jsdom frontend suite.
The [container guide](tests/containers/README.md) describes their artifact inputs and isolation boundaries.

## Build and pin SQL Server Plus

Check out the companion repository at the intended source revision.
Its complete source fingerprint must match `sqlserverPlus.sourceSha256` in `integration/companions.json`.
The fingerprint includes tracked and unignored files, including new source files before their first commit.
Build outputs are ignored. Committing the same files does not change the fingerprint.

Set `SQLSERVER_PLUS_ROOT` to that checkout's absolute path and `RELEASE_WORK` to an empty temporary directory.
Run the companion's locked build and ZIP packaging commands from its
[build guide](https://github.com/hei5enbug/tabularis-sqlserver-plus/blob/main/docs/build-release.md).
Do not copy an installed plugin binary into the source build.

From the host repository, bind the resulting platform archive before compiling the host:

```sh
node scripts/companion-source.mjs verify \
  --root "$SQLSERVER_PLUS_ROOT" \
  --expected "$(node -p 'JSON.parse(require("fs").readFileSync("integration/companions.json")).sqlserverPlus.sourceSha256')"
node scripts/pin-companion.mjs \
  --source-root "$SQLSERVER_PLUS_ROOT" \
  --archive "$RELEASE_WORK/sqlserver-plus-darwin-arm64.zip" \
  --platform darwin-arm64 \
  --lock "$PWD/integration/companions.json"
```

The pin records the archive checksum and each installed file's checksum.
The host compiles those values into its profile importer.
It changes imported SQL Server driver IDs to `sqlserver-plus` only when the installed package matches that pin.
A missing platform pin or changed binary, manifest, UI, or parser leaves the original driver ID intact.

Changing companion source requires reviewing the changes, running its checks, and updating the source pin explicitly.
The pinning command refuses a source mismatch. It does not silently accept a newer companion version.

## Build native artifacts

For macOS ARM64, build and stage an unsigned disk image:

```sh
corepack pnpm tauri build --no-sign --ci --bundles dmg -- --locked
node scripts/stage-extended-release.mjs \
  --root "$PWD" \
  --bundle-root "$PWD/src-tauri/target/release/bundle" \
  --output "$RELEASE_WORK/tabularis-extended-release" \
  --companion-archive "$RELEASE_WORK/sqlserver-plus-darwin-arm64.zip" \
  --platform darwin-arm64
```

Use a clean bundle directory. Staging rejects multiple installers and an existing output directory.
It writes the native installer, companion ZIP, their `.sha256` sidecars, `companions.json`, `release.json`, and `SHA256SUMS`.
The receipt records the actual source fingerprint and whether the source tree contained uncommitted changes.
The host's two existing internal agent-instruction links are fingerprinted as links with their exact targets.
Other source links and changed targets are rejected. Companion source continues to reject all symbolic links.
Builds with the same dependency locks are repeatable source builds; native installer bytes are not claimed to be
identical across toolchains or operating-system images.

The `Extended verified artifacts` workflow runs checks and owned SQL Server/PostgreSQL tests before packaging.
It builds macOS ARM64 DMG and Linux x64 DEB artifacts.
Its `companion_ref` input must be a full commit containing exactly the pinned companion source.
Commit and push that companion source before running the workflow.
The workflow uploads run artifacts. It does not create a release, publish npm packages, configure signing,
change installed applications, or enable an update feed.

## Install and import a profile

Verify `SHA256SUMS` before installation. Keep the previous application and its profile.
The fork has the following independent identity:

| Item | Value |
|---|---|
| Application | `Tabularis Extended` |
| Executable | `tabularis-extended` |
| Bundle identifier | `io.github.hei5enbug.tabularis.extended` |
| Profile and keychain service | `tabularis-extended` |
| MCP server name | `tabularis-extended` |
| OS deep-link scheme | `tabularis-extended` |

Close the fork and its MCP processes before installing the companion package.
Use the SQL Server Plus installer with an explicit fork data directory and the supplied SHA-256 sidecar.
The installer defaults to a dry run. Its `--apply` option stages and swaps only `plugins/drivers/sqlserver-plus`.
It retains an existing package as a named backup.

Close the original application and its MCP processes so the source does not change during the copy.
Open the fork's Storage settings and review the original and destination paths before choosing Import.
The importer accepts an empty destination or the known empty first-run files.
It refuses an existing populated profile. It preserves a private source backup and reports its path.
It copies settings and plugin packages and transfers available credentials to the fork keychain.
Missing credentials are listed for manual entry. Restart the fork after a successful import.

The default profile paths follow the platform's `tabularis-extended` config/data directories.
`TABULARIS_EXTENDED_DATA_DIR` selects an explicit fork profile.
`TABULARIS_EXTENDED_PLUGIN_DIR` accepts an absolute fork plugin directory.
`TABULARIS_DATA_DIR` is used only to resolve the original profile during an explicit import.
Do not point both applications at the same profile.

An editor opened before another process changes a connection cannot overwrite that newer revision.
It keeps the draft and offers Reload latest. Reloading deliberately replaces that draft.

After verifying the imported profile, register the fork executable in the desired MCP client.
The Settings action registers `tabularis-extended` and preserves the original `tabularis` registration.
No migration or build command automatically changes MCP client configuration.

## Roll back

Close Tabularis Extended and its MCP processes.
Start the original application with its original profile and executable.
Its source profile and keychain entries remain available because import does not delete them.
Remove or disable only the fork's MCP registration if it was added.

For a companion-only rollback, close the fork, move the new `plugins/drivers/sqlserver-plus` package aside,
and restore the backup path printed by the installer.
For a fork-profile rollback, preserve the current fork profile first, then restore the appropriate import backup.
Do not overwrite a live profile or delete the original credentials.

## Verification limits

The 2026-10-11 local checks passed on macOS ARM64 with Node 24.21.0 and pnpm 10.30.3:

| Check | Result |
|---|---|
| Complete Rust library suite | 1,573 passed; 6 existing ignored tests |
| Complete frontend suite | 333 files; 5,231 passed |
| Owned host/database integration | 17 passed against SQL Server 2022 and PostgreSQL 17/PostGIS 3.5 |
| Release tools | 14 passed, including source links, identity, companion integrity, and checksum sidecars |
| Typecheck, lint, production frontend build, plugin API sync | Passed |

The isolated host integration run verified server-side termination without administrative kill commands,
neighboring-query survival, stable pages, restart history, permission changes, and approval-policy changes.
The profile importer uses synthetic source profiles and credential stores in its tests.
These results do not assert that the user's existing 14 connections were migrated or retested.

Owned containers verify SQL Server execution/cancellation, PostgreSQL execution/cancellation, PostGIS spatial queries,
and the Cosmos NoSQL emulator paths covered by their suites.
They do not verify Azure account identity, Entra service acceptance, production RBAC, or existing private endpoints.
The Cosmos emulator is not evidence of cloud RU billing or authorization behavior.
Real-profile import and cloud connection checks remain explicit deployment actions.
