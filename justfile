set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Build both required JavaScript bundles before compiling the driver.
build: build-ui build-explain
    cargo build --locked

# Build the binaries that are used by the platform release packages.
release: build-ui build-explain
    cargo build --locked --release

build-ui:
    cd ui && npx --yes npm@11.6.2 ci --ignore-scripts --no-audit --no-fund
    cd ui && npm run build

build-explain:
    npx --yes pnpm@11.23.0 --dir explain install --frozen-lockfile
    npx --yes pnpm@11.23.0 --dir explain build

test-explain:
    npx --yes pnpm@11.23.0 --dir explain install --frozen-lockfile
    npx --yes pnpm@11.23.0 --dir explain typecheck
    npx --yes pnpm@11.23.0 --dir explain test

test:
    cargo test --locked --bins --test conformance

# Testcontainers owns its SQL Server container, random port, and credentials.
test-containers:
    cargo build --locked --bin sqlserver-plugin
    npm --prefix tests/containers ci --ignore-scripts --no-audit --no-fund
    npm --prefix tests/containers test

test-packaging:
    node --test tests/packaging/*.test.mjs

manifest-check:
    node scripts/preflight.mjs --root . --asset-root .

repl:
    cargo run --locked --bin test_plugin

lint:
    cargo clippy --locked --all-targets -- -D warnings

fmt:
    cargo fmt --all
