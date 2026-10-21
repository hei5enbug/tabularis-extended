//! The MCP tools end to end over a scripted driver: asynchronous replies,
//! control calls under load, cancellation, database selection, catalog kinds,
//! result paging, previews and job history.

use super::tests::{approve_waiting, Harness};
use super::*;
use crate::drivers::driver_trait::{PluginManifest, ServiceRpcMethod, SqlDialect};
use crate::services::contracts::JobStatus;
use crate::services::history::HistoryQuery;
use std::time::Instant;

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
async fn prompt(h: &Harness, name: &str, arguments: Value) -> Value {
    tokio::time::timeout(Duration::from_secs(1), call(h, name, arguments))
        .await
        .unwrap_or_else(|_| panic!("{name} did not answer promptly"))
}
fn summary(result: &Value) -> &Value {
    &result["structuredContent"]
}
fn code(result: &Value) -> Option<&str> {
    summary(result)["error"]["code"].as_str()
}
fn job(reply: &Value) -> Value {
    json!({
        "job_id": summary(reply)["job_id"],
        "owner_instance_id": summary(reply)["owner_instance_id"],
    })
}
async fn until(h: &Harness, job: &Value, wanted: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let state = call(h, "job_get", job.clone()).await;
            if summary(&state)["status"] == wanted {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the job never became {wanted}"))
}
async fn held_query(h: &Harness, text: &str) -> Value {
    h.state.hold.store(true, Ordering::SeqCst);
    call(
        h,
        "query_execute",
        json!({"connection_id":h.id,"text":text,"wait_ms":0}),
    )
    .await
}

#[tokio::test]
async fn query_execute는_실행이_끝나기_전에_작업_정보로_응답한다() {
    // given
    let h = Harness::new().await;
    // when
    let started = Instant::now();
    let reply = held_query(&h, "select 1").await;
    let elapsed = started.elapsed();
    // then
    assert!(elapsed < Duration::from_secs(1));
    assert!(matches!(
        summary(&reply)["status"].as_str(),
        Some("queued" | "running")
    ));
    assert!(summary(&reply)["job_id"].is_string());
    assert_eq!(
        summary(&reply)["owner_instance_id"],
        h.core.platform.runtime.jobs.instance_id()
    );
    assert!(summary(&reply)["data"].get("rows").is_none());
    h.state.wait_entered(1).await;
    h.state.release(1);
    let done = until(&h, &job(&reply), "succeeded").await;
    assert_eq!(summary(&done)["data"]["job_id"], summary(&reply)["job_id"]);
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 빨리_끝나는_작업은_첫_페이지와_유효_데이터베이스를_함께_돌려준다() {
    // given
    let h = Harness::new().await;
    // when
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1"}),
    )
    .await;
    // then
    let data = &summary(&reply)["data"];
    assert_eq!(summary(&reply)["status"], "succeeded");
    assert_eq!(data["rows"].as_array().unwrap().len(), 10);
    assert_eq!(data["offset"], 0);
    assert_eq!(data["next_offset"], 10);
    assert_eq!(data["has_more"], false);
    assert_eq!(data["stored_row_count"], 10);
    assert_eq!(data["truncated"], false);
    assert_eq!(data["readonly_guarantee"], "read_only_transaction");
    assert!(summary(&reply)["result_id"].is_string());
    assert_eq!(summary(&reply)["effective"]["database"], "fixture");
}

#[tokio::test]
async fn 승인을_기다리는_동안에도_응답하고_제어_호출은_즉시_답한다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    // when
    let started = Instant::now();
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"UPDATE records SET a = 1","write":true,"wait_ms":0}),
    )
    .await;
    let replied_in = started.elapsed();
    let pending = until(&h, &job(&reply), "awaiting_approval").await;
    let listed = prompt(&h, "job_list", json!({})).await;
    let catalog = tokio::time::timeout(Duration::from_secs(1), async {
        h.core.transport.tools().unwrap()
    })
    .await
    .unwrap();
    let cancelled = prompt(&h, "job_cancel", job(&reply)).await;
    let ended = until(&h, &job(&reply), "cancelled").await;
    // then
    assert!(replied_in < Duration::from_secs(1));
    assert_eq!(summary(&pending)["data"]["awaiting_approval"], true);
    assert!(summary(&listed)["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["job_id"] == summary(&reply)["job_id"]));
    assert_eq!(catalog["tools"].as_array().unwrap().len(), 49);
    assert!(summary(&cancelled)["error"].is_null());
    assert_eq!(summary(&ended)["data"]["cancel_state"], "confirmed");
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn 승인된_쓰기는_한번_실행되고_의도_없는_쓰기는_실행되지_않는다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    // when
    let rejected = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"UPDATE records SET a = 1"}),
    )
    .await;
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"UPDATE records SET a = 1","write":true,"wait_ms":0}),
    )
    .await;
    approve_waiting(&h).await;
    let done = until(&h, &job(&reply), "succeeded").await;
    // then
    assert_eq!(code(&rejected), Some("WRITE_INTENT_REQUIRED"));
    assert_eq!(summary(&done)["status"], "succeeded");
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
    assert!(!h.state.calls.lock().unwrap().last().unwrap().2);
}

