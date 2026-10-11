# Build and release SQL Server Plus

This guide covers local verification, deterministic platform packages, and the
fork's GitHub release workflow. It does not publish the EXPLAIN parser to npm
or the manifest to the upstream registry.

## Toolchain

- Stable Rust from `rust-toolchain.toml`
- Node.js `24.21.0` and npm `11.6.2`
- pnpm `11.23.0`
- Docker for the isolated SQL Server Testcontainers suite
- `just` for the common local commands

The UI extension and EXPLAIN parser are required runtime files. `just build`
and `just release` install their dependencies from the lockfiles and build both
bundles before compiling the Rust driver:

```sh
just build
just release
just manifest-check
```

## Local checks

Run the deterministic unit and packaging checks without a database, then the
separate Testcontainers suite when Docker is available:

```sh
just test
just test-explain
just test-packaging
just lint
just fmt
just test-containers
```

`just test-containers` builds the plugin with `--locked`, starts the
digest-pinned SQL Server image on a random port using a generated password,
and removes its disposable container after the suite. It never reads saved
connections or contacts an existing SQL Server. See
[`../tests/containers/README.md`](../tests/containers/README.md) for its
coverage and platform requirements.

## Build a platform ZIP

First build the UI and EXPLAIN bundles, then build the matching Rust target.
The release workflow keeps the existing five-platform matrix: Linux x64,
Linux arm64, macOS x64, macOS arm64, and Windows x64. Linux arm64 uses
`cross`; the other targets use Cargo directly.

The package script validates the manifest and requires the executable,
`.tabularium`, `ui/dist/index.js`, `explain/dist/index.iife.js`, `LICENSE`,
`README.md`, and the vendored TDS bridge MIT license. It sorts ZIP paths by
UTF-8 bytes, fixes timestamps and file modes, and emits a `<archive>.sha256`
sidecar. Missing or non-regular files fail the package step.

For example, after a Linux x64 release build:

```sh
node scripts/package.mjs \
  --binary target/release/sqlserver-plugin \
  --platform linux-x64 \
  --output dist/sqlserver-plus-linux-x64.zip
```

For Windows, pass `target/release/sqlserver-plugin.exe` and
`--platform win-x64`. The same script accepts the other three platform labels.
No local release binary or release ZIP is produced as part of repository
validation; GitHub Actions performs those platform builds when a release tag
is pushed.

## Verify and install an archive

The installer requires an archive, its SHA-256 digest or sidecar, and an
absolute `--profile-data-dir`. It does not guess between Tabularis and
Tabularis Extended profiles. Without `--apply`, it validates the checksum,
manifest, ZIP entries, referenced bundles, and target path, then prints the
planned destination without changing files:

```sh
node scripts/install.mjs \
  --archive dist/sqlserver-plus-linux-x64.zip \
  --sha256 dist/sqlserver-plus-linux-x64.zip.sha256 \
  --profile-data-dir "/absolute/path/to/Tabularis Extended data"
```

Review the target before repeating the command with `--apply`. The only target
is `<profile-data-dir>/plugins/drivers/sqlserver-plus`. The installer rejects
ZIP traversal, symbolic links, duplicate paths, unsupported ZIP formats,
oversized entries, checksum mismatches, and missing manifest assets. It
extracts to a temporary sibling directory, moves the current SQL Server Plus
directory to a backup, and then renames the staged package into place. If
activation fails, it renames the backup back. On success, it keeps the backup
and prints its path for an explicit rollback. Before applying or restoring,
close Tabularis Extended and any Tabularis MCP clients using that profile. For
a rollback, move the current `sqlserver-plus` directory aside and rename the
printed backup directory to `<profile-data-dir>/plugins/drivers/sqlserver-plus`.
Other driver directories and profile files are not used as fallback targets
or modified.

On macOS or Linux, use the exact path printed by the installer:

```sh
TARGET="/absolute/path/to/Tabularis Extended data/plugins/drivers/sqlserver-plus"
BACKUP="/absolute/path/printed/by/the/installer"
mv "$TARGET" "${TARGET}.before-rollback"
mv "$BACKUP" "$TARGET"
```

Keep the moved-aside current directory until the restored plugin is verified.

The release contains unsigned ZIP files plus SHA-256 sidecars. The checksum
detects accidental corruption when compared with the sidecar; it does not
provide a digital signature or attest who produced the files.

## Release checks

`.tabularium`, `Cargo.toml`, `ui/package.json`, and `explain/package.json` must
all use `1.0.0-beta.3+plus.1` for this release. The manifest uses ID
`sqlserver-plus`, display name `SQL Server Plus`, minimum runtime `0.27.0`,
and SQL dialect `mssql`. `node scripts/preflight.mjs` checks those identities,
the package privacy settings, the EXPLAIN Vitest pin, and the required local
bundle references.

The workflow accepts only `plus-v<exact-version>` tags in
`hei5enbug/tabularis-sqlserver-plus`. It repeats the package-version check,
builds all five native targets, packages them with the same deterministic
script, and publishes the ZIPs, SHA-256 sidecars, and `.tabularium` file to a
GitHub release. It has no npm publish job and no upstream repository target.
