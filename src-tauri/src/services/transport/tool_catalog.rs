//! The MCP tool catalog: one tool per service operation (dots become
//! underscores) plus `table_preview`, `job_list`, `server_activity` and
//! `server_cancel`. Arguments are flat: the
//! operation's input fields at the top level next to the identifiers the
//! operation uses, so a caller never builds the request envelope.

use super::{
    super::{
        contracts::{ErrorCode, Operation, REQUEST_SCHEMA},
        error::{failure, ServiceResult},
    },
    server_tools,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

pub const MAX_WAIT_MS: u64 = 60_000;
pub const DEFAULT_WAIT_MS: u64 = 5_000;
pub const OBJECT_KINDS: &[&str] = &["table", "view", "materialized_view", "routine", "trigger"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Service(Operation),
    TablePreview,
    JobList,
    ServerActivity,
    ServerCancel,
}

#[derive(Clone, Debug)]
pub struct ToolSpec {
    pub name: String,
    pub kind: ToolKind,
    pub description: &'static str,
    pub schema: Value,
    /// Argument names the tool accepts.
    pub arguments: BTreeSet<String>,
}

pub fn tool_name(operation: &str) -> String {
    operation.replace('.', "_")
}

pub fn valid_tool_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Tools that read or manage jobs and stored results. They answer at once
/// and never queue behind a query or an approval.
pub fn is_control_tool(name: &str) -> bool {
    matches!(
        name,
        "job_get"
            | "job_result"
            | "job_cancel"
            | "job_list"
            | "result_get"
            | "result_release"
            | "auth_status"
            | "auth_cancel"
            | "auth_logout"
    )
}

/// The tools that take an explicit `write` flag, and the operation each
/// maps onto in the write policy.
pub fn accepts_write(operation: Operation) -> bool {
    super::super::policy::accepts_write_intent(operation)
}

pub fn description(name: &str) -> &'static str {
    match name {
        "connection_list" => "Lists the saved connections this caller may use, with id, name, driver, sanitized settings, version and whether writes are allowed. Read-only.",
        "connection_create" => "Creates a saved connection from non-secret settings and returns it. Writes the profile only after approval in the Tabularis window; passwords and other secrets are rejected.",
        "connection_update" => "Applies a patch of non-secret settings to a saved connection at its current version and returns the new version. Writes the profile only after approval in the Tabularis window; a stale version is rejected.",
        "connection_test" => "Tests a saved connection with its stored credentials and returns whether it connected. Read-only; it accepts no parameter overrides.",
        "catalog_databases" => "Lists the databases the driver reports for a connection. Read-only.",
        "catalog_schemas" => "Lists the schemas of one database and reports the effective database. Read-only.",
        "catalog_objects" => "Lists the objects of a database and schema, optionally only one kind such as table, view or routine, and reports the effective database and schema. Read-only; a kind the driver lacks fails with UNSUPPORTED_OPERATION naming it.",
        "catalog_describe" => "Returns the columns and key positions of one table or view and reports the effective database and schema. Read-only; a kind the driver lacks fails with UNSUPPORTED_OPERATION naming it.",
        "query_execute" => "Registers a SQL job and returns at once with job_id, owner_instance_id and status (queued, awaiting_approval or running), or with the first page of rows when it finishes within wait_ms. Read-only unless write is true, which also needs connection write permission and approval. Without write, a read is one SELECT or WITH statement; on postgresql-plus it may also be one read statement wrapped in BEGIN, SET [LOCAL] of statement_timeout, lock_timeout, idle_in_transaction_session_timeout, plan_cache_mode, work_mem, search_path, jit, random_page_cost, max_parallel_workers_per_gather or the enable_* planner switches, and COMMIT or ROLLBACK (not inside a session), and on sqlserver-plus a T-SQL batch of reads with DECLARE, variable and session-option SET, IF, WHILE, BEGIN...END, TRY...CATCH, THROW, PRINT and WAITFOR DELAY; anything else needs write: true. Page further with result_get.",
        "query_explain" => "Returns the estimated execution plan of one SELECT, WITH, INSERT, UPDATE, DELETE, MERGE, VALUES or TABLE statement without running it: the statement is planned only (never ANALYZE, BUFFERS or STATISTICS XML) inside a read-only call, so it needs no write flag and no approval. data.summary gives total_cost in cost_unit (postgres_planner_cost or sqlserver_estimated_subtree_cost, not comparable across engines), estimated_rows, node_count, nodes (pre-order, at most 40, each with depth, operation, object, index, estimated_rows and total_cost), full_scans (Seq Scan on PostgreSQL; Table Scan and Clustered Index Scan on SQL Server), and on SQL Server warnings and missing_indexes. data.plan holds the raw plan unless data.plan_omitted is true. Use it before query_execute to judge the load of a statement. Only postgresql-plus and sqlserver-plus connections support it.",
        "query_next" => "Continues a result from the page token of an earlier reply and returns the next page. Read-only; stored results are never re-run.",
        "result_get" => "Returns rows of a stored result by offset and limit with next_offset, has_more, stored_row_count and readonly_guarantee. Read-only; it pages the stored rows and never re-runs the SQL.",
        "result_release" => "Deletes a stored result before it expires. Does not touch the database.",
        "row_insert" => "Inserts one row into a table and returns the affected row count. Writes data: needs write set to true, connection write permission and approval.",
        "row_update" => "Updates one row of a table, found by its primary key values, and returns the affected row count. Writes data: needs write set to true, connection write permission and approval.",
        "row_delete" => "Deletes one row of a table, found by its primary key values, and returns the affected row count. Writes data: needs write set to true, connection write permission and approval.",
        "document_read" => "Reads one document by id and partition key from a document container. Read-only.",
        "document_create" => "Creates one document in a container and returns it with its etag. Writes data: needs write set to true, connection write permission and approval.",
        "document_replace" => "Replaces one document when its etag still matches and returns the new etag. Writes data: needs write set to true, connection write permission and approval.",
        "document_delete" => "Deletes one document when its etag still matches. Writes data: needs write set to true, connection write permission and approval.",
        "session_open" => "Opens a database session for several statements and returns its session_id. A write session needs connection write permission and approval; nothing persists until session_commit.",
        "session_commit" => "Commits a session and closes it. Persists a write session's changes, so write must be true for one; a read-only session commits nothing.",
        "session_rollback" => "Rolls back a session and closes it, discarding its pending changes.",
        "session_close" => "Closes a session without committing it.",
        "spatial_columns" => "Lists the geometry and geography columns of a table with their types and SRIDs. Read-only.",
        "spatial_query_result" => "Converts a geometry column of a stored result into map features, one page at a time. Read-only.",
        "spatial_table_query" => "Reads map features of a table inside a viewport, one page at a time. Read-only.",
        "spatial_feature" => "Returns one feature in full, from a stored result or from a table row. Read-only.",
        "spatial_export" => "Exports a layer source to a GeoJSON artifact and returns its artifact id. Does not change the database; it writes an artifact file in the profile.",
        "map_create" => "Creates a saved map and returns it with its version. Writes the saved map file.",
        "map_update" => "Renames a map or changes its basemap at its current version. Writes the saved map file.",
        "map_get" => "Returns one saved map with its layers, viewport, selection and version. Read-only.",
        "map_list" => "Lists the saved maps. Read-only.",
        "map_open" => "Opens a saved map in a running Tabularis window. Changes the window only.",
        "map_close" => "Closes a saved map in a running Tabularis window. Changes the window only.",
        "map_layer_add" => "Adds a layer backed by a query result or table to a saved map. Writes the saved map file.",
        "map_layer_remove" => "Removes a layer from a saved map. Writes the saved map file.",
        "map_layer_update" => "Changes the visibility or style of a map layer. Writes the saved map file.",
        "map_viewport_set" => "Sets the viewport of a saved map. Writes the saved map file.",
        "map_selection_set" => "Sets the selected features of a saved map. Writes the saved map file.",
        "map_save" => "Saves the current state of a map at its version. Writes the saved map file.",
        "job_get" => "Returns the status of a job, including awaiting_approval and cancel_state, without waiting for it. Read-only.",
        "job_result" => "Returns the stored final response of a finished job. Read-only.",
        "job_cancel" => "Asks the driver to cancel a job. The job becomes cancelled only when the driver confirms; otherwise cancel_state shows CANCEL_NOT_CONFIRMED and the call may still finish, so poll job_get. Does not wait unless wait_ms is set.",
        "auth_begin" => "Starts a sign-in for a connection and returns an auth job and what the user must do. Never returns tokens or codes.",
        "auth_status" => "Returns the state of a sign-in started with auth_begin. Read-only.",
        "auth_cancel" => "Cancels a sign-in started with auth_begin.",
        "auth_logout" => "Forgets the stored sign-in of a connection so the next use signs in again.",
        "table_preview" => "Returns the first rows of a table through a dialect-correct, quoted read-only query and reports the effective database and schema. Never writes; replies like query_execute when it takes longer than wait_ms.",
        "job_list" => "Lists running and recent jobs of this process together with persisted history, newest first, filtered by connection, status or time, with an opaque cursor. Returns metadata and SQL text, never rows.",
        "server_activity" | "server_cancel" => server_tools::description(name),
        _ => "",
    }
}

