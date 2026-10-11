# Owned database integration tests

This suite starts disposable SQL Server 2022 and PostgreSQL/PostGIS containers with Testcontainers 12.2.0.
Images are pinned by digest. Ports and database passwords are generated for each run.
The suite creates a private temporary host profile and plugin directory.
It does not load existing connection files or keychain entries.

Build the host executable, SQL Server Plus package, and the PostgreSQL source pinned in
`integration/companions.json` first. Provide absolute paths:

```sh
npm --prefix tests/containers ci --ignore-scripts --no-audit --no-fund
TABULARIS_TEST_HOST_BINARY=/absolute/path/to/tabularis-extended \
TABULARIS_TEST_SQL_PACKAGE_DIR=/absolute/path/to/unpacked/sqlserver-plus \
TABULARIS_TEST_POSTGRES_SOURCE_DIR=/absolute/path/to/pinned-postgresql-source \
  npm --prefix tests/containers test
```

The PostgreSQL source directory must contain `.tabularium` and the built
`target/debug/postgresql-plugin` executable.
The SQL Server Plus directory must contain the executable, manifest, UI, and EXPLAIN assets.
The host runs as a standalone `--mcp` subprocess.

The suite checks the five legacy and six managed tools, repeatable stored pages, empty results,
JSON/TOON compatibility, queued and running cancellation, deadlines, write rejection, and restart history.
It also checks access revocation after driver or connection changes, disabled audit storage, approval deadlines,
edited-query validation, and a switch to approval-required policy while a query is queued.
Approval IPC uses a synthetic heartbeat and decision files in the owned temporary profile; it does not open a GUI.
Independent driver processes query database activity to verify that cancellation terminates only the owned query.
A PostgreSQL test combines ordinary and managed calls to exercise the shared concurrency limit.

No test sends `KILL` or administrative termination commands.
All SQL sent to a database is SELECT-only. Invalid statements submitted to the host must be rejected before execution.
Containers, subprocesses, and temporary profiles are removed after the run.
On ARM64 machines, these pinned database images run through Docker's x64 emulation.
Emulated test success does not establish native ARM64 support for SQL Server.
