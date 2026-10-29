set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Package identity the Tabularis Extended host verifies before it remaps connections.
plugin_id := "sqlserver-plus"
binary := "sqlserver-plugin"

# Destination profile. The default is the Extended fork's app directory, never
# the official `tabularis` profile; set TABULARIS_APP_DIR to choose another one
# on purpose. TABULARIS_PLUGIN_DIR (absolute) replaces the whole plugins root,
# exactly as in the host (src-tauri/src/paths.rs).
app_dir := env("TABULARIS_APP_DIR", "tabularis-extended")
plugins_root := env("TABULARIS_PLUGIN_DIR", if os() == "macos" { env("HOME") + "/Library/Application Support/" + app_dir + "/plugins" } else if os() == "windows" { env("APPDATA") + "/" + app_dir + "/plugins" } else { env("XDG_DATA_HOME", env("HOME") + "/.local/share") + "/" + app_dir + "/plugins" })
dest := plugins_root + "/drivers/" + plugin_id

# Archive platform label (<os>-<arch>) and executable suffix of the host build.
os_label := if os() == "macos" { "darwin" } else if os() == "windows" { "win" } else { os() }
arch_label := if arch() == "x86_64" { "x64" } else if arch() == "aarch64" { "arm64" } else { arch() }
platform := os_label + "-" + arch_label
exe_suffix := if os() == "windows" { ".exe" } else { "" }

# Run SQL Server 2022 via Docker (accept the EULA, set a strong SA password).
run-sqlserver:
    docker run -d --name sqlserver-dev -p 1433:1433 \
        -e "ACCEPT_EULA=Y" -e "MSSQL_SA_PASSWORD=Str0ng!Passw0rd" \
        mcr.microsoft.com/mssql/server:2022-latest

# Stop and remove the local SQL Server container.
stop-sqlserver:
    docker rm -f sqlserver-dev

# Seed a test database into the local SQL Server container. CREATE DATABASE
# must finish in its own batch before sqlcmd selects the new database.
seed-sqlserver:
    docker exec sqlserver-dev /opt/mssql-tools18/bin/sqlcmd \
        -S localhost -U sa -P "Str0ng!Passw0rd" -C -Q \
        "IF DB_ID('tabularis_test') IS NULL CREATE DATABASE tabularis_test;"
    docker exec sqlserver-dev /opt/mssql-tools18/bin/sqlcmd \
        -S localhost -U sa -P "Str0ng!Passw0rd" -C -d tabularis_test -Q \
        "IF OBJECT_ID('dbo.users') IS NULL BEGIN \
           CREATE TABLE dbo.users (id INT IDENTITY(1,1) PRIMARY KEY, name NVARCHAR(100) NOT NULL, email NVARCHAR(255) NOT NULL); \
           INSERT INTO dbo.users (name, email) VALUES (N'Alice', N'alice@example.com'), (N'Bob', N'bob@example.com'); \
         END"

# ---------------------------------------------------------------------------
# Cross-platform recipes (only shell-agnostic tooling: cargo and pnpm).
# ---------------------------------------------------------------------------

# Build the plugin binary and its JavaScript artifacts.
build: build-ui build-explain
    cargo build --locked

# Build for release (what a package contains).
release: build-ui build-explain
    cargo build --locked --release

# Each package manager version comes from the nearest package.json, so these
# recipes run inside the package directory rather than using `pnpm --dir`.

# Install the connection-modal UI from its frozen lockfile and bundle it.
[working-directory: 'ui']
build-ui:
    pnpm install --frozen-lockfile
    pnpm build

[working-directory: 'ui']
test-ui:
    pnpm install --frozen-lockfile
    pnpm typecheck
    pnpm test

# Build the browser-safe SQL Server SHOWPLAN parser bundle.
[working-directory: 'explain']
build-explain:
    pnpm install --frozen-lockfile
    pnpm build

[working-directory: 'explain']
test-explain:
    pnpm install --frozen-lockfile
    pnpm typecheck
    pnpm test

# This crate is binary-only, so --lib would fail; selecting conformance
# explicitly keeps tests/live_db.rs out of the default run.
#
# Run unit tests and recorded host-model conformance.
test:
    cargo test --locked --bins --test conformance

# Launch the local REPL that simulates Tabularis JSON-RPC calls over stdio.
repl:
    cargo run --bin test_plugin

# Run clippy on the workspace.
lint:
    cargo clippy --locked --all-targets -- -D warnings

# Format the codebase.
fmt:
    cargo fmt --all