pub(super) fn clean_envelope(key: &str) -> Value {
    match key {
        "connection_id" => {
            json!({"type":"string","minLength":1,"maxLength":128,"description":"Id of the saved connection (see connection_list or list_connections)."})
        }
        "session_id" => {
            json!({"type":"string","minLength":1,"maxLength":128,"description":"Session id from session_open."})
        }
        "map_id" => {
            json!({"type":"string","minLength":1,"maxLength":128,"description":"Id of the saved map."})
        }
        _ => {
            json!({"type":"integer","minimum":0,"maximum":9007199254740991_u64,"description":"Version last read for this record; a stale version is rejected."})
        }
    }
}

fn references(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(name) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/definitions/"))
            {
                names.insert(name.replace("~1", "/").replace("~0", "~"));
            }
            object.values().for_each(|value| references(value, names));
        }
        Value::Array(values) => values.iter().for_each(|value| references(value, names)),
        _ => {}
    }
}

fn with_definitions(source: &Value, mut schema: Value) -> ServiceResult<Value> {
    let mut pending = BTreeSet::new();
    references(&schema, &mut pending);
    let mut definitions = Map::new();
    while let Some(name) = pending.pop_first() {
        if definitions.contains_key(&name) {
            continue;
        }
        let definition = source["definitions"]
            .get(&name)
            .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
        references(definition, &mut pending);
        definitions.insert(name, definition.clone());
    }
    if !definitions.is_empty() {
        schema["definitions"] = Value::Object(definitions);
    }
    Ok(schema)
}