#[tokio::test]
async fn 막힌_쿼리와_가득_찬_대기열에서도_제어_호출은_기다리지_않는다() {
    // given
    let h = Harness::new().await;
    let stored = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1"}),
    )
    .await;
    let result_id = summary(&stored)["result_id"].clone();
    let mut jobs = Vec::new();
    for _ in 0..6 {
        jobs.push(held_query(&h, "select 2").await);
    }
    h.state.wait_entered(4).await;
    // when
    let page = prompt(
        &h,
        "result_get",
        json!({"connection_id":h.id,"result_id":result_id,"limit":3}),
    )
    .await;
    let state = prompt(&h, "job_get", job(&jobs[5])).await;
    let listed = prompt(&h, "job_list", json!({"status":"running"})).await;
    let released = prompt(
        &h,
        "result_release",
        json!({"connection_id":h.id,"result_id":result_id}),
    )
    .await;
    // then
    assert_eq!(summary(&page)["data"]["rows"].as_array().unwrap().len(), 3);
    assert_eq!(summary(&state)["status"], "queued");
    assert!(
        summary(&listed)["data"]["entries"]
            .as_array()
            .unwrap()
            .len()
            >= 4
    );
    assert_eq!(summary(&released)["data"]["released"], true);
    h.state.release(6);
}