# ---------------------------------------------------------------------------
# Package identity, packaging and installation.
#
# Host source: src-tauri/src/paths.rs resolves the plugins root as
# <app data dir of "tabularis-extended">/plugins (or TABULARIS_PLUGIN_DIR), and
# src-tauri/src/driver_remap.rs::verified_driver_package accepts a package
# under plugins/drivers/<id> only when the manifest id equals the requested id,
# min_runtime_version fits the host, and the executable is a regular file
# inside the package. The default roots are
# ${XDG_DATA_HOME:-$HOME/.local/share}/tabularis-extended/plugins on Linux,
# $HOME/Library/Application Support/tabularis-extended/plugins on macOS, and
# %APPDATA%\tabularis-extended\plugins on Windows.
#
# No recipe edits connection files or renames a directory in use. Quit
# Tabularis Extended before installing over an existing package.
# ---------------------------------------------------------------------------

# Packaging logic lives in scripts/*.mjs (Node built-ins only) and is covered by
# tests/packaging, so `just test-packaging` exercises the code these recipes run.

# Run the packaging script tests (archive, preflight, package and install).
test-packaging:
    node --test "tests/packaging/*.test.mjs"

# Manifest-only preflight (no build, no writes): package identity, aligned
# versions and the plugin-level settings contract.
#
# Verify package identity, aligned versions and plugin settings without building.
preflight:
    node scripts/preflight.mjs --metadata-only

# The archive root holds exactly what the host loads: the executable,
# .tabularium, README.md, assets, ui/dist/index.js and explain/dist/index.iife.js,
# plus the LICENSE that Apache-2.0 requires with every binary copy. An existing
# archive of the same name is replaced.
#
# Build the release package for this host into OUT and write its SHA-256 file.
package out="dist": release preflight
    node scripts/package.mjs --replace --binary "target/release/{{ binary }}{{ exe_suffix }}" --platform "{{ platform }}" --output "{{ out }}/{{ plugin_id }}-{{ platform }}.zip"

# Check a packaged archive against its SHA-256 file and its embedded manifest.
verify-package archive:
    node scripts/install.mjs --verify-only --archive "{{ archive }}" --sha256 "{{ archive }}.sha256"

# Show the destination and run the manifest preflight without touching the plugins directory.
install-dry-run: preflight
    @echo "Would install {{ plugin_id }} into {{ dest }}"
    @echo "Profile directory: {{ app_dir }}"

# Verify a packaged archive and report where it would be installed, without writing anything.
install-package-dry-run archive:
    node scripts/install.mjs --archive "{{ archive }}" --sha256 "{{ archive }}.sha256" --plugins-root "{{ plugins_root }}"

# The previous package is kept as a hidden backup next to it and restored if
# the replacement fails. The plugins root's parent (the profile directory) must
# already exist, so a mistyped TABULARIS_APP_DIR never creates a profile.
#
# Verify a packaged archive, stage it, then replace the fork's plugin directory.
install-package archive:
    node scripts/install.mjs --apply --archive "{{ archive }}" --sha256 "{{ archive }}.sha256" --plugins-root "{{ plugins_root }}"
    @echo "Restart Tabularis Extended (or toggle the plugin in Settings) to pick up changes."

# Build a debug binary and copy it, the manifest and the bundles into the fork's plugin folder.
[unix]
dev-install: build preflight
    mkdir -p "{{ dest }}/ui/dist" "{{ dest }}/explain/dist"
    cp "target/debug/{{ binary }}" "{{ dest }}/"
    cp .tabularium "{{ dest }}/"
    cp ui/dist/index.js "{{ dest }}/ui/dist/"
    cp explain/dist/index.iife.js "{{ dest }}/explain/dist/"
    @echo "Installed to {{ dest }}"
    @echo "Restart Tabularis Extended (or toggle the plugin in Settings) to pick up changes."

# Each recipe line runs in a fresh shell, so this must be one logical command.
[windows]
dev-install: build preflight
    $dest = "{{ dest }}"; \
    New-Item -ItemType Directory -Force -Path (Join-Path $dest "ui\dist"), (Join-Path $dest "explain\dist") | Out-Null; \
    Copy-Item "target\debug\{{ binary }}.exe" $dest; \
    Copy-Item ".tabularium" $dest; \
    Copy-Item "ui\dist\index.js" (Join-Path $dest "ui\dist"); \
    Copy-Item "explain\dist\index.iife.js" (Join-Path $dest "explain\dist"); \
    Write-Host "Installed to $dest"; \
    Write-Host "Restart Tabularis Extended (or toggle the plugin in Settings) to pick up changes."

# Remove the installed package from the same host-defined directory.
[unix]
uninstall:
    rm -rf "{{ dest }}"

[windows]
uninstall:
    if (Test-Path "{{ dest }}") { Remove-Item -Recurse -Force "{{ dest }}" }
