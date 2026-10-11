use super::query_manager::{global_manager, ManagedError, ManagedJobSpec, QueryFilter};
use super::*;
use serde_json::Map;
use std::time::Duration;

pub(super) fn is_managed(name: &str) -> bool {
    matches!(
        name,
        "start_query"
            | "get_query_status"
            | "fetch_query_result"
            | "cancel_query"
            | "list_query_history"
            | "release_query_result"
    )
}

pub(super) fn schemas(default_format: &str) -> Vec<Tool> {
    let definitions = [
        ("start_query", "Execute one SELECT asynchronously and retain a bounded result snapshot", json!({
            "connection_id": {"type":"string"}, "query": {"type":"string","maxLength":262144},
            "max_rows": {"type":"integer","minimum":1,"maximum":10000,"default":10000},
            "timeout_ms": {"type":"integer","minimum":1000,"maximum":300000,"default":60000}
        }), vec!["connection_id", "query"]),
        ("get_query_status", "Get the state and result metadata of an owned query", json!({"query_id":{"type":"string"}}), vec!["query_id"]),
        ("fetch_query_result", "Read a retained result page without executing SQL again", json!({
            "query_id":{"type":"string"}, "result_set_index":{"type":"integer","minimum":0,"default":0},
            "offset":{"type":"integer","minimum":0,"default":0}, "limit":{"type":"integer","minimum":1,"maximum":1000,"default":100}
        }), vec!["query_id"]),
        ("cancel_query", "Request cancellation of an owned query; acceptance alone does not confirm termination", json!({"query_id":{"type":"string"}}), vec!["query_id"]),
        ("list_query_history", "List bounded MCP query metadata; previous-process results cannot be fetched or cancelled", json!({
            "connection_id":{"type":"string"}, "status":{"type":"string"},
            "since":{"type":"string","format":"date-time"}, "until":{"type":"string","format":"date-time"},
            "limit":{"type":"integer","minimum":1,"maximum":200,"default":50}, "cursor":{"type":"string"}
        }), vec![]),
        ("release_query_result", "Idempotently release a completed result while retaining its history", json!({"query_id":{"type":"string"}}), vec!["query_id"]),
    ];
    definitions.into_iter().map(|(name, description, mut properties, required)| {
        properties["output_format"] = output_format_schema(default_format);
        Tool { name: name.into(), description: Some(description.into()), input_schema: json!({
            "type":"object", "properties":properties, "required":required, "additionalProperties":false
        }) }
    }).collect()
}

fn invalid(message: &str) -> JsonRpcError {
    JsonRpcError {
        code: -32602,
        message: message.into(),
        data: Some(json!({"kind":"INVALID_ARGUMENT"})),
    }
}

fn domain(kind: &str, message: &str) -> JsonRpcError {
    JsonRpcError {
        code: -32000,
        message: message.into(),
        data: Some(json!({"kind":kind})),
    }
}

fn manager_error(error: ManagedError) -> JsonRpcError {
    JsonRpcError {
        code: if error.invalid_argument {
            -32602
        } else {
            -32000
        },
        message: error.message.into(),
        data: Some(json!({"kind":error.code})),
    }
}

fn optional_string<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    max_bytes: usize,
) -> Result<Option<&'a str>, JsonRpcError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= max_bytes => {
            Ok(Some(value))
        }
        _ => Err(invalid(&format!("Invalid {key}"))),
    }
}

fn required_string<'a>(
    args: &'a Map<String, Value>,
    key: &str,
    max_bytes: usize,
) -> Result<&'a str, JsonRpcError> {
    optional_string(args, key, max_bytes)?.ok_or_else(|| invalid(&format!("Missing {key}")))
}

fn integer(
    args: &Map<String, Value>,
    key: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, JsonRpcError> {
    let Some(value) = args.get(key) else {
        return Ok(default);
    };
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (min..=max).contains(value))
        .ok_or_else(|| invalid(&format!("Invalid {key}")))
}

