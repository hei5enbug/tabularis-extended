# Tabularis Extended

Tabularis Extended is a maintained fork of Tabularis 0.27.0 with a separate application identity,
profile, keychain service, and MCP registration. MySQL and SQLite remain built in.
PostgreSQL uses the official plugin. SQL Server uses the companion SQL Server Plus driver.

The fork adds managed SELECT execution, saved-result paging, cancellation, status, and MCP history.
It also rejects stale connection edits and provides an explicit import from the original application.

| Repository | Responsibility |
|---|---|
| [tabularis-extended](https://github.com/hei5enbug/tabularis-extended) | Desktop host, profile isolation, connection revisions, and MCP query management |
| [tabularis-sqlserver-plus](https://github.com/hei5enbug/tabularis-sqlserver-plus) | SQL Server authentication, native cancellation, connection UI, and EXPLAIN parser |
| [tabularis-spatial](https://github.com/hei5enbug/tabularis-spatial) | Spatial plugin and PostGIS table maps |
| [tabularis-azure](https://github.com/hei5enbug/tabularis-azure) | Cosmos DB for NoSQL plugin |

Use the [Extended build and installation guide](BUILD_EXTENDED.md) for this fork.
The initial artifacts are unsigned and use explicit installation. The official automatic update feed is disabled.
The build includes a verified companion package pin in [integration/companions.json](integration/companions.json).

The [implementation contract](IMPLEMENTATION_PLAN.md) defines limits and acceptance criteria.
[Upstream project documentation](README.upstream.md) remains available for inherited features.
Its official downloads, package-manager commands, and release badges refer to the original Tabularis application.
