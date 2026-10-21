use super::{
    super::{contracts::*, dispatch::*, policy::ApprovalMode},
    support::*,
};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

fn mcp() -> CallerContext {
    CallerContext {
        transport: CallerTransport::Mcp,
        ..caller()
    }
}
fn sql(text: &str) -> ServiceRequest {
    request(
        Operation::QueryExecute,
        json!({"language":"sql","text":text,"parameters":[],"page_size":10}),
    )
}
fn harness(read_only: bool, mode: ApprovalMode) -> Harness {
    let harness = Harness::new();
    harness
        .policy
        .writes_allowed
        .store(!read_only, Ordering::SeqCst);
    harness.policy.approved.store(true, Ordering::SeqCst);
    *harness.policy.mode.lock().unwrap() = mode;
    harness
}
async fn run(harness: &Harness, input: &ServiceRequest, write: bool) -> ServiceResponse {
    harness.policy.write_intent.store(write, Ordering::SeqCst);
    harness.runtime.call_validated(input, &mcp()).await
}

#[derive(Debug, PartialEq)]
enum Expect {
    Runs { approvals: usize },
    Fails(ErrorCode),
}
fn expected_for_statement(
    read_only: bool,
    mode: ApprovalMode,
    is_read: bool,
    write: bool,
) -> Expect {
    match (is_read, write) {
        (true, false) => Expect::Runs {
            approvals: usize::from(mode == ApprovalMode::All),
        },
        (true, true) => Expect::Fails(ErrorCode::InvalidArgument),
        (false, false) => Expect::Fails(ErrorCode::WriteIntentRequired),
        (false, true) if read_only => Expect::Fails(ErrorCode::WriteNotAllowed),
        (false, true) => Expect::Runs {
            approvals: usize::from(mode != ApprovalMode::Off),
        },
    }
}
fn observed(harness: &Harness, response: &ServiceResponse) -> (Expect, usize) {
    let calls = harness.driver.calls.load(Ordering::SeqCst);
    match &response.error {
        None => (
            Expect::Runs {
                approvals: harness.policy.approvals.load(Ordering::SeqCst),
            },
            calls,
        ),
        Some(error) => (Expect::Fails(error.code), calls),
    }
}

const MODES: [ApprovalMode; 3] = [
    ApprovalMode::Off,
    ApprovalMode::WritesOnly,
    ApprovalMode::All,
];

#[tokio::test]
async fn 읽기전용과_승인모드와_문장종류와_쓰기플래그_조합은_하나의_정책표를_따른다() {
    // given
    let statements = [
        ("SELECT 1", true),
        ("SELECT 'a;b'", true),
        ("UPDATE t SET a = 1", false),
        ("CREATE TABLE t (a INT)", false),
        ("VACUUM", false),
        ("SELECT * INTO t2 FROM t", false),
        ("EXEC sp_who", false),
        ("CALL refresh()", false),
        ("SELECT 1; DROP TABLE t", false),
    ];

    // when
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for read_only in [false, true] {
        for mode in MODES {
            for (text, is_read) in statements {
                for write in [false, true] {
                    let harness = harness(read_only, mode);
                    let response = run(&harness, &sql(text), write).await;
                    let (seen, calls) = observed(&harness, &response);
                    let want = expected_for_statement(read_only, mode, is_read, write);
                    let driver_calls = usize::from(matches!(want, Expect::Runs { .. }));
                    actual.push((read_only, mode, text, write, seen, calls));
                    expected.push((read_only, mode, text, write, want, driver_calls));
                }
            }
        }
    }

    // then
    assert_eq!(actual.len(), 108);
    assert_eq!(actual, expected);
}

fn row_inputs() -> Vec<(Operation, Value)> {
    let table = json!({"database":"fixture","schema":null,"table":"items"});
    let identity = json!({"columns":["id"],"values":[1]});
    let partition = json!([{"type":"string","value":"p"}]);
    let document_identity = json!({"id":"d","partition_key":partition});
    vec![
        (
            Operation::RowInsert,
            json!({"table":table,"values":{"id":1}}),
        ),
        (
            Operation::RowUpdate,
            json!({"table":table,"identity":identity,"changes":{"a":1}}),
        ),
        (
            Operation::RowDelete,
            json!({"table":table,"identity":identity}),
        ),
        (
            Operation::DocumentCreate,
            json!({"database":"d","container":"c","document":{"id":"d"},"partition_key":partition}),
        ),
        (
            Operation::DocumentReplace,
            json!({"database":"d","container":"c","identity":document_identity,"if_match":"e","document":{"id":"d"}}),
        ),
        (
            Operation::DocumentDelete,
            json!({"database":"d","container":"c","identity":document_identity,"if_match":"e"}),
        ),
    ]
}