fn date_filter(args: &Map<String, Value>, key: &str) -> Result<Option<String>, JsonRpcError> {
    optional_string(args, key, 64)?
        .map(|value| {
            chrono::DateTime::parse_from_rfc3339(value)
                .map(|time| time.with_timezone(&chrono::Utc).to_rfc3339())
                .map_err(|_| invalid(&format!("Invalid {key}: expected an RFC3339 timestamp")))
        })
        .transpose()
}

pub(super) async fn verify_connection_access(
    connection: &SavedConnection,
    config: &AppConfig,
) -> Result<Arc<dyn DatabaseDriver>, JsonRpcError> {
    let id = &connection.params.driver;
    if config
        .active_external_drivers
        .as_ref()
        .is_some_and(|ids| !ids.contains(id))
    {
        return Err(domain(
            "CONNECTION_ACCESS_CHANGED",
            "The database driver is disabled",
        ));
    }
    let driver = driver_registry::get_driver(id).await.ok_or_else(|| {
        domain(
            "CONNECTION_ACCESS_CHANGED",
            "The database driver is unavailable",
        )
    })?;
    let expected_version = match id.as_str() {
        "postgresql" => "1.0.0-rc.6",
        "sqlserver-plus" => "1.0.0-beta.3+plus.1",
        _ => {
            return Err(domain(
                "CANCELLATION_UNSUPPORTED",
                "Managed queries require a verified cancellable driver",
            ))
        }
    };
    if driver.manifest().version != expected_version {
        return Err(domain(
            "CANCELLATION_UNSUPPORTED",
            "This database driver version has no verified cancellation adapter",
        ));
    }
    let installed = plugins::manager::list_driver_manifests_from_disk().map_err(|_| {
        domain(
            "CONNECTION_ACCESS_CHANGED",
            "The installed database driver could not be verified",
        )
    })?;
    if !installed
        .iter()
        .any(|manifest| manifest.id == *id && manifest.version == expected_version)
    {
        return Err(domain(
            "CONNECTION_ACCESS_CHANGED",
            "The installed database driver changed",
        ));
    }
    Ok(driver)
}

async fn verify_query_access(query_id: &str, config: &AppConfig) -> Result<(), JsonRpcError> {
    let manager = global_manager();
    let status = manager.status(query_id).await.map_err(manager_error)?;
    let connection = find_connection(&status.connection_id).map_err(|_| {
        domain(
            "CONNECTION_ACCESS_CHANGED",
            "The connection is no longer available",
        )
    })?;
    let driver = verify_connection_access(&connection, config).await?;
    let manifest = driver.manifest();
    manager
        .verify_fingerprint(
            query_id,
            &connection_fingerprint(&connection, &manifest.id, &manifest.version),
        )
        .await
        .map_err(manager_error)
}