#[tokio::test]
async fn 취소는_드라이버가_확인한_뒤에만_cancelled이고_이어서_다시_실행하지_않는다() {
    // given
    let h = Harness::new().await;
    let reply = held_query(&h, "select 3").await;
    h.state.wait_entered(1).await;
    // when
    let cancelled = prompt(&h, "job_cancel", job(&reply)).await;
    let ended = until(&h, &job(&reply), "cancelled").await;
    // then
    assert_eq!(summary(&cancelled)["data"]["cancel_state"], "requested");
    assert_eq!(h.state.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(summary(&ended)["data"]["cancel_state"], "confirmed");
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
    assert_eq!(h.core.platform.runtime.jobs.active_count(), 0);
}

#[tokio::test]
async fn 한_작업을_취소해도_이웃_작업은_계속_실행되어_성공한다() {
    // given
    let h = Harness::new().await;
    let target = held_query(&h, "select 10").await;
    let neighbor = held_query(&h, "select 11").await;
    h.state.wait_entered(2).await;
    // when
    call(&h, "job_cancel", job(&target)).await;
    until(&h, &job(&target), "cancelled").await;
    let still = call(&h, "job_get", job(&neighbor)).await;
    h.state.release(1);
    let ended = until(&h, &job(&neighbor), "succeeded").await;
    // then
    assert_eq!(summary(&still)["status"], "running");
    assert!(summary(&still)["data"]["cancel_state"].is_null());
    assert_eq!(summary(&ended)["data"]["cancel_requested"], false);
    assert_eq!(h.state.cancels.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 드라이버가_취소를_확인하지_않으면_작업은_계속되고_완료가_이기면_성공으로_남는다() {
    // given
    let h = Harness::new().await;
    h.state.ignore_cancel.store(true, Ordering::SeqCst);
    let reply = held_query(&h, "select 4").await;
    h.state.wait_entered(1).await;
    // when
    let cancelled = call(
        &h,
        "job_cancel",
        json!({"job_id":job(&reply)["job_id"],"owner_instance_id":job(&reply)["owner_instance_id"],"wait_ms":200}),
    )
    .await;
    let still = call(&h, "job_get", job(&reply)).await;
    let held = h.core.platform.runtime.jobs.active_count();
    h.state.release(1);
    let ended = until(&h, &job(&reply), "succeeded").await;
    // then
    assert_eq!(code(&cancelled), Some("CANCEL_NOT_CONFIRMED"));
    assert_eq!(
        summary(&cancelled)["data"]["cancel_code"],
        "CANCEL_NOT_CONFIRMED"
    );
    assert_eq!(summary(&still)["status"], "running");
    assert_eq!(summary(&still)["data"]["cancel_state"], "not_confirmed");
    assert_eq!(held, 1);
    assert_eq!(summary(&ended)["data"]["cancel_requested"], true);
    assert_eq!(summary(&ended)["data"]["cancel_state"], "completed_first");
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while h.core.platform.runtime.jobs.active_count() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn 취소_요청을_보낼_수_없으면_즉시_확인불가로_기록하고_작업은_계속된다() {
    // given
    let h = Harness::new().await;
    h.state.cancel_fails.store(true, Ordering::SeqCst);
    let reply = held_query(&h, "select 5").await;
    h.state.wait_entered(1).await;
    // when
    let cancelled = prompt(&h, "job_cancel", job(&reply)).await;
    let state = prompt(&h, "job_get", job(&reply)).await;
    h.state.release(1);
    until(&h, &job(&reply), "succeeded").await;
    // then
    assert_eq!(code(&cancelled), Some("CANCEL_NOT_CONFIRMED"));
    assert_eq!(summary(&state)["status"], "running");
    assert_eq!(
        summary(&state)["data"]["cancel_code"],
        "CANCEL_NOT_CONFIRMED"
    );
}

#[tokio::test]
async fn 취소된_쓰기는_결과를_알_수_없다고_보고한다() {
    // given
    let h = Harness::new().await;
    h.allow_write().await;
    h.state.hold.store(true, Ordering::SeqCst);
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"UPDATE records SET a = 1","write":true,"wait_ms":0}),
    )
    .await;
    approve_waiting(&h).await;
    h.state.wait_entered(1).await;
    // when
    call(&h, "job_cancel", job(&reply)).await;
    let ended = until(&h, &job(&reply), "outcome_unknown").await;
    let stored = call(&h, "job_result", job(&reply)).await;
    // then
    assert_eq!(summary(&ended)["data"]["cancel_state"], "confirmed");
    let error = &summary(&stored)["data"]["response"]["error"];
    assert_eq!(error["code"], "CANCELLED");
    assert_eq!(error["outcome"], "unknown");
}

#[tokio::test]
async fn 기한이_끝나면_취소를_요청하고_확인되면_기한초과로_끝난다() {
    // given
    let h = Harness::new().await;
    h.core.platform.runtime.set_cancel_grace_ms(2_000);
    h.state.hold.store(true, Ordering::SeqCst);
    // when
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 6","wait_ms":0,"deadline_ms":200}),
    )
    .await;
    let ended = until(&h, &job(&reply), "failed").await;
    let stored = call(&h, "job_result", job(&reply)).await;
    // then
    assert_eq!(h.state.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(summary(&ended)["data"]["cancel_state"], "confirmed");
    assert_eq!(
        summary(&stored)["data"]["response"]["error"]["code"],
        "DEADLINE_EXCEEDED"
    );
}

#[tokio::test]
async fn 기한_뒤_취소_확인이_없으면_확인불가로_기록하고_호출이_끝날_때까지_자리를_지킨다() {
    // given
    let h = Harness::new().await;
    h.core.platform.runtime.set_cancel_grace_ms(150);
    h.state.hold.store(true, Ordering::SeqCst);
    h.state.ignore_cancel.store(true, Ordering::SeqCst);
    // when
    let reply = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 7","wait_ms":0,"deadline_ms":200}),
    )
    .await;
    let ended = until(&h, &job(&reply), "failed").await;
    let held = h.core.platform.runtime.jobs.active_count();
    h.state.release(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        while h.core.platform.runtime.jobs.active_count() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let stored = call(&h, "job_result", job(&reply)).await;
    // then
    assert_eq!(summary(&ended)["data"]["cancel_state"], "not_confirmed");
    assert_eq!(held, 1);
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        summary(&stored)["data"]["response"]["error"]["code"],
        "DEADLINE_EXCEEDED"
    );
    assert!(summary(&stored)["data"]["response"]["warnings"]
        .to_string()
        .contains("cancel_not_confirmed"));
}