fn add(properties: &mut Map<String, Value>, name: &str, schema: Value) {
    properties.insert(name.into(), schema);
}

fn service_schema(source: &Value, operation: Operation) -> ServiceResult<Value> {
    let name = serde_json::to_value(operation)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    let branch = source["oneOf"]
        .as_array()
        .and_then(|branches| {
            branches
                .iter()
                .find(|branch| branch["properties"]["operation"]["const"] == name.as_str())
        })
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    let input = source["definitions"]
        .get(format!("input.{name}"))
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    let mut properties = input["properties"].as_object().cloned().unwrap_or_default();
    let mut required: Vec<String> = input["required"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for key in ["connection_id", "session_id", "map_id", "expected_version"] {
        if branch["properties"].get(key).is_some() {
            add(&mut properties, key, clean_envelope(key));
            if branch["required"]
                .as_array()
                .is_some_and(|names| names.iter().any(|value| value == key))
            {
                required.push(key.into());
            }
        }
    }
    if matches!(
        operation,
        Operation::QueryExecute
            | Operation::RowInsert
            | Operation::RowUpdate
            | Operation::RowDelete
    ) && !properties.contains_key("session_id")
    {
        add(&mut properties, "session_id", clean_envelope("session_id"));
    }
    add(
        &mut properties,
        "deadline_ms",
        json!({"type":"integer","minimum":1,"maximum":120000,"default":30000,"description":"End-to-end limit in milliseconds. At expiry the driver is asked to cancel and gets 5 seconds to answer."}),
    );
    if accepts_write(operation) {
        add(
            &mut properties,
            "write",
            json!({"type":"boolean","description":"Set to true only when this call is meant to change data. It is the explicit write intent the policy requires; a statement that is not a proven read fails without it, and a proven read fails with it."}),
        );
    }
    // Contract fields that have a natural default are optional here.
    let optional: &[&str] = match operation {
        Operation::QueryExecute => &["language", "parameters", "page_size"],
        Operation::CatalogObjects => &["database", "schema"],
        Operation::CatalogSchemas => &["database"],
        Operation::ResultGet => &["result_set_index", "offset", "limit"],
        Operation::AuthBegin => &["persistence"],
        _ => &[],
    };
    required.retain(|name| !optional.contains(&name.as_str()));
    if matches!(
        operation,
        Operation::CatalogDatabases
            | Operation::CatalogSchemas
            | Operation::CatalogObjects
            | Operation::CatalogDescribe
    ) {
        add(
            &mut properties,
            "offset",
            json!({"type":"integer","minimum":0,"default":0,"description":"Entry to start the list at; use next_offset of the previous reply when has_more is true. A list that does not fit one 64 KiB response is returned in parts."}),
        );
    }
    match operation {
        Operation::QueryExecute => {
            add(
                &mut properties,
                "wait_ms",
                json!({"type":"integer","minimum":0,"maximum":MAX_WAIT_MS,"default":DEFAULT_WAIT_MS,"description":"How long to wait for the job before replying with its job_id. A job that finishes in time returns its first page."}),
            );
            add(
                &mut properties,
                "schema",
                json!({"type":"string","minLength":1,"description":"Schema to run the statement in (optional)."}),
            );
            properties["database"] = json!({"type":"string","minLength":1,"description":"Database to use for this call (optional). It must be one catalog_databases lists for the connection."});
            properties["language"] = json!({"enum":["sql","cosmos_sql"],"default":"sql"});
            properties["page_size"] =
                json!({"type":"integer","minimum":1,"maximum":1000,"default":100});
        }
        Operation::CatalogObjects => {
            properties["database"] = json!({"type":"string","minLength":1,"description":"Database to list (optional). It must be one catalog_databases lists; the connection's own database when omitted."});
            properties["schema"] = json!({"anyOf":[{"type":"string","minLength":1},{"type":"null"}],"description":"Schema to list (optional)."});
            add(
                &mut properties,
                "kind",
                json!({"enum":OBJECT_KINDS,"description":"Only objects of this kind (optional)."}),
            );
        }
        Operation::CatalogSchemas => {
            properties["database"] = json!({"type":"string","minLength":1,"description":"Database whose schemas to list (optional). It must be one catalog_databases lists; the connection's own database when omitted."});
        }
        Operation::CatalogDescribe => {
            properties["table"] = json!({"description":"Table name, or an object with database, schema and table.","oneOf":[{"type":"string","minLength":1},{"$ref":"#/definitions/table"}]});
            add(
                &mut properties,
                "database",
                json!({"type":"string","minLength":1,"description":"Database of the table (optional). It must be one catalog_databases lists."}),
            );
            add(
                &mut properties,
                "schema",
                json!({"type":"string","minLength":1,"description":"Schema of the table (optional)."}),
            );
            add(
                &mut properties,
                "kind",
                json!({"enum":OBJECT_KINDS,"default":"table","description":"Kind of the object to describe."}),
            );
        }
        Operation::QueryExplain => {
            properties["sql"] = json!({"type":"string","minLength":1,"description":"The one statement to plan, without EXPLAIN. It is never run."});
            properties["database"] = json!({"type":"string","minLength":1,"description":"Database to plan against (optional). It must be one catalog_databases lists for the connection."});
            properties["schema"] = json!({"type":"string","minLength":1,"description":"Schema to put first on the search path for the plan (optional); PostgreSQL only, SQL Server ignores it."});
        }
        Operation::ResultGet => {
            properties["result_set_index"] = json!({"type":"integer","minimum":0,"default":0});
            properties["offset"] = json!({"type":"integer","minimum":0,"default":0,"description":"Row to start at; use next_offset of the previous reply."});
            properties["limit"] = json!({"type":"integer","minimum":1,"maximum":1000,"default":100,"description":"Most rows to return. Fewer come back when they would not fit the 64 KiB response."});
        }
        Operation::JobCancel => {
            add(
                &mut properties,
                "wait_ms",
                json!({"type":"integer","minimum":0,"maximum":10000,"default":0,"description":"Wait this long for the job to end after the cancellation request."}),
            );
        }
        _ => {}
    }
    let mut schema = json!({"type":"object","properties":properties,"required":required,"additionalProperties":false});
    for key in ["allOf", "anyOf", "oneOf", "not"] {
        if let Some(value) = input.get(key) {
            schema[key] = value.clone();
        }
    }
    with_definitions(source, schema)
}

fn table_preview_schema() -> Value {
    json!({"type":"object","properties":{
        "connection_id":clean_envelope("connection_id"),
        "table":{"type":"string","minLength":1,"description":"Table or view name, without quotes."},
        "database":{"type":"string","minLength":1,"description":"Database (optional). It must be one catalog_databases lists."},
        "schema":{"type":"string","minLength":1,"description":"Schema (optional)."},
        "limit":{"type":"integer","minimum":1,"maximum":1000,"default":10,"description":"Rows to preview."},
        "wait_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS,"default":DEFAULT_WAIT_MS},
        "deadline_ms":{"type":"integer","minimum":1,"maximum":120000,"default":30000}
    },"required":["connection_id","table"],"additionalProperties":false})
}

fn job_list_schema() -> Value {
    json!({"type":"object","properties":{
        "connection_id":{"type":"string","minLength":1,"maxLength":128,"description":"Only jobs of this connection (optional)."},
        "status":{"enum":["queued","running","succeeded","failed","cancelled","interrupted","outcome_unknown"],"description":"Only jobs in this status (optional)."},
        "since_ms":{"type":"integer","minimum":0,"description":"Only jobs started at or after this Unix time in milliseconds (optional)."},
        "until_ms":{"type":"integer","minimum":0,"description":"Only jobs started before this Unix time in milliseconds (optional)."},
        "limit":{"type":"integer","minimum":1,"maximum":200,"default":50},
        "cursor":{"type":"string","minLength":1,"description":"Opaque cursor from the previous reply's next_cursor."}
    },"additionalProperties":false})
}

pub fn build() -> ServiceResult<Vec<ToolSpec>> {
    let source: Value = serde_json::from_str(REQUEST_SCHEMA)
        .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
    let operations = source["properties"]["operation"]["enum"]
        .as_array()
        .ok_or_else(|| failure(ErrorCode::CapabilityUnavailable))?;
    let mut tools = Vec::new();
    for name in operations.iter().filter_map(Value::as_str) {
        let operation: Operation = serde_json::from_value(json!(name))
            .map_err(|_| failure(ErrorCode::CapabilityUnavailable))?;
        if super::mcp_forbidden(operation) {
            continue;
        }
        let tool = tool_name(name);
        let schema = service_schema(&source, operation)?;
        tools.push(spec(tool, ToolKind::Service(operation), schema));
    }
    tools.push(spec(
        "table_preview".into(),
        ToolKind::TablePreview,
        table_preview_schema(),
    ));
    tools.push(spec(
        "job_list".into(),
        ToolKind::JobList,
        job_list_schema(),
    ));
    tools.push(spec(
        "server_activity".into(),
        ToolKind::ServerActivity,
        server_tools::activity_schema(),
    ));
    tools.push(spec(
        "server_cancel".into(),
        ToolKind::ServerCancel,
        server_tools::cancel_schema(),
    ));
    for tool in &tools {
        if !valid_tool_name(&tool.name) || tool.description.is_empty() {
            return Err(failure(ErrorCode::CapabilityUnavailable));
        }
    }
    Ok(tools)
}

fn spec(name: String, kind: ToolKind, schema: Value) -> ToolSpec {
    let arguments = schema["properties"]
        .as_object()
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    ToolSpec {
        description: description(&name),
        name,
        kind,
        schema,
        arguments,
    }
}
