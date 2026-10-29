//! Microsoft SQL Server driver core.
//!
//! Editing is enabled for single/composite primary keys and IDENTITY tables.
//! The driver supports schema introspection, table/view DDL, foreign keys,
//! triggers, and stored-routine management.

pub mod azure_cli_auth;
pub mod blob;
pub mod ddl;
pub mod error;
pub mod explain;
pub mod extract;
pub mod helpers;
pub mod introspection;
pub mod ops;
pub mod pool;
pub mod query_templates;
pub mod routines;
pub mod service;
pub mod triggers;
pub mod types;
pub mod users;
pub mod version;

use mssql_tds::connection::tds_client::{ResultSet, ResultSetClient};
use mssql_tds::error::Error as TdsError;
use mssql_tiberius_bridge::row::RowSchema;
use mssql_tiberius_bridge::Row;

use crate::cancellation::{QueryCancellation, QueryExecutionError};
use crate::models::{ConnectionParams, Pagination, QueryResult};
use crate::pool_manager::{self, get_sqlserver_pool};

/// Maximum rows retained across all result sets from one statement.
///
/// JSON-RPC responses are single JSON lines, so they cannot be streamed to the
/// host incrementally. This ceiling bounds response memory even when callers
/// omit `limit` or request an impractically large page. SQL Server may produce
/// one additional row so the collector can set `truncated` accurately.
pub const MAX_RESULT_ROWS: usize = 10_000;

#[derive(Clone, Copy)]
enum OverflowPolicy {
    /// Cancel the remaining TDS result stream as soon as the ceiling is known
    /// to have been exceeded.
    Stop,
    /// Drain without retaining ordinary rows. Result-bearing DML needs its
    /// trailing affected-row sentinel, even when OUTPUT exceeds the ceiling.
    DrainForAffectedRows,
}

/// Acquire a pooled client from the pool manager.
pub async fn acquire(
    params: &ConnectionParams,
) -> Result<deadpool::managed::Object<pool::BridgeManager>, String> {
    let pool = get_sqlserver_pool(params)
        .await
        .map_err(|message| error::redact_connection_secrets(message, params))?;
    match pool.get().await {
        Ok(connection) => Ok(connection),
        Err(pool_error) => {
            // A pool whose manager cannot connect must not pin stale host,
            // credential, or TLS configuration under a stable connection id.
            pool_manager::remove_sqlserver_pool(params).await;
            let message = error::format_pool_error(&pool_error, params.ssl_mode.as_deref());
            Err(error::redact_connection_secrets(message, params))
        }
    }
}

fn empty_query_result(columns: Vec<String>) -> QueryResult {
    QueryResult {
        columns,
        rows: Vec::new(),
        affected_rows: 0,
        truncated: false,
        pagination: None,
        additional_results: None,
    }
}

/// Run `query` as a simple batch and collect its bounded result sets.
///
/// Goes through the bridge's `inner_mut()` escape hatch instead of
/// `simple_query().into_results()`: the bridge derives columns from rows, so
/// a result set with zero rows would lose its column headers. Reading the
/// result-set metadata directly preserves them, matching the behaviour the
/// UI expects for empty SELECTs. This cannot be simulated faithfully without
/// a TDS stream; SS-003 exercises zero-row headers here through simple,
/// multi-result, batch-RPC, and paginated JSON-RPC calls.
async fn run_query_collecting(
    conn: &mut pool::BridgeConnection,
    query: &str,
    overflow_policy: OverflowPolicy,
) -> Result<Vec<QueryResult>, String> {
    conn.ensure_usable()?;
    let query_timeout_seconds = conn.query_timeout_seconds();
    let ssl_mode = conn.ssl_mode().to_owned();
    let results = collect_result_sets(
        conn.inner_mut(),
        query,
        query_timeout_seconds,
        overflow_policy,
        None,
    )
    .await;
    if results.is_err() {
        conn.discard_if_batch_left_open();
    }
    results.map_err(|error| error.into_string(&ssl_mode))
}

/// Why collecting a statement's result sets stopped.
#[derive(Debug)]
enum QueryRunError {
    CancelledBeforeDispatch,
    Tds(TdsError),
    Driver(String),
}

impl QueryRunError {
    fn into_string(self, ssl_mode: &str) -> String {
        match self {
            Self::CancelledBeforeDispatch => "query cancelled before dispatch".to_string(),
            Self::Tds(error) => error::format_tds_error(&error, Some(ssl_mode)),
            Self::Driver(message) => message,
        }
    }
}

