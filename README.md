# Tabularis Extended

Tabularis Extended is a personal fork of [Tabularis](https://github.com/TabularisDB/tabularis) for people who let
coding agents work with their databases. It runs beside the official app and gives its MCP server the database
features that agents expect from a database IDE. It never changes the official app's profile or keychain entries.

## What the fork adds

- A separate identity: app name, bundle identifier, profile directory, keychain service, deep-link scheme, and MCP
  client key.
- An import from the official profile that copies connections, groups, saved queries, settings, and saved passwords.
- An MCP server with catalog browsing, asynchronous queries with paging and cancellation, query history, estimated
  execution plans, and tools to list and stop queries that run on the server.
- One write policy for every tool: read-only mode, approval in the app, and a SQL classifier that treats anything it
  cannot prove to be a single read as a write.
- Four extensions: PostgreSQL Plus and SQL Server Plus drivers, a Spatial plugin for PostGIS maps, and a Cosmos DB
  for NoSQL plugin.

## Platforms and prerequisites

Only macOS on Apple silicon has been tested with the real app. Builds are unsigned, and the app has no update feed.

To build, use the Node version in `.node-version`, pnpm 10.30.3, a Rust toolchain that matches the lockfiles, the
Tauri build tools, and `just` for the driver packages. The SQL Server Plus EXPLAIN parser also needs pnpm 11.23.0,
which its own `package.json` pins.

## Install and first use

1. Build the app (see [Build from source](#build-from-source)) and copy `Tabularis Extended.app` to `Applications`.
2. Build the PostgreSQL Plus and SQL Server Plus packages with `just package` in each extension folder. Unzip each
   archive into its own folder under `~/Library/Application Support/tabularis-extended/plugins/drivers/`.
3. Start the app. On first run it offers to import the official profile. Without one, add a connection in the app.
4. Open **MCP Server** in the sidebar and press **Install Config** for your AI client, such as Claude, Cursor, or
   Codex. The config key is `tabularis-extended`. The page also shows the command to add by hand.
5. Restart the AI client. It can now list your connections and run queries through the MCP server.

## Documentation

| Document | Question it answers |
|---|---|
| [RELEASE_NOTES_EXTENDED.md](RELEASE_NOTES_EXTENDED.md) | What does the fork change, how does the MCP server behave, and what was verified? |
| [UPSTREAM_SYNC.md](UPSTREAM_SYNC.md) | How is a new Tabularis release merged into the fork? `node scripts/upstream-sync.mjs status` shows the newest release and the files a merge would conflict on. |
| [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md) | Why was the fork built this way? (the original plan) |
| [docs/web/README.md](docs/web/README.md) | How do I run Tabularis in a browser with `tabularis web`? |
| [PostgreSQL Plus](extensions/postgresql/README.md), [SQL Server Plus](extensions/sqlserver-plus/README.md), [Spatial](extensions/spatial/README.md), [Cosmos DB](extensions/azure/README.md) | What does each extension do, and how is it built? |

## Build from source

With the [prerequisites](#platforms-and-prerequisites) installed, build the app:

```sh
pnpm install --frozen-lockfile
pnpm tauri build --bundles app
```

| Path | Contents | Package build |
|---|---|---|
| `src-tauri` | The Rust backend and the MCP server | `pnpm tauri build` |
| `packages/web-ui` | The React frontend | Built with the app |
| `extensions/postgresql` | PostgreSQL Plus driver | `just package` |
| `extensions/sqlserver-plus` | SQL Server Plus driver with Entra sign-in through Azure CLI | `just package` |
| `extensions/spatial` | Spatial plugin and PostGIS table maps | [docs/building.md](extensions/spatial/docs/building.md) |
| `extensions/azure` | Cosmos DB for NoSQL plugin | [docs/building.md](extensions/azure/docs/building.md) |

## License

Apache License 2.0, as upstream. Each extension keeps its own `LICENSE`.