fn dialect(dialect: SqlDialect, prefix: &'static str) -> impl FnOnce(&mut PluginManifest) {
    move |manifest| {
        manifest.id = format!("{prefix}-{}", uuid::Uuid::new_v4());
        manifest.capabilities.sql_dialect = Some(dialect);
    }
}

async fn selected(h: &Harness) {
    *h.state.databases.lock().unwrap() = vec!["fixture".into(), "analytics".into()];
    let reply = call(
        h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1","database":"analytics","schema":"reporting"}),
    )
    .await;
    assert_eq!(summary(&reply)["status"], "succeeded", "{reply}");
    assert_eq!(summary(&reply)["effective"]["database"], "analytics");
    assert_eq!(summary(&reply)["effective"]["schema"], "reporting");
    let seen = h.state.params_databases.lock().unwrap().clone();
    assert_eq!(seen.last().map(String::as_str), Some("analytics"));
    let executed = h
        .state
        .calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call.0 == ServiceRpcMethod::ExactQuery)
        .unwrap()
        .1
        .clone();
    assert_eq!(executed["schema"], "reporting");
    let objects = call(
        h,
        "catalog_objects",
        json!({"connection_id":h.id,"database":"analytics","schema":"reporting"}),
    )
    .await;
    assert_eq!(summary(&objects)["effective"]["database"], "analytics");
    assert_eq!(
        h.state.params_databases.lock().unwrap().last().unwrap(),
        "analytics"
    );
    let before = h.state.executions.load(Ordering::SeqCst);
    let invalid = call(
        h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1","database":"unlisted"}),
    )
    .await;
    assert_eq!(code(&invalid), Some("INVALID_ARGUMENT"));
    assert_eq!(h.state.executions.load(Ordering::SeqCst), before);
    let default = call(h, "catalog_schemas", json!({"connection_id":h.id})).await;
    assert_eq!(summary(&default)["effective"]["database"], "fixture");
}

#[tokio::test]
async fn postgresql_plus_연결은_고른_데이터베이스에서_실행하고_그대로_알린다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    // when / then
    selected(&h).await;
}

#[tokio::test]
async fn sqlserver_plus_연결은_고른_데이터베이스에서_실행하고_그대로_알린다() {
    // given
    let h = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;
    // when / then
    selected(&h).await;
}

