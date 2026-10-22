use super::{
    super::{contracts::*, policy::ApprovalMode, sql_classifier::is_plannable_statement},
    support::*,
};
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn 단일_조회_계열_문장만_계획_대상으로_허용한다() {
    // given
    let allowed = [
        "SELECT 1",
        "select * from t;",
        "  (SELECT 1) UNION ALL (SELECT 2)",
        "WITH c AS (SELECT 1) SELECT * FROM c",
        "INSERT INTO t VALUES (1)",
        "UPDATE t SET a = 1",
        "DELETE FROM t",
        "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE;",
        "VALUES (1), (2)",
        "TABLE t",
        "/* lead */ SELECT 'a;b'",
        "SELECT 1 -- ; DELETE FROM t",
    ];

    // when
    let actual: Vec<bool> = allowed
        .iter()
        .map(|sql| is_plannable_statement(sql))
        .collect();

    // then
    assert!(actual.iter().all(|ok| *ok), "{actual:?}");
}

#[test]
fn 여러_문장이나_조회_계열이_아닌_문장은_계획_대상이_아니다() {
    // given
    let refused = [
        "",
        "   ",
        ";",
        "-- only a comment",
        "EXPLAIN SELECT 1",
        "explain analyze select 1",
        "SET search_path = x",
        "EXEC sp_who",
        "EXECUTE sp_who",
        "CALL refresh()",
        "CREATE TABLE t (a int)",
        "DROP TABLE t",
        "ALTER TABLE t ADD b int",
        "TRUNCATE t",
        "USE other",
        "BEGIN",
        "COMMIT",
        "SHOW TABLES",
        "SELECT 1; SELECT 2",
        "SELECT 1;; ",
        "SELECT 1; DROP TABLE t",
        "SELECT 'it''s'; DELETE FROM t",
        "SELECT '\\'; DELETE FROM t; --'",
        "SELECT [a']; DELETE FROM t; --",
        "SELECT $$ x $$; DELETE FROM t",
        "SELECT [a;b] FROM t",
        "SELECT $$a; b$$",
    ];

    // when
    let actual: Vec<bool> = refused
        .iter()
        .map(|sql| is_plannable_statement(sql))
        .collect();

    // then
    for (sql, plannable) in refused.iter().zip(actual) {
        assert!(!plannable, "{sql:?}");
    }
}

#[tokio::test]
async fn query_explain은_읽기_호출로_드라이버에_전달되고_승인을_묻지_않는다() {
    // given
    let harness = Harness::new();
    *harness.policy.mode.lock().unwrap() = ApprovalMode::All;
    let input = request(
        Operation::QueryExplain,
        json!({"sql":"SELECT * FROM orders","database":"fixture"}),
    );

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded, "{:?}", actual.error);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.policy.approvals.load(Ordering::SeqCst), 0);
    let contexts = harness.driver.contexts.lock().unwrap();
    assert!(contexts[0].1);
    assert!(contexts[0].2.is_none());
}

#[tokio::test]
async fn query_explain은_쓰기_권한이_없는_연결에서도_허용된다() {
    // given
    let harness = Harness::new();
    let input = request(Operation::QueryExplain, json!({"sql":"DELETE FROM orders"}));

    // when
    let actual = harness.call(&input).await;

    // then
    assert!(!harness.policy.writes_allowed.load(Ordering::SeqCst));
    assert_eq!(actual.status, JobStatus::Succeeded, "{:?}", actual.error);
    assert_eq!(harness.policy.approvals.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn query_explain은_부적합한_문장을_드라이버에_닿기_전에_거부한다() {
    // given
    let harness = Harness::new();
    let inputs = [
        "EXPLAIN SELECT 1",
        "SELECT 1; SELECT 2",
        "SET x = 1",
        "BEGIN",
    ]
    .map(|sql| request(Operation::QueryExplain, json!({"sql":sql})));

    // when
    let mut actual = Vec::new();
    for input in &inputs {
        actual.push(harness.call(input).await);
    }

    // then
    for response in actual {
        assert_eq!(response.error.unwrap().code, ErrorCode::InvalidArgument);
    }
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn query_explain은_세션_안에서는_실행하지_않는다() {
    // given
    let harness = Harness::new();
    let mut input = request(Operation::QueryExplain, json!({"sql":"SELECT 1"}));
    input.session_id = Some("session-1".into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::UnsupportedOperation);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}
