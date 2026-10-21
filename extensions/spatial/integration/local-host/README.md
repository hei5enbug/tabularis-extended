# Local app with the official PostgreSQL driver

This patch builds a local Tabularis app from official `v0.26.0` source.
It removes the deprecated `postgres` driver's registration from both GUI and standalone MCP startup.
MySQL and SQLite remain built in. PostgreSQL uses the official `postgresql` plugin.
The PostgreSQL SQL dialect and geometry helpers remain available to the spatial UI extension.

The patch also corrects foreign connection imports: PostgreSQL maps to `postgresql`, and SQL Server maps
to `sqlserver`. `mssql` remains the SQL Server SQL dialect; it is not the official plugin's driver ID.
These changes prevent new imports from recreating unavailable driver references.

`pins.json` records the exact upstream revision, local version, and patch SHA256.
This patch is separate from `integration/patches/host.patch` and does not add its spatial service APIs.

## Build

Use a clean checkout at the revision in `pins.json`, Rust, macOS build tools, Node 24, and pnpm 10.30.3.
The patch produces version `0.26.0+local.1`; build metadata preserves compatibility with plugins requiring
stable `0.26.0`.

```sh
git clone https://github.com/TabularisDB/tabularis.git ../tabularis-local
git -C ../tabularis-local checkout 5d7e27408907c0faafc8cc6b940725f4a1258f42
git -C ../tabularis-local apply --check "$PWD/integration/local-host/plugin-drivers-only.patch"
git -C ../tabularis-local apply "$PWD/integration/local-host/plugin-drivers-only.patch"
pnpm --dir ../tabularis-local install --frozen-lockfile --ignore-scripts
pnpm --dir ../tabularis-local build:explain
pnpm --dir ../tabularis-local build
pnpm --dir ../tabularis-local exec tauri build --bundles app --ci \
  --config '{"build":{"beforeBuildCommand":""},"bundle":{"createUpdaterArtifacts":false}}' -- --locked
codesign --force --deep --sign - ../tabularis-local/src-tauri/target/release/bundle/macos/tabularis.app
codesign --verify --deep --strict ../tabularis-local/src-tauri/target/release/bundle/macos/tabularis.app
```

The macOS bundle is `../tabularis-local/src-tauri/target/release/bundle/macos/tabularis.app`.
It is a local build, not an official notarized release. Preserve the installed app and settings before
replacing them. Do not remove quarantine or disable Gatekeeper to install it.

## Verify without database access

The local binary adds `--list-drivers`. It reads configured plugin manifests and lists their IDs,
names, versions, and built-in status. It does not open connections, resolve connection credentials,
run SQL, or perform the MCP connection-file migration.

```sh
../tabularis-local/src-tauri/target/release/tabularis --list-drivers
```

The result must omit `postgres` and include `postgresql` when that official plugin is installed and enabled.
For SQL Server, it must include `sqlserver`. Each saved connection's `params.driver` must match a loaded ID.
Retain connection IDs and all other fields when correcting an old `mssql` reference to `sqlserver`.
Back up connection metadata outside this repository; do not publish credentials or private endpoint details.

Database connection tests, schema discovery, queries, and table-map actions are outside this verification.
Driver availability does not establish that a server is reachable or authentication is configured.
The DataGrip importer still does not transfer passwords or Azure authentication-provider settings.

## Updates and rollback

An official app update may restore the deprecated built-in driver because it replaces this local build.
Review and rebase the patch against the chosen official revision before building a replacement.
Do not lower plugin runtime-version requirements to bypass compatibility checks.

To roll back, close the local app and restore the app bundle saved before installation.
The official app supports `postgresql` and `sqlserver`, so those corrected connection IDs can remain.
Restore the private settings backup only when its newer changes are no longer needed.
