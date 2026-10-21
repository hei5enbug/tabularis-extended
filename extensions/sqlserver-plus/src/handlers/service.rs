use crate::{
    driver::service::{
        catalog, exact, pool, readonly,
        registry::{self, Scope},
        row, sessions,
        types::{self, DriverContext},
        ServiceError,
    },
    models::ConnectionParams,
};
use mssql_tds::message::transaction_management::TransactionIsolationLevel;
use serde_json::{json, Value};
use std::time::Duration;

pub const METHODS: &[&str] = &[
    "execute_query_exact",
    "service_session_open",
    "service_session_finish",
    "service_session_discard",
    "service_invalidate_auth",
    "service_test",
    "service_catalog",
    "service_row",
    "cancel_request",
];
pub fn is_method(method: &str) -> bool {
    METHODS.contains(&method)
}
pub fn capabilities() -> Value {
    json!({"service_protocol":1,"exact_query":true,"exact_affected_rows":false,"sessions":true,"cancel":true,"entra_token":false,"readonly_guarantee":"restricted_database_principal","catalog":true,"rows":true})
}
pub fn prepare(params: &Value) -> Result<Scope, ServiceError> {
    let (context, _) = DriverContext::parse(params)?;
    Scope::reserve(&context)
}
enum Lease {
    Owned(Option<Box<pool::NativeConnection>>, bool),
    Session(tokio::sync::OwnedMutexGuard<sessions::Slot>, bool),
}
impl Lease {
    fn client(&mut self) -> &mut mssql_tds::connection::tds_client::TdsClient {
        match self {
            Self::Owned(client, _) => client.as_mut().unwrap(),
            Self::Session(slot, _) => slot.client.as_mut().unwrap(),
        }
    }
    fn completed(&mut self) {
        match self {
            Self::Owned(_, valid) | Self::Session(_, valid) => *valid = true,
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        match self {
            Self::Owned(client, false) => {
                if let Some(client) = client.take() {
                    pool::discard(*client);
                }
            }
            Self::Session(slot, false) => {
                if let Some(client) = slot.client.take() {
                    pool::discard(client);
                }
            }
            _ => {}
        }
    }
}
async fn acquire(
    params: &ConnectionParams,
    context: &DriverContext,
    scope: &Scope,
) -> Result<Lease, ServiceError> {
    scope.remaining()?;
    context.validate_auth(params)?;
    if let Some(handle) = &context.session_handle {
        let slot = sessions::lock(handle, params, context, scope).await?;
        scope.touch_session();
        Ok(Lease::Session(slot, false))
    } else {
        Ok(Lease::Owned(
            Some(Box::new(pool::acquire(params, context, scope).await?)),
            false,
        ))
    }
}
async fn run(
    params: &Value,
    method: &str,
    context: &DriverContext,
    connection: &ConnectionParams,
    scope: &Scope,
    response_overhead: usize,
) -> Result<Value, ServiceError> {
    if method == "service_invalidate_auth" {
        types::only_keys(&params["input"], &[])?;
        scope.seal();
        registry::cancel_connection(&context.connection_id);
        pool::invalidate(&context.connection_id).await;
        sessions::invalidate(&context.connection_id).await;
        return Ok(json!({"invalidated":true}));
    }
    if method == "service_session_open" {
        let input: sessions::OpenInput = types::input(params)?;
        if context.session_handle.is_some()
            || (input.mode == sessions::Mode::ReadOnly) != context.read_only
            || input
                .database
                .as_ref()
                .is_some_and(|database| database != connection.database.primary())
        {
            return Err(ServiceError::argument(
                "The session mode or database does not match this connection",
            ));
        }
        let permit = sessions::reserve()?;
        let mut lease = Lease::Owned(
            Some(Box::new(pool::acquire(connection, context, scope).await?)),
            false,
        );
        lease
            .client()
            .begin_transaction(TransactionIsolationLevel::ReadCommitted, None)
            .await
            .map_err(ServiceError::database)?;
        scope.remaining()?;
        let Lease::Owned(client, _) = &mut lease else {
            unreachable!()
        };
        let result = sessions::open(*client.take().unwrap(), connection, context, permit).await;
        scope.register_opened_session(result["session_handle"].as_str().unwrap());
        return Ok(result);
    }
    if matches!(method, "service_session_finish" | "service_session_discard") {
        let handle = context
            .session_handle
            .as_deref()
            .ok_or_else(|| ServiceError::argument("session_handle is required"))?;
        let mut slot = sessions::lock(handle, connection, context, scope).await?;
        if method == "service_session_discard" {
            types::only_keys(&params["input"], &[])?;
            scope.touch_session();
            if let Some(client) = slot.client.take() {
                pool::discard(client);
            }
            drop(slot);
            sessions::forget(handle).await;
            return Ok(
                json!({"session_handle":handle,"in_transaction":false,"rollback_confirmed":false}),
            );
        }
        let input: sessions::FinishInput = types::input(params)?;
        scope.touch_session();
        let mut client = slot
            .client
            .take()
            .ok_or_else(|| ServiceError::argument("The physical session was closed"))?;
        if slot.dirty {
            scope.mark_write();
        }
        let commit = input.action == sessions::Action::Commit;
        let result = if commit {
            client.commit_transaction(None, None).await
        } else {
            client.rollback_transaction(None, None).await
        };
        pool::discard(client);
        drop(slot);
        sessions::forget(handle).await;
        result.map_err(|error| {
            let mut error = ServiceError::database(error);
            error.session_closed = true;
            error
        })?;
        return Ok(
            json!({"session_handle":handle,"in_transaction":false,"rollback_confirmed":!commit}),
        );
    }
    let mut row_operation = None;
    let prepared = match method {
        "execute_query_exact" => {
            let input: exact::ExactInput = types::input(params)?;
            let bindings = exact::validate(&input)?;
            (
                input.text,
                bindings,
                input.max_rows,
                input.max_bytes,
                false,
                None,
            )
        }
        "service_catalog" => {
            let input: catalog::Input = types::input(params)?;
            let query = catalog::query(&input, connection)?;
            let bindings = exact::bind(&query.parameters)?;
            (
                query.text.clone(),
                bindings,
                10_000,
                types::FRAME_LIMIT,
                false,
                Some(query),
            )
        }
        "service_row" => {
            if context.read_only {
                return Err(ServiceError::new(
                    "WRITE_NOT_ALLOWED",
                    "Read-only context cannot edit rows",
                ));
            }
            let input: row::Input = types::input(params)?;
            let query = row::query(&input, connection)?;
            let bindings = exact::bind(&query.parameters)?;
            row_operation = Some(input);
            (query.text, bindings, 10_000, types::FRAME_LIMIT, true, None)
        }
        "service_test" => {
            types::only_keys(&params["input"], &[])?;
            (
                "SELECT 1 AS connected".to_owned(),
                Vec::new(),
                10_000,
                types::FRAME_LIMIT,
                false,
                None,
            )
        }
        _ => {
            return Err(ServiceError::new(
                "UNSUPPORTED_OPERATION",
                "Unknown service operation",
            ))
        }
    };
    let mut lease = acquire(connection, context, scope).await?;
    let cancel = scope.cancel_handle();
    if context.read_only {
        readonly::probe(lease.client(), Some(&cancel)).await?;
    }
    if let Some(input) = &row_operation {
        row::validate_primary_key(lease.client(), input, &cancel).await?;
    }
    if let Lease::Session(slot, _) = &lease {
        if !slot.client.as_ref().unwrap().has_active_transaction() {
            return Err(ServiceError::new(
                "OUTCOME_UNKNOWN",
                "The reserved transaction is no longer active",
            ));
        }
    }
    scope.remaining()?;
    if method == "service_row" || (method == "execute_query_exact" && !context.read_only) {
        scope.mark_write();
        if let Lease::Session(slot, _) = &mut lease {
            slot.dirty = true;
        }
    }
    let prefix =
        method == "execute_query_exact" && context.read_only && context.session_handle.is_none();
    let data = if prefix {
        exact::collect_read_prefix(
            lease.client(),
            &prepared.0,
            prepared.1,
            cancel,
            prepared.2,
            prepared.3,
            response_overhead,
        )
        .await?
    } else {
        exact::collect(
            lease.client(),
            &prepared.0,
            prepared.1,
            Some(&cancel),
            prepared.2,
            prepared.3,
            prepared.4,
        )
        .await?
    };
    if row_operation.is_some() && data["affected_rows"].as_u64().is_none_or(|count| count > 1) {
        return Err(ServiceError::new(
            "OUTCOME_UNKNOWN",
            "The row operation did not confirm a single-row outcome",
        ));
    }
    scope.remaining()?;
    if let Lease::Session(slot, _) = &mut lease {
        if !slot.client.as_ref().unwrap().has_active_transaction() {
            return Err(ServiceError::new(
                "OUTCOME_UNKNOWN",
                "The user SQL changed the reserved transaction",
            ));
        }
        slot.last_used = tokio::time::Instant::now();
    } else if lease.client().has_active_transaction() {
        return Err(ServiceError::new(
            "OUTCOME_UNKNOWN",
            "User SQL opened an unmanaged transaction",
        ));
    }
    if data["truncated"] != true {
        lease.completed();
    }
    Ok(if let Some(query) = prepared.5 {
        catalog::result(&query, &data)
    } else if method == "service_test" {
        json!({"success":true})
    } else {
        data
    })
}
pub async fn dispatch(id: Value, params: &Value, method: &str, supplied: Option<Scope>) -> Value {
    if !types::valid_rpc_id(&id) {
        return ServiceError::argument(
            "Service RPC id must be null, a bounded string, or a safe integer",
        )
        .response(Value::Null);
    }
    if method == "cancel_request" {
        if types::only_keys(params, &["connection_id", "request_id"]).is_err() {
            return ServiceError::argument("Invalid cancellation input").response(id);
        }
        let Some(connection) = params["connection_id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.chars().count() <= 128)
        else {
            return ServiceError::argument("connection_id is required").response(id);
        };
        let Some(request) = params["request_id"]
            .as_str()
            .filter(|v| !v.is_empty() && v.chars().count() <= 128)
        else {
            return ServiceError::argument("request_id is required").response(id);
        };
        return crate::rpc::ok_response(
            id,
            json!({"cancellation_requested":registry::cancel(connection, request)}),
        );
    }
    let parsed = if method == "service_invalidate_auth" {
        DriverContext::parse_control(params).map(|context| (context, ConnectionParams::default()))
    } else {
        DriverContext::parse(params)
    };
    let (context, connection) = match parsed {
        Ok(value) => value,
        Err(error) => return error.response(id),
    };
    let scope = match supplied.map(Ok).unwrap_or_else(|| Scope::reserve(&context)) {
        Ok(scope) => scope,
        Err(error) => return error.response(id),
    };
    let result;
    {
        let response_overhead =
            serde_json::to_vec(&crate::rpc::ok_response(id.clone(), Value::Null))
                .unwrap()
                .len()
                - 4;
        let action = run(
            params,
            method,
            &context,
            &connection,
            &scope,
            response_overhead,
        );
        tokio::pin!(action);
        result = tokio::select! {
            result = &mut action => result,
            _ = tokio::time::sleep_until(scope.state.deadline) => {
                scope.stop(); let _ = tokio::time::timeout(Duration::from_secs(5), &mut action).await;
                Err(ServiceError::new("DEADLINE_EXCEEDED", "The service execution deadline was exceeded"))
            }
            _ = scope.wait_cancelled() => {
                let _ = tokio::time::timeout(Duration::from_secs(5), &mut action).await;
                Err(ServiceError::new("CANCELLED", "The service request was cancelled"))
            }
        };
    }
    scope.seal();
    let response = match result {
        Ok(value) if !scope.cancelled() => crate::rpc::ok_response(id.clone(), value),
        result => {
            let mut error = result.err().unwrap_or_else(|| {
                ServiceError::new("CANCELLED", "The service request was cancelled")
            });
            if scope.cancelled() {
                error.code = if tokio::time::Instant::now() >= scope.state.deadline {
                    "DEADLINE_EXCEEDED"
                } else {
                    "CANCELLED"
                };
            }
            if scope.write_dispatched() {
                error.code = "OUTCOME_UNKNOWN";
                error.unknown = true;
            }
            if scope.session_touched() {
                if let Some(handle) = context.session_handle.as_deref() {
                    sessions::forget(handle).await;
                    error.session_closed = true;
                }
            }
            if let Some(handle) = scope.opened_session() {
                sessions::forget(&handle).await;
                error.session_closed = true;
            }
            error.response(id.clone())
        }
    };
    if serde_json::to_vec(&response).unwrap().len() > types::FRAME_LIMIT {
        let mut error = ServiceError::new(
            "RESOURCE_LIMIT",
            "The actual driver response exceeds its byte limit",
        );
        if scope.write_dispatched() {
            error.code = "OUTCOME_UNKNOWN";
            error.unknown = true;
        }
        error.response(id)
    } else {
        response
    }
}
