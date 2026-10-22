//! The statements behind `server_activity` and `server_cancel` and their
//! catalog entries. Both tools run as an ordinary `query.execute`, so the
//! classifier, write intent, read-only mode and approval apply to them as to
//! any other statement.

use super::{
    super::{
        contracts::ErrorCode,
        error::{failure, ServiceResult},
    },
    tool_catalog::{clean_envelope, DEFAULT_WAIT_MS, MAX_WAIT_MS},
    SqlFlavor,
};
use serde_json::{json, Value};

pub(crate) const DEFAULT_ACTIVITY_LIMIT: u64 = 50;
pub(crate) const MAX_ACTIVITY_LIMIT: u64 = 200;
pub(crate) const MAX_PROCESS_ID: u64 = 2_147_483_647;
/// Characters of a running statement that the activity list carries.
pub(crate) const QUERY_TEXT_CHARS: usize = 2000;

/// The sessions of the server, one row each, with the same column names on
/// both engines.
pub(crate) fn activity_sql(
    flavor: SqlFlavor,
    include_idle: bool,
    limit: u64,
) -> ServiceResult<String> {
    Ok(match flavor {
        SqlFlavor::Postgres => {
            let idle = if include_idle {
                ""
            } else {
                "AND a.state IS DISTINCT FROM 'idle' "
            };
            format!(
                "SELECT a.pid AS process_id, a.usename::text AS user_name, \
                 a.datname::text AS database_name, a.application_name::text AS application_name, \
                 CASE WHEN a.client_addr IS NULL THEN NULL \
                 ELSE host(a.client_addr) || ':' || a.client_port END AS client, \
                 a.state::text AS state, \
                 CASE WHEN a.wait_event_type IS NULL THEN NULL \
                 ELSE a.wait_event_type || ':' || a.wait_event END AS wait, \
                 (pg_blocking_pids(a.pid))[1] AS blocking_process_id, \
                 a.query_start AS query_start, \
                 (EXTRACT(EPOCH FROM (clock_timestamp() - a.query_start)) * 1000)::bigint AS running_ms, \
                 left(a.query, {QUERY_TEXT_CHARS}) AS query \
                 FROM pg_stat_activity a \
                 WHERE a.backend_type = 'client backend' AND a.pid <> pg_backend_pid() {idle}\
                 ORDER BY a.query_start ASC NULLS LAST, a.pid LIMIT {limit}"
            )
        }
        SqlFlavor::Mssql => {
            let idle = if include_idle {
                ""
            } else {
                "AND r.session_id IS NOT NULL "
            };
            format!(
                "SELECT TOP ({limit}) s.session_id AS process_id, s.login_name AS user_name, \
                 DB_NAME(COALESCE(r.database_id, s.database_id)) AS database_name, \
                 s.program_name AS application_name, s.host_name AS client, \
                 COALESCE(r.status, s.status) AS state, r.wait_type AS wait, \
                 NULLIF(r.blocking_session_id, 0) AS blocking_process_id, \
                 COALESCE(r.start_time, s.last_request_start_time) AS query_start, \
                 COALESCE(r.total_elapsed_time, \
                 DATEDIFF_BIG(MILLISECOND, s.last_request_start_time, GETDATE())) AS running_ms, \
                 r.command AS command, COALESCE(r.cpu_time, s.cpu_time) AS cpu_ms, \
                 COALESCE(r.logical_reads, s.logical_reads) AS logical_reads, \
                 LEFT(t.text, {QUERY_TEXT_CHARS}) AS query \
                 FROM sys.dm_exec_sessions s \
                 LEFT JOIN sys.dm_exec_requests r ON r.session_id = s.session_id \
                 OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) t \
                 WHERE s.is_user_process = 1 AND s.session_id <> @@SPID {idle}\
                 ORDER BY COALESCE(r.start_time, s.last_request_start_time), s.session_id"
            )
        }
        _ => return Err(failure(ErrorCode::CapabilityUnavailable)),
    })
}

/// The statement that stops one process. The id is formatted from the parsed
/// integer, never from caller text.
pub(crate) fn cancel_sql(flavor: SqlFlavor, process_id: u32) -> ServiceResult<String> {
    Ok(match flavor {
        SqlFlavor::Postgres => format!("SELECT pg_cancel_backend({process_id}) AS cancelled"),
        SqlFlavor::Mssql => format!("KILL {process_id}"),
        _ => return Err(failure(ErrorCode::CapabilityUnavailable)),
    })
}

pub(crate) fn description(name: &str) -> &'static str {
    match name {
        "server_activity" => "Lists the queries running on the database server from any client, read from pg_stat_activity on PostgreSQL and sys.dm_exec_sessions on SQL Server, with process_id, user_name, database_name, application_name, client, state, wait, blocking_process_id, query_start, running_ms and the query text cut to 2000 characters; SQL Server also reports command, cpu_ms and logical_reads. Sessions that run no query are left out unless include_idle is true, and limit (1 to 200, default 50) bounds the rows. Read-only; replies like query_execute and pages with result_get. Only postgresql-plus and sqlserver-plus connections support it.",
        "server_cancel" => "Cancels one query on the database server by the process_id that server_activity lists. On PostgreSQL it runs pg_cancel_backend, which cancels the running query and keeps the session; on SQL Server it runs KILL, which ends the whole session and rolls back its open transaction. It changes server state: write must be true, read-only mode refuses it, the connection needs write permission and, under the approval mode, the call waits for approval in the Tabularis window. The server's own permission rules still apply: pg_signal_backend or the same role on PostgreSQL, ALTER ANY CONNECTION on SQL Server. Only postgresql-plus and sqlserver-plus connections support it.",
        _ => "",
    }
}

pub(crate) fn activity_schema() -> Value {
    json!({"type":"object","properties":{
        "connection_id":clean_envelope("connection_id"),
        "database":{"type":"string","minLength":1,"description":"Database to connect to (optional). It must be one catalog_databases lists; the list covers every database of the server either way."},
        "include_idle":{"type":"boolean","default":false,"description":"Also list sessions that run no query."},
        "limit":{"type":"integer","minimum":1,"maximum":MAX_ACTIVITY_LIMIT,"default":DEFAULT_ACTIVITY_LIMIT,"description":"Most sessions to list."},
        "wait_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS,"default":DEFAULT_WAIT_MS},
        "deadline_ms":{"type":"integer","minimum":1,"maximum":120000,"default":30000}
    },"required":["connection_id"],"additionalProperties":false})
}

pub(crate) fn cancel_schema() -> Value {
    json!({"type":"object","properties":{
        "connection_id":clean_envelope("connection_id"),
        "process_id":{"type":"integer","minimum":1,"maximum":MAX_PROCESS_ID,"description":"Process id (PostgreSQL pid, SQL Server session id) from server_activity."},
        "database":{"type":"string","minLength":1,"description":"Database to connect to (optional). It must be one catalog_databases lists."},
        "write":{"type":"boolean","description":"Must be true: stopping a process changes server state and is the explicit write intent the policy requires."},
        "wait_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS,"default":DEFAULT_WAIT_MS},
        "deadline_ms":{"type":"integer","minimum":1,"maximum":120000,"default":30000}
    },"required":["connection_id","process_id"],"additionalProperties":false})
}

#[cfg(test)]
#[path = "server_tools_tests.rs"]
mod tests;
