# SQL Server container tests

Run from the repository root with Docker available:

```sh
cargo build --locked --bin sqlserver-plugin
npm --prefix tests/containers ci --ignore-scripts --no-audit --no-fund
npm --prefix tests/containers test
```

The tests start SQL Server Developer with `ACCEPT_EULA=Y`. They use an image pinned by digest, a random
password and port, and their own disposable container. They do not read saved connections or accept an
external database endpoint. Teardown closes both plugin processes and removes the container.

SQL Server's Linux image uses `linux/amd64`. Running these tests on an ARM host requires Docker emulation;
this does not establish support for SQL Server production deployments on ARM.

The suite checks result shapes, empty and multiple result sets, row truncation, and native cancellation.
An independent plugin process observes `sys.dm_exec_requests` to check that a cancelled SELECT stopped
and neighboring requests kept running. The tests issue no `KILL` statements.

These tests use SQL authentication. Entra token acquisition and identity checks use controlled CLI and
token fixtures in the Rust suite; a SQL Server container cannot validate Azure login or cloud permissions.