#[tokio::test]
async fn 행과_문서_쓰기는_같은_정책표를_따른다() {
    // given
    let cases = row_inputs();

    // when
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for read_only in [false, true] {
        for mode in MODES {
            for (operation, input) in &cases {
                for write in [false, true] {
                    let harness = harness(read_only, mode);
                    let response = run(&harness, &request(*operation, input.clone()), write).await;
                    let want = match (write, read_only) {
                        (false, _) => Expect::Fails(ErrorCode::WriteIntentRequired),
                        (true, true) => Expect::Fails(ErrorCode::WriteNotAllowed),
                        (true, false) => Expect::Runs {
                            approvals: usize::from(mode != ApprovalMode::Off),
                        },
                    };
                    let calls = usize::from(matches!(want, Expect::Runs { .. }));
                    actual.push((
                        read_only,
                        mode,
                        *operation,
                        write,
                        observed(&harness, &response),
                    ));
                    expected.push((read_only, mode, *operation, write, (want, calls)));
                }
            }
        }
    }

    // then
    assert_eq!(actual, expected);
}

async fn open_write_session(harness: &Harness) -> ServiceResponse {
    let input = request(
        Operation::SessionOpen,
        json!({"database":"fixture","mode":"write"}),
    );
    harness.runtime.call_validated(&input, &mcp()).await
}

#[tokio::test]
async fn 쓰기_세션의_commit은_쓰기_의도가_있어야_실행한다() {
    // given
    let harness = harness(false, ApprovalMode::Off);
    let opened = open_write_session(&harness).await;
    let id = opened.data["session_id"].as_str().unwrap().to_owned();
    let mut commit = request(Operation::SessionCommit, json!({}));
    commit.session_id = Some(id);

    // when
    let without = run(&harness, &commit, false).await;
    let with = run(&harness, &commit, true).await;

    // then
    assert_eq!(without.error.unwrap().code, ErrorCode::WriteIntentRequired);
    assert_eq!(with.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn 읽기전용_연결은_쓰기_세션을_열기_전에_거부한다() {
    // given
    let harness = harness(true, ApprovalMode::Off);

    // when
    let actual = open_write_session(&harness).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.runtime.jobs.active_count(), 0);
}

#[tokio::test]
async fn 읽기전용_연결의_쓰기는_작업을_만들지_않는다() {
    // given
    let harness = harness(true, ApprovalMode::WritesOnly);

    // when
    let actual = run(&harness, &sql("DELETE FROM items"), true).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert!(actual.job_id.is_none());
    assert_eq!(harness.runtime.jobs.active_count(), 0);
}

#[tokio::test]
async fn 읽기_결과와_저장된_결과는_같은_읽기_전용_보장을_보고한다() {
    // given
    let harness = harness(true, ApprovalMode::Off);
    let executed = run(&harness, &sql("SELECT 1"), false).await;
    let mut get = request(
        Operation::ResultGet,
        json!({"result_id":executed.result_id.clone().unwrap(),"result_set_index":0,"offset":0,"limit":5}),
    );
    get.session_id = None;

    // when
    let stored = harness.runtime.call_validated(&get, &mcp()).await;

    // then
    assert_eq!(executed.data["readonly_guarantee"], "read_only_transaction");
    assert_eq!(stored.data["readonly_guarantee"], "read_only_transaction");
    assert_eq!(stored.data["stored_row_count"], 30);
}

#[tokio::test]
async fn 보장이_없는_드라이버의_저장된_결과는_분류_기준으로_표시한다() {
    // given
    let harness = harness(true, ApprovalMode::Off);
    harness.driver.readonly.store(false, Ordering::SeqCst);
    let executed = run(&harness, &sql("SELECT 1"), false).await;
    let get = request(
        Operation::ResultGet,
        json!({"result_id":executed.result_id.clone().unwrap(),"result_set_index":0,"offset":0,"limit":5}),
    );

    // when
    let stored = harness.runtime.call_validated(&get, &mcp()).await;

    // then
    assert_eq!(stored.data["readonly_guarantee"], "classification_only");
}

#[tokio::test]
async fn 취소_확인이_없는_쓰기는_기한_뒤_재시도없이_outcome_unknown이다() {
    // given
    let harness = harness(false, ApprovalMode::Off);
    let mut input = write_query("stuck");
    input.deadline_ms = 300;

    // when
    let actual = run(&harness, &input, true).await;

    // then
    assert_eq!(actual.status, JobStatus::OutcomeUnknown);
    assert_eq!(actual.error.unwrap().outcome, Outcome::Unknown);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn 승인은_한_번만_쓰이고_같은_요청을_다시_통과시키지_않는다() {
    // given
    let harness = harness(false, ApprovalMode::WritesOnly);
    let input = sql("DELETE FROM items");

    // when
    let first = run(&harness, &input, true).await;
    harness.policy.approved.store(false, Ordering::SeqCst);
    let again = run(&harness, &sql("DELETE FROM items"), true).await;

    // then
    assert_eq!(first.status, JobStatus::Succeeded);
    assert_eq!(again.error.unwrap().code, ErrorCode::WriteNotAllowed);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}