pub(super) async fn dispatch(
    name: &str,
    args: Option<&Map<String, Value>>,
    config: &AppConfig,
    session_id: &str,
) -> Result<Value, JsonRpcError> {
    let empty = Map::new();
    let args = args.unwrap_or(&empty);
    let schema = schemas("json")
        .into_iter()
        .find(|tool| tool.name == name)
        .ok_or_else(|| invalid("Unknown managed tool"))?;
    let properties = schema.input_schema["properties"]
        .as_object()
        .expect("managed schema has properties");
    if args.keys().any(|key| !properties.contains_key(key)) {
        return Err(invalid("Unknown argument"));
    }
    let format = requested_output_format(Some(args), config)
        .map_err(|_| invalid("Invalid output_format: expected json or toon"))?;
    let manager = global_manager();
    match name {
        "start_query" => {
            let id = required_string(args, "connection_id", 4096)?;
            let query = required_string(args, "query", sql_gate::MAX_MANAGED_SQL_BYTES)?;
            let max_rows = integer(args, "max_rows", 10_000, 1, 10_000)?;
            let timeout_ms = integer(args, "timeout_ms", 60_000, 1_000, 300_000)?;
            let connection = find_connection(id)
                .map_err(|_| domain("CONNECTION_NOT_FOUND", "The connection is unavailable"))?;
            if !matches!(
                connection.params.driver.as_str(),
                "postgresql" | "sqlserver-plus"
            ) {
                return Err(domain(
                    "CANCELLATION_UNSUPPORTED",
                    "Managed queries require a verified cancellable driver",
                ));
            }
            if driver_registry::get_driver(&connection.params.driver)
                .await
                .is_none()
                && RELOAD_COOLDOWN.elapsed()
            {
                plugins::manager::reload_plugins_from_disk_config().await;
            }
            let driver = verify_connection_access(&connection, config).await?;
            let manifest = driver.manifest();
            let dialect = manifest.capabilities.sql_dialect.ok_or_else(|| {
                domain(
                    "CANCELLATION_UNSUPPORTED",
                    "The driver does not declare a supported SQL dialect",
                )
            })?;
            sql_gate::validate_managed_select(query, dialect)
                .map_err(|_| invalid("Managed execution requires one read-only SELECT"))?;
            let fingerprint = connection_fingerprint(&connection, &manifest.id, &manifest.version);
            let spec = ManagedJobSpec {
                params: connection.params.clone(),
                connection,
                driver,
                active_driver_ids: config.active_external_drivers.clone(),
                config: config.clone(),
                query: query.to_string(),
                max_rows,
                timeout: Duration::from_millis(timeout_ms as u64),
                fingerprint,
                session_id: session_id.to_string(),
                client_hint: ai_activity::get_client_hint(),
                audit_enabled: config.ai_audit_enabled.unwrap_or(DEFAULT_AI_AUDIT_ENABLED),
                max_audit_entries: config
                    .ai_audit_max_entries
                    .unwrap_or(DEFAULT_AI_AUDIT_MAX_ENTRIES)
                    .clamp(1, 5000) as usize,
            };
            let status = manager.start(spec).await.map_err(manager_error)?;
            tool_result(
                &json!({"query_id":status.query_id,"state":"queued"}),
                format,
            )
        }
        "list_query_history" => {
            let connection_id = optional_string(args, "connection_id", 4096)?.map(str::to_string);
            let status = optional_string(args, "status", 64)?.map(str::to_string);
            let since = date_filter(args, "since")?;
            let until = date_filter(args, "until")?;
            if let (Some(from), Some(to)) = (&since, &until) {
                if chrono::DateTime::parse_from_rfc3339(from).unwrap()
                    > chrono::DateTime::parse_from_rfc3339(to).unwrap()
                {
                    return Err(invalid("since must not be after until"));
                }
            }
            let filter = QueryFilter {
                connection_id,
                status,
                since,
                until,
                limit: integer(args, "limit", 50, 1, 200)?,
            };
            let cursor = optional_string(args, "cursor", 128)?;
            let persisted = if config.ai_audit_enabled.unwrap_or(DEFAULT_AI_AUDIT_ENABLED) {
                tokio::task::spawn_blocking(ai_activity::read_query_history_events)
                    .await
                    .map_err(|_| domain("HISTORY_UNAVAILABLE", "Query history could not be read"))?
                    .map_err(|_| domain("HISTORY_UNAVAILABLE", "Query history could not be read"))?
            } else {
                Vec::new()
            };
            let page = manager
                .list_history(filter, persisted, cursor)
                .await
                .map_err(manager_error)?;
            tool_result(&page, format)
        }
        _ => {
            let id = required_string(args, "query_id", 64)?;
            uuid::Uuid::parse_str(id).map_err(|_| invalid("Invalid query_id"))?;
            let set = integer(args, "result_set_index", 0, 0, usize::MAX)?;
            let offset = integer(args, "offset", 0, 0, usize::MAX)?;
            let limit = integer(args, "limit", 100, 1, 1000)?;
            verify_query_access(id, config).await?;
            match name {
                "get_query_status" => {
                    tool_result(&manager.status(id).await.map_err(manager_error)?, format)
                }
                "fetch_query_result" => tool_result(
                    &manager
                        .fetch(id, set, offset, limit)
                        .await
                        .map_err(manager_error)?,
                    format,
                ),
                "cancel_query" => {
                    tool_result(&manager.cancel(id).await.map_err(manager_error)?, format)
                }
                "release_query_result" => {
                    tool_result(&manager.release(id).await.map_err(manager_error)?, format)
                }
                _ => Err(invalid("Unknown managed tool")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection_fixture() -> SavedConnection {
        serde_json::from_value(json!({
            "id":"fixture-id", "revision":"fixture-revision", "name":"fixture",
            "params":{"driver":"postgresql","host":"fixture.invalid","database":"fixture","username":"fixture-user"}
        })).unwrap()
    }

    #[test]
    fn 연결_대상과_인증_주체_수정은_이전_결과의_지문과_달라진다() {
        // given
        let connection = connection_fixture();
        let original = connection_fingerprint(&connection, "postgresql", "1.0.0-rc.6");
        let mut target = connection.clone();
        target.params.host = Some("changed.invalid".into());
        let mut identity = connection.clone();
        identity.params.extra.insert(
            "entra_object_id".into(),
            "different-fixture-identity".into(),
        );
        let mut revision = connection.clone();
        revision.revision = "new-revision".into();
        // when / then
        for changed in [target, identity, revision] {
            assert_ne!(
                connection_fingerprint(&changed, "postgresql", "1.0.0-rc.6"),
                original
            );
        }
        assert_ne!(
            connection_fingerprint(&connection, "postgresql", "different-version"),
            original
        );
    }

    #[test]
    fn 같은_대상의_정규화와_임시_비밀값은_지문을_바꾸지_않는다() {
        // given
        let connection = connection_fixture();
        let original = connection_fingerprint(&connection, "postgresql", "1.0.0-rc.6");
        let mut resolved = connection.clone();
        resolved.params.database =
            crate::models::DatabaseSelection::Multiple(vec!["fixture".into()]);
        resolved.params.password = Some("synthetic-password".into());
        resolved
            .params
            .extra
            .insert("access_token".into(), "synthetic-token".into());
        // when / then
        assert_eq!(
            connection_fingerprint(&resolved, "postgresql", "1.0.0-rc.6"),
            original
        );
    }

    #[tokio::test]
    async fn 잘못된_관리형_인수는_연결을_조회하기_전에_거부한다() {
        // given
        let cases = [
            json!({"connection_id":"unused","query":"SELECT 1","max_rows":0}),
            json!({"connection_id":"unused","query":"SELECT 1","timeout_ms":999}),
            json!({"connection_id":"unused","query":"SELECT 1","max_rows":1.5}),
            json!({"query_id":"invalid"}),
            json!({"unexpected":true}),
        ];
        // when / then
        for (index, args) in cases.iter().enumerate() {
            let name = if index < 3 {
                "start_query"
            } else {
                "get_query_status"
            };
            let error = dispatch(
                name,
                args.as_object(),
                &AppConfig::default(),
                "test-session",
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, -32602);
            assert_eq!(error.data.unwrap()["kind"], "INVALID_ARGUMENT");
        }
    }

    #[test]
    fn 이력_시간은_시간대가_포함된_표준_입력만_허용한다() {
        // given
        let good = json!({"since":"2026-10-11T09:00:00+09:00"});
        let bad = json!({"since":"yesterday"});
        // when / then
        assert_eq!(
            date_filter(good.as_object().unwrap(), "since")
                .unwrap()
                .unwrap(),
            "2026-10-11T00:00:00+00:00"
        );
        assert!(date_filter(bad.as_object().unwrap(), "since").is_err());
    }

    #[test]
    fn 경계값과_범위밖_정수를_구분한다() {
        // given
        let args = json!({"limit":1000,"offset":-1,"max_rows":10001});
        let args = args.as_object().unwrap();
        // when / then
        assert_eq!(integer(args, "limit", 100, 1, 1000).unwrap(), 1000);
        assert!(integer(args, "offset", 0, 0, usize::MAX).is_err());
        assert!(integer(args, "max_rows", 10000, 1, 10000).is_err());
    }
}