#[tokio::test]
async fn 드라이버에_없는_개체_종류는_종류를_밝힌_지원불가로_답하고_빈_성공을_주지_않는다() {
    // given
    let h = Harness::with_manifest(|manifest| {
        manifest.capabilities.views = true;
        manifest.capabilities.materialized_views = false;
        manifest.capabilities.routines = false;
        manifest.capabilities.triggers = false;
    })
    .await;
    // when
    let mut failures = Vec::new();
    for kind in ["materialized_view", "routine", "trigger"] {
        let listed = call(
            &h,
            "catalog_objects",
            json!({"connection_id":h.id,"kind":kind}),
        )
        .await;
        let described = call(
            &h,
            "catalog_describe",
            json!({"connection_id":h.id,"table":"records","kind":kind}),
        )
        .await;
        failures.push((kind, listed, described));
    }
    let views = call(
        &h,
        "catalog_objects",
        json!({"connection_id":h.id,"kind":"view"}),
    )
    .await;
    let tables = call(
        &h,
        "catalog_objects",
        json!({"connection_id":h.id,"kind":"table"}),
    )
    .await;
    let described = call(
        &h,
        "catalog_describe",
        json!({"connection_id":h.id,"table":"records"}),
    )
    .await;
    // then
    for (kind, listed, described) in &failures {
        for result in [listed, described] {
            assert_eq!(result["isError"], true, "{kind}");
            assert_eq!(code(result), Some("UNSUPPORTED_OPERATION"), "{kind}");
            assert_eq!(summary(result)["error"]["details"]["object_kind"], *kind);
        }
    }
    assert_eq!(
        summary(&views)["data"]["objects"][0]["name"],
        "records_view"
    );
    assert_eq!(summary(&tables)["data"]["objects"][0]["name"], "records");
    assert_eq!(summary(&described)["status"], "succeeded");
    assert_eq!(summary(&described)["effective"]["schema"], "public");
}

#[tokio::test]
async fn 서비스가_다루지_않는_개체의_설명은_있는_종류여도_지원불가다() {
    // given
    let h = Harness::new().await;
    // when
    let actual = call(
        &h,
        "catalog_describe",
        json!({"connection_id":h.id,"table":"fn","kind":"routine"}),
    )
    .await;
    // then
    assert_eq!(code(&actual), Some("UNSUPPORTED_OPERATION"));
    assert_eq!(
        summary(&actual)["error"]["details"]["object_kind"],
        "routine"
    );
}

#[tokio::test]
async fn 만개_행_결과는_한번의_실행으로_빠짐없이_중복없이_육십사킬로바이트_이하로_나뉜다() {
    // given
    let h = Harness::new().await;
    h.state.rows.store(10_000, Ordering::SeqCst);
    h.state.cell_bytes.store(180, Ordering::SeqCst);
    let first = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select n from big","page_size":1000}),
    )
    .await;
    let result_id = summary(&first)["result_id"].as_str().unwrap().to_owned();
    // when
    let mut numbers: Vec<u64> = Vec::new();
    let mut sizes = vec![serde_json::to_vec(&first).unwrap().len()];
    let mut page = first;
    let mut pages = 1;
    loop {
        let data = &summary(&page)["data"];
        numbers.extend(
            data["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[0].as_u64().unwrap()),
        );
        assert_eq!(data["next_offset"].as_u64().unwrap(), numbers.len() as u64);
        if data["has_more"] != true {
            break;
        }
        page = call(
            &h,
            "result_get",
            json!({"connection_id":h.id,"result_id":result_id,"offset":numbers.len(),"limit":1000}),
        )
        .await;
        sizes.push(serde_json::to_vec(&page).unwrap().len());
        pages += 1;
    }
    // then
    assert_eq!(numbers, (0..10_000).collect::<Vec<u64>>());
    assert!(sizes.iter().all(|size| *size <= 64 * 1024), "{sizes:?}");
    assert!(pages > 20);
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.state
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.0 == ServiceRpcMethod::ExactQuery)
            .count(),
        1
    );
    let last = summary(&page)["data"].clone();
    assert_eq!(last["stored_row_count"], 10_000);
    assert_eq!(last["has_more"], false);
    assert_eq!(last["readonly_guarantee"], "read_only_transaction");
    assert!(last.get("truncated").is_some() && last.get("truncation_reason").is_some());
}

