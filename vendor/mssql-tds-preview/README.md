# mssql-tds-preview

**Publishable fork of [`microsoft/mssql-rs`](https://github.com/microsoft/mssql-rs) for crates.io distribution.**

This crate is a **temporary, unmodified fork** of Microsoft's `mssql-tds` — the official Rust implementation of the TDS (Tabular Data Stream) protocol for SQL Server. No code changes are made; only the package name is different to allow publishing to crates.io.

## ⚠️ This crate will be yanked

Once Microsoft publishes the official `mssql-tds` crate to crates.io, this preview fork will be **yanked** and all users should migrate to the official crate:

```toml
# Before (preview fork)
mssql-tds-preview = "=0.1.0-preview.1"

# After (official — when available)
mssql-tds = "0.1"
```

The API is identical — `use mssql_tds::...` imports work the same with both crates. Migration requires only a `Cargo.toml` change.

## Why does this exist?

- `mssql-tds` is not yet published to crates.io
- Crates that depend on it (like [`mssql-tiberius-bridge`](https://github.com/saurabh500/mssql-tiberius-bridge)) cannot be published to crates.io with a git dependency
- This fork bridges the gap until the official publish

## Versioning

Versions follow the scheme `{upstream_version}-preview.{N}`:

- `0.1.0-preview.1` — based on upstream `0.1.0`, first preview release
- `0.1.0-preview.2` — same upstream version, updated preview
- `0.2.0-preview.1` — based on upstream `0.2.0`

Pin exact versions in your `Cargo.toml`:

```toml
mssql-tds-preview = "=0.1.0-preview.1"
```

## Sync with upstream

The `fork` branch is automatically synced with `microsoft/mssql-rs` main via a daily GitHub Action. The `main` branch is a clean mirror of upstream.

## License

MIT — same as the upstream `microsoft/mssql-rs`.

## Links

- **Upstream:** https://github.com/microsoft/mssql-rs
- **This fork:** https://github.com/saurabh500/mssql-rs
- **Bridge crate:** https://github.com/saurabh500/mssql-tiberius-bridge