async fn collect_result_sets(
    client: &mut mssql_tiberius_bridge::TdsClient,
    query: &str,
    query_timeout_seconds: Option<u32>,
    overflow_policy: OverflowPolicy,
    cancellation: Option<&QueryCancellation>,
) -> Result<Vec<QueryResult>, QueryRunError> {
    // Drain any leftover state from a prior query / dropped stream so we
    // don't hit "open batch" errors when re-using the client.
    client.close_query().await.map_err(QueryRunError::Tds)?;
    let cancel_handle = match cancellation {
        Some(control) if control.begin_query_dispatch() => Some(control.tds_child_handle()),
        Some(_) => return Err(QueryRunError::CancelledBeforeDispatch),
        None => None,
    };
    client
        .execute(
            query.to_string(),
            query_timeout_seconds,
            cancel_handle.as_ref(),
        )
        .await
        .map_err(QueryRunError::Tds)?;

    let mut results = Vec::new();
    let mut retained_rows = 0usize;
    let mut stopped_early = false;
    'result_sets: while let Some(result_set) = client.get_current_resultset() {
        let metadata = result_set.get_metadata().clone();
        let schema = RowSchema::from_metadata(&metadata);
        let is_affected_rows_sentinel =
            metadata.len() == 1 && metadata[0].column_name == helpers::AFFECTED_ROWS_COLUMN;
        let mut current = empty_query_result(
            metadata
                .iter()
                .map(|column| column.column_name.clone())
                .collect(),
        );
        while let Some(values) = result_set.next_row().await.map_err(QueryRunError::Tds)? {
            if retained_rows >= MAX_RESULT_ROWS && !is_affected_rows_sentinel {
                current.truncated = true;
                if matches!(overflow_policy, OverflowPolicy::Stop) {
                    results.push(current);
                    stopped_early = true;
                    break 'result_sets;
                }
                continue;
            }

            let row = Row::from_schema(schema.clone(), values);
            current.rows.push(
                metadata
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let column_type = extract::normalized_column_type(
                            column.data_type,
                            column.type_info.length,
                        );
                        extract::extract_value_as(&row, index, column_type)
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(QueryRunError::Driver)?,
            );
            if !is_affected_rows_sentinel {
                retained_rows += 1;
            }
        }
        results.push(current);
        if !client.move_to_next().await.map_err(QueryRunError::Tds)? {
            break;
        }
    }
    if stopped_early {
        client.close_query().await.map_err(QueryRunError::Tds)?;
    }
    if results.iter().any(|result| result.truncated) {
        // `truncated` on the primary result is the statement-level signal the
        // host already consumes. Keep it true even if the budget was crossed
        // in an additional result set.
        if let Some(first) = results.first_mut() {
            first.truncated = true;
        }
    }
    Ok(results)
}

/// Pull the trailing [`helpers::AFFECTED_ROWS_COLUMN`] result set produced by
/// a parameterized DML batch and return its count.
pub fn affected_rows_from_query(result: mssql_tiberius_bridge::QueryResult) -> Result<u64, String> {
    result
        .into_results()
        .last()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get::<i64, _>(0))
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| "SQL Server did not return affected rows for DML".to_string())
}