#[tokio::test]
async fn result_get은_범위를_벗어난_한도와_알수없는_결과를_거부한다() {
    // given
    let h = Harness::new().await;
    let stored = call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1"}),
    )
    .await;
    let result_id = summary(&stored)["result_id"].clone();
    // when
    let too_many = h
        .core
        .transport
        .call_tool(
            "result_get",
            json!({"connection_id":h.id,"result_id":result_id,"limit":1001}),
            caller(&h),
            None,
        )
        .await;
    let gone = call(
        &h,
        "result_get",
        json!({"connection_id":h.id,"result_id":"r-missing"}),
    )
    .await;
    // then
    assert_eq!(too_many.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(code(&gone), Some("RESULT_EXPIRED"));
}

fn last_statement(h: &Harness) -> String {
    h.state
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.0 == ServiceRpcMethod::ExactQuery)
        .last()
        .unwrap()
        .1["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn table_preview는_방언에_맞게_식별자를_인용해_읽기전용으로_실행한다() {
    // given
    let postgres = Harness::with_manifest(dialect(SqlDialect::Postgres, "postgresql-plus")).await;
    let mssql = Harness::with_manifest(dialect(SqlDialect::Mssql, "sqlserver-plus")).await;
    // when
    let first = call(
        &postgres,
        "table_preview",
        json!({"connection_id":postgres.id,"table":"we\"ird; drop table x","schema":"my schema","limit":5}),
    )
    .await;
    let second = call(
        &mssql,
        "table_preview",
        json!({"connection_id":mssql.id,"table":"or]ders","schema":"sales"}),
    )
    .await;
    // then
    assert_eq!(summary(&first)["status"], "succeeded", "{first}");
    assert_eq!(
        last_statement(&postgres),
        "SELECT * FROM \"my schema\".\"we\"\"ird; drop table x\" LIMIT 5"
    );
    assert!(postgres.state.calls.lock().unwrap().last().unwrap().2);
    assert_eq!(
        last_statement(&mssql),
        "SELECT TOP (10) * FROM [sales].[or]]ders]"
    );
    assert_eq!(summary(&second)["effective"]["schema"], "sales");
    assert_eq!(summary(&first)["data"]["rows"].as_array().unwrap().len(), 5);
    assert_eq!(
        summary(&second)["data"]["rows"].as_array().unwrap().len(),
        10
    );
}

