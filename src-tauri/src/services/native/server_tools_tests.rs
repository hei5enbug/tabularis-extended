//! `query_explain`, `server_activity` and `server_cancel` end to end over the
//! scripted driver: the statements they run, the write policy around them and
//! the arguments they refuse.

use super::tests::{approve_waiting, Harness};
use super::*;
use crate::drivers::driver_trait::{PluginManifest, ServiceRpcMethod, SqlDialect};

fn caller(h: &Harness) -> CallerContext {
    h.core.caller(CallerTransport::Mcp).unwrap()
}
async fn call(h: &Harness, name: &str, arguments: Value) -> Value {
    h.core
        .transport
        .call_tool(name, arguments, caller(h), None)
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {:?}", error.code))
}
async fn refused(h: &Harness, name: &str, arguments: Value) -> ErrorCode {
    h.core
        .transport
        .call_tool(name, arguments, caller(h), None)
        .await
        .expect_err("the call should have been refused")
        .code
}
fn summary(result: &Value) -> &Value {
    &result["structuredContent"]
}
fn code(result: &Value) -> Option<&str> {
    summary(result)["error"]["code"].as_str()
}
fn dialect(dialect: SqlDialect, prefix: &'static str) -> impl FnOnce(&mut PluginManifest) {
    move |manifest| {
        manifest.id = format!("{prefix}-{}", uuid::Uuid::new_v4());
        manifest.capabilities.sql_dialect = Some(dialect);
    }
}
fn last_exact(h: &Harness) -> (String, bool) {
    let calls = h.state.calls.lock().unwrap();
    let call = calls
        .iter()
        .filter(|call| call.0 == ServiceRpcMethod::ExactQuery)
        .last()
        .unwrap();
    (call.1["text"].as_str().unwrap().to_owned(), call.2)
}
fn set_config(h: &Harness, config: &str) {
    crate::persistence::service_write_atomic(&h.directory.0.join("config.json"), config.as_bytes())
        .unwrap();
}
fn pending_approvals(h: &Harness) -> usize {
    crate::ai_approval::list_pending_in(&h.directory.0)
        .unwrap()
        .len()
}
async fn until_succeeded(h: &Harness, reply: &Value) -> Value {
    let job = json!({
        "job_id": summary(reply)["job_id"],
        "owner_instance_id": summary(reply)["owner_instance_id"],
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = call(h, "job_get", job.clone()).await;
            if summary(&state)["status"] == "succeeded" {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the job never succeeded")
}

#[tokio::test]
async fn server_activity는_방언에_맞는_조회문을_읽기전용으로_실행한다() {
    // given
    let postgres = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let mssql = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;

    // when
    let first = call(
        &postgres,
        "server_activity",
        json!({"connection_id":postgres.id,"limit":7}),
    )
    .await;
    let second = call(
        &mssql,
        "server_activity",
        json!({"connection_id":mssql.id,"include_idle":true}),
    )
    .await;

    // then
    assert_eq!(summary(&first)["status"], "succeeded", "{first}");
    assert_eq!(summary(&second)["status"], "succeeded", "{second}");
    let (statement, read_only) = last_exact(&postgres);
    assert!(statement.contains("FROM pg_stat_activity") && statement.contains("LIMIT 7"));
    assert!(statement.contains("IS DISTINCT FROM 'idle'") && read_only);
    let (statement, read_only) = last_exact(&mssql);
    assert!(
        statement.starts_with("SELECT TOP (50) ") && statement.contains("sys.dm_exec_sessions")
    );
    assert!(!statement.contains("IS NOT NULL") && read_only);
    assert_eq!(summary(&first)["effective"]["database"], "fixture");
}

#[tokio::test]
async fn server_activity는_범위를_벗어난_인자를_드라이버에_보내기_전에_거부한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let id = h.id.clone();
    let cases = [
        json!({"connection_id":id,"limit":0}),
        json!({"connection_id":id,"limit":201}),
        json!({"connection_id":id,"limit":"5"}),
        json!({"connection_id":id,"limit":2.5}),
        json!({"connection_id":id,"include_idle":"yes"}),
        json!({"connection_id":id,"write":true}),
        json!({"connection_id":id,"database":""}),
        json!({"limit":5}),
    ];

    // when
    let mut actual = Vec::new();
    for arguments in cases {
        actual.push(refused(&h, "server_activity", arguments).await);
    }

    // then
    assert!(actual
        .iter()
        .all(|code| *code == ErrorCode::InvalidArgument));
    assert!(h.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn 서버_도구는_두_방언이_아닌_드라이버에_지원불가_오류를_준다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Mysql, "mysql")).await;
    h.allow_write().await;

    // when
    let activity = refused(&h, "server_activity", json!({"connection_id":h.id})).await;
    let cancel = refused(
        &h,
        "server_cancel",
        json!({"connection_id":h.id,"process_id":5,"write":true}),
    )
    .await;

    // then
    assert_eq!(activity, ErrorCode::CapabilityUnavailable);
    assert_eq!(cancel, ErrorCode::CapabilityUnavailable);
    assert!(h.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn server_cancel은_승인된_정수_id로_방언별_종료문을_쓰기로_실행한다() {
    // given
    let postgres = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let mssql = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;
    postgres.allow_write().await;
    mssql.allow_write().await;

    // when
    let mut finished = Vec::new();
    for (h, id) in [(&postgres, 4242), (&mssql, 77)] {
        let reply = call(
            h,
            "server_cancel",
            json!({"connection_id":h.id,"process_id":id,"write":true,"wait_ms":0}),
        )
        .await;
        approve_waiting(h).await;
        finished.push(until_succeeded(h, &reply).await);
    }

    // then
    assert!(finished
        .iter()
        .all(|state| summary(state)["status"] == "succeeded"));
    assert_eq!(
        last_exact(&postgres),
        (
            "SELECT pg_cancel_backend(4242) AS cancelled".to_owned(),
            false
        )
    );
    assert_eq!(last_exact(&mssql), ("KILL 77".to_owned(), false));
    assert_eq!(postgres.state.executions.load(Ordering::SeqCst), 1);
    assert_eq!(mssql.state.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn server_cancel은_write가_참이_아니면_쓰기_의도_오류로_실행하지_않는다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    h.allow_write().await;

    // when
    let missing = call(
        &h,
        "server_cancel",
        json!({"connection_id":h.id,"process_id":9}),
    )
    .await;
    let denied = call(
        &h,
        "server_cancel",
        json!({"connection_id":h.id,"process_id":9,"write":false}),
    )
    .await;

    // then
    assert_eq!(code(&missing), Some("WRITE_INTENT_REQUIRED"));
    assert_eq!(code(&denied), Some("WRITE_INTENT_REQUIRED"));
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
    assert_eq!(pending_approvals(&h), 0);
}

#[tokio::test]
async fn server_cancel은_읽기전용_모드에서_승인을_묻기_전에_거부한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;
    h.allow_write().await;
    set_config(&h, r#"{"mcpReadonlyDefault":true}"#);

    // when
    let actual = call(
        &h,
        "server_cancel",
        json!({"connection_id":h.id,"process_id":9,"write":true,"wait_ms":0}),
    )
    .await;

    // then
    assert_eq!(code(&actual), Some("WRITE_NOT_ALLOWED"));
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
    assert_eq!(pending_approvals(&h), 0);
}

#[tokio::test]
async fn server_cancel은_정수가_아니거나_범위를_벗어난_process_id를_거부한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    h.allow_write().await;
    let id = h.id.clone();
    let cases = [
        json!({"connection_id":id,"write":true}),
        json!({"connection_id":id,"process_id":0,"write":true}),
        json!({"connection_id":id,"process_id":-1,"write":true}),
        json!({"connection_id":id,"process_id":2147483648_u64,"write":true}),
        json!({"connection_id":id,"process_id":"42","write":true}),
        json!({"connection_id":id,"process_id":"42; DROP TABLE t","write":true}),
        json!({"connection_id":id,"process_id":42.5,"write":true}),
        json!({"connection_id":id,"process_id":42.0,"write":true}),
        json!({"connection_id":id,"process_id":true,"write":true}),
        json!({"connection_id":id,"process_id":null,"write":true}),
        json!({"connection_id":id,"process_id":[42],"write":true}),
        json!({"connection_id":id,"process_id":42,"write":"true"}),
    ];

    // when
    let mut actual = Vec::new();
    for arguments in cases {
        actual.push(refused(&h, "server_cancel", arguments).await);
    }

    // then
    assert!(actual
        .iter()
        .all(|code| *code == ErrorCode::InvalidArgument));
    assert!(h.state.calls.lock().unwrap().is_empty());
    assert_eq!(pending_approvals(&h), 0);
}

#[tokio::test]
async fn query_explain은_계획만_읽어_요약하고_문장을_실행하지_않는다() {
    // given
    let postgres = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let mssql = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;

    // when
    let first = call(
        &postgres,
        "query_explain",
        json!({"connection_id":postgres.id,"sql":"SELECT * FROM orders;","schema":"sales"}),
    )
    .await;
    let second = call(
        &mssql,
        "query_explain",
        json!({"connection_id":mssql.id,"sql":"WITH c AS (SELECT 1 AS n) SELECT n FROM c"}),
    )
    .await;

    // then
    assert_eq!(summary(&first)["status"], "succeeded", "{first}");
    let data = &summary(&first)["data"];
    assert_eq!(data["executed"], false);
    assert_eq!(data["format"], "postgres-json");
    assert_eq!(data["summary"]["cost_unit"], "postgres_planner_cost");
    assert_eq!(data["summary"]["full_scans"][0]["object"], "orders");
    assert_eq!(data["plan"][0]["Plan"]["Node Type"], "Seq Scan");
    assert_eq!(summary(&first)["effective"]["schema"], "sales");
    let data = &summary(&second)["data"];
    assert_eq!(data["format"], "sqlserver-showplan-xml");
    assert_eq!(
        data["summary"]["cost_unit"],
        "sqlserver_estimated_subtree_cost"
    );
    assert_eq!(data["summary"]["full_scans"][0]["object"], "[dbo].[orders]");
    for h in [&postgres, &mssql] {
        let calls = h.state.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, ServiceRpcMethod::Explain);
        assert!(calls[0].2);
        assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
    }
    let sent = postgres.state.calls.lock().unwrap()[0].1.clone();
    assert_eq!(
        sent,
        json!({"sql":"SELECT * FROM orders;","schema":"sales"})
    );
}

#[tokio::test]
async fn query_explain은_계획을_받을_수_없는_문장을_드라이버에_보내기_전에_거부한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let statements = [
        "EXPLAIN SELECT 1",
        "EXPLAIN ANALYZE SELECT 1",
        "SET search_path = x",
        "EXEC sp_who",
        "CREATE TABLE t (a int)",
        "USE other",
        "BEGIN",
        "SELECT 1; SELECT 2",
        "SELECT 'a'; DROP TABLE t",
        "SELECT 1 /* ; */; DELETE FROM t",
        "-- only a comment",
    ];

    // when
    let mut actual = Vec::new();
    for sql in statements {
        let reply = call(&h, "query_explain", json!({"connection_id":h.id,"sql":sql})).await;
        actual.push((sql, code(&reply).map(str::to_owned)));
    }

    // then
    for (sql, code) in actual {
        assert_eq!(code.as_deref(), Some("INVALID_ARGUMENT"), "{sql}");
    }
    assert!(h.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn query_explain은_쓰기_인자를_받지_않고_빈_문장과_세션을_거부한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let id = h.id.clone();

    // when
    let write = refused(
        &h,
        "query_explain",
        json!({"connection_id":id,"sql":"SELECT 1","write":true}),
    )
    .await;
    let empty = refused(&h, "query_explain", json!({"connection_id":id,"sql":""})).await;
    let session = refused(
        &h,
        "query_explain",
        json!({"connection_id":id,"sql":"SELECT 1","session_id":"s-1"}),
    )
    .await;
    let extra = refused(
        &h,
        "query_explain",
        json!({"connection_id":id,"sql":"SELECT 1","analyze":true}),
    )
    .await;

    // then
    for code in [write, empty, session, extra] {
        assert_eq!(code, ErrorCode::InvalidArgument);
    }
    assert!(h.state.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn query_explain은_읽기전용_모드와_모든_승인_모드에서도_승인_없이_실행된다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    set_config(&h, r#"{"mcpReadonlyDefault":true,"mcpApprovalMode":"all"}"#);

    // when
    let actual = tokio::time::timeout(
        Duration::from_secs(5),
        call(
            &h,
            "query_explain",
            json!({"connection_id":h.id,"sql":"DELETE FROM orders WHERE id = 1"}),
        ),
    )
    .await
    .expect("the plan should not wait for approval");

    // then
    assert_eq!(summary(&actual)["status"], "succeeded", "{actual}");
    assert_eq!(summary(&actual)["data"]["executed"], false);
    assert_eq!(pending_approvals(&h), 0);
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 계획을_받는_기능이_없는_드라이버는_지원불가로_답한다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    h.state.explain_unsupported.store(true, Ordering::SeqCst);

    // when
    let actual = call(
        &h,
        "query_explain",
        json!({"connection_id":h.id,"sql":"SELECT 1"}),
    )
    .await;

    // then
    assert_eq!(code(&actual), Some("CAPABILITY_UNAVAILABLE"));
    assert!(h.state.calls.lock().unwrap().is_empty());
}
