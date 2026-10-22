# Tabularis Extended

Tabularis Extended is a personal fork of [Tabularis](https://github.com/TabularisDB/tabularis), based on release
0.27.0. It runs beside the official app and gives its MCP server the database features that coding agents expect
from a database IDE. The official app is not modified, and its profile and keychain entries are never changed.

Everything lives in this one repository: the desktop host and four extensions.

| Path | What it is |
|---|---|
| `src`, `src-tauri` | The desktop app (React and Tauri) and its MCP server |
| `extensions/postgresql` | PostgreSQL Plus driver, from `TabularisDB/tabularis-postgresql-plugin` |
| `extensions/sqlserver-plus` | SQL Server Plus driver with Entra sign-in through Azure CLI |
| `extensions/spatial` | Spatial plugin and PostGIS table maps |
| `extensions/azure` | Cosmos DB for NoSQL plugin |

## What the fork adds

- A separate identity: app name, bundle identifier, profile directory, keychain service, deep-link scheme, and MCP
  client key. The official update feed is off.
- An import from the official profile that copies connections, groups, saved queries, settings, and saved passwords.
- An MCP server with 57 tools: catalog browsing, asynchronous queries with paging and cancellation, query history,
  `query_explain` for estimated execution plans, and `server_activity` and `server_cancel` for queries that run on
  the server.
- One write policy for every tool: read-only mode, approval in the app, and a SQL classifier that treats anything it
  cannot prove to be a single read as a write.

Details, the verification status, and known gaps are in [RELEASE_NOTES_EXTENDED.md](RELEASE_NOTES_EXTENDED.md).

## Build

Use Node 24.21.0, pnpm 10.30.3, a Rust toolchain that matches the lockfiles, and the Tauri build tools.

```sh
pnpm install --frozen-lockfile
pnpm tauri build --bundles app
```

Each extension has a `justfile`; `just package` builds its release archive. The archive holds the plugin binary,
its manifest, and the `LICENSE`. The SQL Server Plus EXPLAIN parser needs pnpm 11.23.0, which its own
`package.json` pins.

Builds are unsigned. Install the app and the two driver archives yourself.

## Following upstream

[UPSTREAM_SYNC.md](UPSTREAM_SYNC.md) describes how new Tabularis releases are merged. The command
`node scripts/upstream-sync.mjs status` shows the newest release and the files a merge would conflict on. The
original plan is in [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).

## License

Apache License 2.0, as upstream. Each extension keeps its own `LICENSE`.