fn affected_rows_value(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn finish_result_bearing_dml(mut results: Vec<QueryResult>) -> Result<QueryResult, String> {
    let affected = results
        .last()
        .filter(|result| result.columns == [helpers::AFFECTED_ROWS_COLUMN])
        .and_then(|result| result.rows.first())
        .and_then(|row| row.first())
        .and_then(affected_rows_value)
        .ok_or_else(|| {
            "SQL Server did not return affected rows for result-bearing DML".to_string()
        })?;
    // The sentinel is an implementation detail. Removing it here guarantees
    // OUTPUT and mixed batches never expose a stray one-cell grid to the host.
    results.pop();

    let mut first = if results.is_empty() {
        empty_query_result(Vec::new())
    } else {
        results.remove(0)
    };
    first.affected_rows = affected;
    if !results.is_empty() {
        first.additional_results = Some(results);
    }
    Ok(first)
}

async fn execute_result_bearing_dml(
    conn: &mut pool::BridgeConnection,
    query: &str,
) -> Result<QueryResult, String> {
    let wrapped = helpers::wrap_dml_with_rowcount(query);
    finish_result_bearing_dml(
        run_query_collecting(conn, &wrapped, OverflowPolicy::DrainForAffectedRows).await?,
    )
}

pub async fn execute_on_connection(
    conn: &mut pool::BridgeConnection,
    query: &str,
    limit: Option<u32>,
    page: u32,
) -> Result<QueryResult, String> {
    let returns_result_set = helpers::query_returns_result_set(query);
    if helpers::query_reports_affected_rows(query) {
        // The TDS client reports rows returned, not rows affected, so every
        // final DML statement goes through the @@ROWCOUNT-capturing batch —
        // result-bearing (OUTPUT clauses) or not. SS-003 verifies plain,
        // multi-statement, and OUTPUT cases against SQL Server.
        let mut result = execute_result_bearing_dml(conn, query).await?;
        if !returns_result_set {
            result.columns = Vec::new();
            result.rows = Vec::new();
        }
        return Ok(result);
    }
    if !returns_result_set {
        conn.simple_query(query)
            .await
            .map_err(|error| error.to_string())?
            .into_results();
        return Ok(empty_query_result(Vec::new()));
    }

    let pagination_limit = limit.filter(|_| helpers::query_can_be_paginated(query));
    let mut pagination = pagination_limit.map(|page_size| Pagination {
        page,
        page_size,
        total_rows: None,
        has_more: false,
    });
    let final_query = match pagination_limit {
        Some(page_size) => helpers::build_paginated_query(query, page_size, page),
        None => query.to_string(),
    };
    let mut results = run_query_collecting(conn, &final_query, OverflowPolicy::Stop).await?;
    let mut first = if results.is_empty() {
        empty_query_result(Vec::new())
    } else {
        results.remove(0)
    };

    if let Some(ref mut pagination) = pagination {
        pagination.has_more = first.truncated || first.rows.len() > pagination.page_size as usize;
        if first.rows.len() > pagination.page_size as usize {
            first.rows.truncate(pagination.page_size as usize);
            first.truncated = true;
        }
    }
    first.pagination = pagination;
    if !results.is_empty() {
        first.additional_results = Some(results);
    }
    Ok(first)
}

/// Collect one statement's result sets with the request's cancellation handle
/// on the SQL batch. A completion that wins the race against a cancellation
/// stays a success.
async fn run_query_collecting_with_cancellation(
    conn: &mut pool::BridgeConnection,
    query: &str,
    overflow_policy: OverflowPolicy,
    cancellation: &QueryCancellation,
) -> Result<Vec<QueryResult>, QueryExecutionError> {
    conn.ensure_usable().map_err(QueryExecutionError::Driver)?;
    let query_timeout_seconds = conn.query_timeout_seconds();
    let ssl_mode = conn.ssl_mode().to_owned();
    let results = collect_result_sets(
        conn.inner_mut(),
        query,
        query_timeout_seconds,
        overflow_policy,
        Some(cancellation),
    )
    .await;
    match results {
        Ok(results) => {
            cancellation.finish_query_dispatch();
            Ok(results)
        }
        Err(error) => {
            conn.discard_if_batch_left_open();
            Err(map_cancelable_query_error(
                conn,
                error,
                cancellation,
                &ssl_mode,
            ))
        }
    }
}

fn map_cancelable_query_error(
    conn: &mut pool::BridgeConnection,
    error: QueryRunError,
    cancellation: &QueryCancellation,
    ssl_mode: &str,
) -> QueryExecutionError {
    match error {
        QueryRunError::CancelledBeforeDispatch => QueryExecutionError::CancelledBeforeDispatch,
        QueryRunError::Tds(error) => {
            if cancellation.is_cancelled() && cancellation.query_was_dispatched() {
                // Even a reported cancellation does not prove the stream is
                // clean, so the session is closed instead of pooled.
                conn.discard();
                if cancellation.is_requested_cancellation(&error) {
                    return QueryExecutionError::Cancelled;
                }
            }
            QueryExecutionError::Driver(error::format_tds_error(&error, Some(ssl_mode)))
        }
        QueryRunError::Driver(message) => {
            // Result conversion can fail before the collector drains the TDS
            // stream, so this session is unsafe to recycle.
            conn.discard();
            QueryExecutionError::Driver(message)
        }
    }
}

/// Execute a query with a request-scoped cancellation handle. Every query
/// shape goes through the TDS collector so the actual SQL batch receives the
/// handle.
pub async fn execute_on_connection_with_cancellation(
    conn: &mut pool::BridgeConnection,
    query: &str,
    limit: Option<u32>,
    page: u32,
    cancellation: &QueryCancellation,
) -> Result<QueryResult, QueryExecutionError> {
    if cancellation.cancelled_before_dispatch() {
        return Err(QueryExecutionError::CancelledBeforeDispatch);
    }

    let returns_result_set = helpers::query_returns_result_set(query);
    if helpers::query_reports_affected_rows(query) {
        let wrapped = helpers::wrap_dml_with_rowcount(query);
        let results = run_query_collecting_with_cancellation(
            conn,
            &wrapped,
            OverflowPolicy::DrainForAffectedRows,
            cancellation,
        )
        .await?;
        let mut result = finish_result_bearing_dml(results).map_err(QueryExecutionError::Driver)?;
        if !returns_result_set {
            result.columns = Vec::new();
            result.rows = Vec::new();
        }
        return Ok(result);
    }

    if !returns_result_set {
        run_query_collecting_with_cancellation(conn, query, OverflowPolicy::Stop, cancellation)
            .await?;
        return Ok(empty_query_result(Vec::new()));
    }

    let pagination_limit = limit.filter(|_| helpers::query_can_be_paginated(query));
    let mut pagination = pagination_limit.map(|page_size| Pagination {
        page,
        page_size,
        total_rows: None,
        has_more: false,
    });
    let final_query = match pagination_limit {
        Some(page_size) => helpers::build_paginated_query(query, page_size, page),
        None => query.to_string(),
    };
    let mut results = run_query_collecting_with_cancellation(
        conn,
        &final_query,
        OverflowPolicy::Stop,
        cancellation,
    )
    .await?;
    let mut first = if results.is_empty() {
        empty_query_result(Vec::new())
    } else {
        results.remove(0)
    };

    if let Some(ref mut pagination) = pagination {
        pagination.has_more = first.truncated || first.rows.len() > pagination.page_size as usize;
        if first.rows.len() > pagination.page_size as usize {
            first.rows.truncate(pagination.page_size as usize);
            first.truncated = true;
        }
    }
    first.pagination = pagination;
    if !results.is_empty() {
        first.additional_results = Some(results);
    }
    Ok(first)
}

#[cfg(test)]
mod sql_audit_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result(columns: &[&str], rows: Vec<Vec<serde_json::Value>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|column| (*column).to_string()).collect(),
            rows,
            ..empty_query_result(Vec::new())
        }
    }

    #[test]
    fn dml_sentinel_is_removed_from_output_result() {
        let output = result(&["id"], vec![vec![json!(7)]]);
        let sentinel = result(&[helpers::AFFECTED_ROWS_COLUMN], vec![vec![json!(1)]]);

        let actual = finish_result_bearing_dml(vec![output, sentinel]).unwrap();

        assert_eq!(actual.columns, ["id"]);
        assert_eq!(actual.rows, vec![vec![json!(7)]]);
        assert_eq!(actual.affected_rows, 1);
        assert!(actual.additional_results.is_none());
    }

    #[test]
    fn dml_sentinel_preserves_additional_result_sets() {
        let first = result(&["before"], vec![vec![json!("first")]]);
        let output = result(&["id"], vec![vec![json!(9)]]);
        let sentinel = result(&[helpers::AFFECTED_ROWS_COLUMN], vec![vec![json!(3)]]);

        let actual = finish_result_bearing_dml(vec![first, output, sentinel]).unwrap();

        assert_eq!(actual.affected_rows, 3);
        let additional = actual.additional_results.unwrap();
        assert_eq!(additional.len(), 1);
        assert_eq!(additional[0].columns, ["id"]);
        assert_eq!(additional[0].rows, vec![vec![json!(9)]]);
    }

    #[test]
    fn dml_without_output_returns_only_affected_count() {
        let sentinel = result(&[helpers::AFFECTED_ROWS_COLUMN], vec![vec![json!(2)]]);

        let actual = finish_result_bearing_dml(vec![sentinel]).unwrap();

        assert!(actual.columns.is_empty());
        assert!(actual.rows.is_empty());
        assert_eq!(actual.affected_rows, 2);
        assert!(actual.additional_results.is_none());
    }

    #[test]
    fn dml_affected_count_accepts_js_unsafe_integer_string() {
        let sentinel = result(
            &[helpers::AFFECTED_ROWS_COLUMN],
            vec![vec![json!("9007199254740992")]],
        );

        let actual = finish_result_bearing_dml(vec![sentinel]).unwrap();

        assert_eq!(actual.affected_rows, 9_007_199_254_740_992);
    }

    #[test]
    fn dml_requires_a_well_formed_trailing_sentinel() {
        let output = result(&["id"], vec![vec![json!(7)]]);
        let error = finish_result_bearing_dml(vec![output]).unwrap_err();

        assert!(error.contains("did not return affected rows"));
    }
}
