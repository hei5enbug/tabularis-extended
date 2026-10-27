//! `server_activity` and `server_cancel`: the statement is built for the
//! connection's dialect and submitted as a `query.execute`, so the write
//! policy, read-only mode, approval and revision checks see an ordinary query.

use super::super::super::error::unsupported_server_tool;
use super::super::server_tools::{
    self, DEFAULT_ACTIVITY_LIMIT, MAX_ACTIVITY_LIMIT, MAX_PROCESS_ID,
};
use super::*;

fn flag(arguments: &Map<String, Value>, key: &str) -> ServiceResult<Option<bool>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(failure(ErrorCode::InvalidArgument)),
    }
}

/// A JSON integer between 1 and the largest id either engine hands out.
fn process_id(arguments: &Map<String, Value>) -> ServiceResult<u32> {
    arguments
        .get("process_id")
        .and_then(Value::as_u64)
        .filter(|id| (1..=MAX_PROCESS_ID).contains(id))
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| failure(ErrorCode::InvalidArgument))
}

struct ServerStatement {
    tool: &'static str,
    page_size: u64,
    write: bool,
}

impl ServiceTransport {
    pub(super) async fn server_activity(
        self: &Arc<Self>,
        arguments: Map<String, Value>,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        let include_idle = flag(&arguments, "include_idle")?.unwrap_or(false);
        let limit =
            bounded(&arguments, "limit", 1, MAX_ACTIVITY_LIMIT)?.unwrap_or(DEFAULT_ACTIVITY_LIMIT);
        self.run_server_statement(
            ServerStatement {
                tool: "server_activity",
                page_size: limit,
                write: false,
            },
            arguments,
            caller,
            signal,
            |flavor| server_tools::activity_sql(flavor, include_idle, limit),
        )
        .await
    }

    pub(super) async fn server_cancel(
        self: &Arc<Self>,
        arguments: Map<String, Value>,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
    ) -> ServiceResult<Value> {
        let process = process_id(&arguments)?;
        let write = flag(&arguments, "write")?.unwrap_or(false);
        self.run_server_statement(
            ServerStatement {
                tool: "server_cancel",
                page_size: 1,
                write,
            },
            arguments,
            caller,
            signal,
            |flavor| server_tools::cancel_sql(flavor, process),
        )
        .await
    }

    /// Without `write` the statement carries no registered write intent, so
    /// the runtime refuses it as an unproven read.
    async fn run_server_statement(
        self: &Arc<Self>,
        ServerStatement {
            tool,
            page_size,
            write,
        }: ServerStatement,
        arguments: Map<String, Value>,
        caller: CallerContext,
        signal: Option<CancellationSignal>,
        statement: impl FnOnce(SqlFlavor) -> ServiceResult<String>,
    ) -> ServiceResult<Value> {
        let connection_id = text(&arguments, "connection_id")?
            .ok_or_else(|| failure(ErrorCode::InvalidArgument))?
            .to_owned();
        let database = text(&arguments, "database")?.map(str::to_owned);
        let wait = bounded(&arguments, "wait_ms", 0, MAX_WAIT_MS)?.unwrap_or(DEFAULT_WAIT_MS);
        let deadline = bounded(&arguments, "deadline_ms", 1, 120_000)?.unwrap_or(30_000);
        let connection = self.runtime.connections.get(&connection_id).await?;
        self.runtime.policy.permission(&connection, &caller).await?;
        let unsupported = |error: ServiceError| {
            if error.code == ErrorCode::CapabilityUnavailable {
                unsupported_server_tool(tool, &connection.driver)
            } else {
                error
            }
        };
        let traits = self.traits(&connection).await.map_err(unsupported)?;
        let mut input = json!({
            "language":"sql",
            "text":statement(traits.flavor).map_err(unsupported)?,
            "parameters":[],
            "page_size":page_size,
        });
        if let Some(database) = &database {
            input["database"] = json!(database);
        }
        let extras = Extras {
            database: database.clone(),
            ..Extras::default()
        };
        let effective = json!({
            "database":database.or_else(|| Self::connection_database(&connection)),
            "schema":null,
        });
        let raw = json!({
            "protocol_version":1,
            "operation":"query.execute",
            "request_id":uuid::Uuid::new_v4().to_string(),
            "connection_id":connection_id,
            "deadline_ms":deadline,
            "input":input,
        });
        let query = request::decode(&self.validator, &raw)?;
        if let Err(error) = self.check_database(&query, &extras, &caller, &signal).await {
            return Ok(Self::failed(&query, error, Some(&effective)));
        }
        if write {
            self.register_query_write(raw.clone(), &caller)?;
        }
        let response = self
            .run_asynchronously(raw, query.clone(), caller, wait, signal)
            .await;
        Ok(self.finish(&query, response, &effective, &extras))
    }
}