#[tokio::test]
async fn table_preview는_한도와_이름을_검증하고_없는_데이터베이스를_거부한다() {
    // given
    let h = Harness::new().await;
    let rejected = |arguments: Value| {
        let h = &h;
        async move {
            h.core
                .transport
                .call_tool("table_preview", arguments, caller(h), None)
                .await
        }
    };
    // when
    let zero = rejected(json!({"connection_id":h.id,"table":"records","limit":0})).await;
    let large = rejected(json!({"connection_id":h.id,"table":"records","limit":1001})).await;
    let empty = rejected(json!({"connection_id":h.id,"table":""})).await;
    let control = rejected(json!({"connection_id":h.id,"table":"a\nb"})).await;
    let database = call(
        &h,
        "table_preview",
        json!({"connection_id":h.id,"table":"records","database":"unlisted"}),
    )
    .await;
    // then
    for result in [zero, large, empty, control] {
        assert_eq!(result.unwrap_err().code, ErrorCode::InvalidArgument);
    }
    assert_eq!(code(&database), Some("INVALID_ARGUMENT"));
    assert_eq!(h.state.executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn job_list는_실행중과_종료된_작업을_걸러서_커서로_빠짐없이_나눠_준다() {
    // given
    let h = Harness::new().await;
    for text in ["select 1", "select 2", "select 3"] {
        call(
            &h,
            "query_execute",
            json!({"connection_id":h.id,"text":text}),
        )
        .await;
    }
    let running = held_query(&h, "select 4").await;
    h.state.wait_entered(1).await;
    // when
    let everything = call(&h, "job_list", json!({"limit":200})).await;
    let only_running = call(&h, "job_list", json!({"status":"running"})).await;
    let other = call(&h, "job_list", json!({"connection_id":"someone-else"})).await;
    let mut collected = Vec::new();
    let mut cursor = Value::Null;
    loop {
        let mut arguments = json!({"limit":2});
        if !cursor.is_null() {
            arguments["cursor"] = cursor.clone();
        }
        let page = call(&h, "job_list", arguments).await;
        let data = &summary(&page)["data"];
        collected.extend(
            data["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["job_id"].as_str().unwrap().to_owned()),
        );
        cursor = data["next_cursor"].clone();
        if cursor.is_null() {
            break;
        }
    }
    let now = crate::services::storage::now_ms();
    let recent = call(
        &h,
        "job_list",
        json!({"since_ms":now - 60_000,"until_ms":now + 60_000}),
    )
    .await;
    let future = call(&h, "job_list", json!({"since_ms":now + 60_000})).await;
    let past = call(&h, "job_list", json!({"until_ms":now - 60_000})).await;
    // then
    assert_eq!(
        summary(&recent)["data"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(summary(&future)["data"]["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(summary(&past)["data"]["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    let entries = summary(&everything)["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|entry| entry.get("rows").is_none()));
    let live = entries
        .iter()
        .find(|entry| entry["job_id"] == summary(&running)["job_id"])
        .unwrap();
    assert_eq!(live["status"], "running");
    assert_eq!(live["cancellable"], true);
    assert_eq!(
        summary(&only_running)["data"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(code(&other), Some("CONNECTION_NOT_FOUND"));
    assert_eq!(collected.len(), 4);
    assert_eq!(
        collected
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    h.state.release(1);
}

#[tokio::test]
async fn job_list는_이전_감사_기록과_재시작_뒤의_끝나지_않은_작업을_함께_읽는다() {
    // given
    let h = Harness::new().await;
    crate::ai_activity::append_event_in(
        &h.directory.0,
        &crate::ai_activity::AiActivityEvent {
            id: "old".into(),
            session_id: "session".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            tool: "run_query".into(),
            connection_id: Some(h.id.clone()),
            connection_name: None,
            query: Some("SELECT old".into()),
            query_kind: Some("select".into()),
            duration_ms: 5,
            status: "success".into(),
            rows: Some(1),
            error: None,
            client_hint: None,
            approval_id: None,
        },
    )
    .unwrap();
    let running = held_query(&h, "select 8").await;
    h.state.wait_entered(1).await;
    // when
    let listed = call(&h, "job_list", json!({})).await;
    let restarted =
        crate::service_platform::QueryHistory::new(h.directory.0.clone(), "after-restart".into());
    let after = crate::services::history::HistoryStore::list(
        &restarted,
        &HistoryQuery::default(),
        &Default::default(),
    )
    .unwrap();
    // then
    let entries = summary(&listed)["data"]["entries"].as_array().unwrap();
    let legacy = entries
        .iter()
        .find(|entry| entry["legacy"] == true)
        .unwrap();
    assert_eq!(legacy["query"], "SELECT old");
    assert_eq!(legacy["cancellable"], false);
    assert!(entries.iter().any(
        |entry| entry["job_id"] == summary(&running)["job_id"] && entry["cancellable"] == true
    ));
    let lost = after
        .entries
        .iter()
        .find(|entry| entry.job_id == summary(&running)["job_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(lost.status, JobStatus::OutcomeUnknown);
    assert!(!lost.cancellable);
    h.state.release(1);
}

#[tokio::test]
async fn 감사를_끄면_job_list는_진행중_작업만_보여준다() {
    // given
    let h = Harness::new().await;
    std::fs::write(
        h.directory.0.join("config.json"),
        br#"{"mcpReadonlyDefault":false,"aiAuditEnabled":false}"#,
    )
    .unwrap();
    call(
        &h,
        "query_execute",
        json!({"connection_id":h.id,"text":"select 1"}),
    )
    .await;
    // when
    let finished = call(&h, "job_list", json!({})).await;
    let running = held_query(&h, "select 9").await;
    h.state.wait_entered(1).await;
    let live = call(&h, "job_list", json!({})).await;
    // then
    assert!(summary(&finished)["data"]["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        summary(&live)["data"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["job_id"] == summary(&running)["job_id"])
            .count(),
        1
    );
    h.state.release(1);
}
