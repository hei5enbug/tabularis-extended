# Service statement limitations

This page lists known limits of `execute_query_exact`, the service-protocol method behind the MCP
`query_execute` tool. Each limit is deliberate and covered by tests.

## Affected rows come from the last statement

The TDS client reports the rows a statement returns, not the rows it changes. For a write, the plugin
appends `SELECT @@ROWCOUNT` when the last top-level statement is `INSERT`, `UPDATE`, `DELETE`, `MERGE`, or
`SELECT … INTO` (`query_reports_affected_rows` in `src/driver/helpers.rs`).

- A batch with several statements reports only the last statement's count in `affected_rows`.
- A batch whose last statement is not one of those statements, such as `EXEC` or DDL, reports no count. The
  response carries the warning `affected_rows_unavailable`.
- A read (`read_only` context) never gets the sentinel.

## When `sp_executesql` is used

Inside a session, a statement without parameters runs as a plain SQL batch. Temporary `#tables` and `SET`
options therefore persist to the next statement of the same session.

Two cases still run through `sp_executesql` (`collect_with` in `src/driver/service/exact.rs`):

- a statement with parameters, in or outside a session
- any statement outside a session

`sp_executesql` runs the text in its own scope. A `#table` created there is dropped when the call ends,
and a `SET` option it changes reverts after the call.

## An error closes a write session

When a statement fails in a session, the plugin drops the physical session and reports
`session_closed: true` (`src/driver/service/supervise.rs`). The host then discards the session. Nothing in
the session's open transaction is committed.

In a write session the plugin dispatches every statement as a write, including a proven read that the host
accepted without `write: true`. A failure there reports `OUTCOME_UNKNOWN` instead of the database error code,
because the plugin cannot confirm what the server did before the session closed. Open a new session and
check the data before retrying.

The same happens when a statement commits or rolls back the session's transaction, even if it opens a new
one, for example inside a `BEGIN … END` block or through `EXEC`. The plugin compares
`CURRENT_TRANSACTION_ID()` before and after each statement and closes the session when it changed.
