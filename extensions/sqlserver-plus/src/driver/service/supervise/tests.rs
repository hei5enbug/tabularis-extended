use super::*;
use crate::driver::service::types::DriverContext;
use std::time::Duration;

const SECRET: &str = "synthetic-password-canary";

fn context(deadline_ms: u64) -> DriverContext {
    serde_json::from_value(json!({"protocol_version":1,"connection_id":uuid::Uuid::new_v4().to_string(),"request_id":"request","deadline_ms":deadline_ms})).unwrap()
}
fn on_worker_stack<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let _gate = registry::SHUTDOWN_GATE.read().unwrap();
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(future)
        })
        .unwrap()
        .join()
        .unwrap()
}
fn code(response: &Value) -> &str {
    response["error"]["data"]["code"].as_str().unwrap()
}
fn cancel_params(context: &DriverContext) -> Value {
    json!({"connection_id":context.connection_id,"request_id":context.request_id})
}
fn cancelled_error() -> ServiceError {
    ServiceError::new("CANCELLED", "SQL Server execution was cancelled")
}

#[test]
fn 진행_중인_실행은_취소_요청으로_확인된_cancelled를_반환한다() {
    // given
    let context = context(60_000);
    let params = cancel_params(&context);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        let action = async {
            scope.wait_cancelled().await;
            Err(cancelled_error())
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            acknowledge(json!("cancel"), &params)
        };
        let (response, ack) =
            tokio::join!(supervise(json!(7), None, &scope, action, SETTLE), canceller);
        (response, ack)
    });
    // then
    assert_eq!(code(&actual.0), "CANCELLED");
    assert_eq!(actual.0["error"]["data"]["outcome"], "not_applied");
    assert_eq!(actual.0["id"], 7);
    assert_eq!(actual.1["result"]["cancellation_requested"], true);
}

#[test]
fn 확인_응답은_종료를_주장하지_않는다() {
    // given
    let context = context(60_000);
    let _scope = Scope::reserve(&context).unwrap();
    // when
    let actual = acknowledge(json!(1), &cancel_params(&context));
    // then
    let mut keys = actual["result"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["acknowledged", "cancellation_requested"]);
    assert_eq!(actual["result"]["acknowledged"], true);
}

#[test]
fn 완료가_먼저면_취소_요청이_겹쳐도_성공을_유지한다() {
    // given
    let context = context(60_000);
    let params = cancel_params(&context);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        let action = async {
            acknowledge(json!(1), &params);
            Ok(json!({"rows":[[1]]}))
        };
        supervise(json!(3), None, &scope, action, SETTLE).await
    });
    // then
    assert_eq!(actual["result"]["rows"], json!([[1]]));
    assert!(actual.get("error").is_none());
}

#[test]
fn 완료한_뒤_도착한_취소와_중복_취소와_모르는_id는_확인만_하는_no_op이다() {
    // given
    let context = context(60_000);
    let params = cancel_params(&context);
    let unknown = json!({"connection_id":context.connection_id,"request_id":"never-registered"});
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        let response = supervise(
            json!(1),
            None,
            &scope,
            async { Ok(json!({"done":true})) },
            SETTLE,
        )
        .await;
        let late = acknowledge(json!("late"), &params);
        drop(scope);
        let after_drop = acknowledge(json!("after"), &params);
        let missing = acknowledge(json!("missing"), &unknown);
        (response, late, after_drop, missing)
    });
    // then
    assert_eq!(actual.0["result"]["done"], true);
    for ack in [&actual.1, &actual.2, &actual.3] {
        assert_eq!(ack["result"]["acknowledged"], true);
        assert_eq!(ack["result"]["cancellation_requested"], false);
    }
}

#[test]
fn 같은_요청의_중복_취소는_두_번째부터_no_op이다() {
    // given
    let context = context(60_000);
    let params = cancel_params(&context);
    let scope = Scope::reserve(&context).unwrap();
    // when
    let first = acknowledge(json!(1), &params);
    let second = acknowledge(json!(2), &params);
    // then
    assert_eq!(first["result"]["cancellation_requested"], true);
    assert_eq!(second["result"]["cancellation_requested"], false);
    assert!(scope.cancelled());
}

#[test]
fn 다른_연결의_같은_request_id_실행은_영향을_받지_않고_성공한다() {
    // given
    let first = context(60_000);
    let second = context(60_000);
    assert_eq!(first.request_id, second.request_id);
    let params = cancel_params(&first);
    // when
    let actual = on_worker_stack(async move {
        let a = Scope::reserve(&first).unwrap();
        let b = Scope::reserve(&second).unwrap();
        let cancelled = async {
            a.wait_cancelled().await;
            Err(cancelled_error())
        };
        let neighbor = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(json!({"neighbor":"ok"}))
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            acknowledge(json!("c"), &params)
        };
        let (first, second, _) = tokio::join!(
            supervise(json!("a"), None, &a, cancelled, SETTLE),
            supervise(json!("b"), None, &b, neighbor, SETTLE),
            canceller
        );
        (first, second, b.cancelled())
    });
    // then
    assert_eq!(code(&actual.0), "CANCELLED");
    assert_eq!(actual.1["result"]["neighbor"], "ok");
    assert!(!actual.2);
}

#[test]
fn 취소와_deadline과_sql_실패와_미확인_취소를_서로_구분한다() {
    // given
    let cancel_context = context(60_000);
    let cancel_params = cancel_params(&cancel_context);
    let deadline_context = context(40);
    let failure_context = context(60_000);
    let unconfirmed_context = context(60_000);
    let unconfirmed_params = self::cancel_params(&unconfirmed_context);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&cancel_context).unwrap();
        let cancelled = async {
            scope.wait_cancelled().await;
            Err(cancelled_error())
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            acknowledge(json!(0), &cancel_params)
        };
        let (cancelled, _) = tokio::join!(
            supervise(json!(1), None, &scope, cancelled, SETTLE),
            canceller
        );
        let scope = Scope::reserve(&deadline_context).unwrap();
        let deadline = supervise(
            json!(2),
            None,
            &scope,
            async {
                scope.wait_cancelled().await;
                Err(cancelled_error())
            },
            SETTLE,
        )
        .await;
        let scope = Scope::reserve(&failure_context).unwrap();
        let failure = supervise(
            json!(3),
            None,
            &scope,
            async {
                Err(ServiceError::new(
                    "WRITE_NOT_ALLOWED",
                    "SQL Server denied the requested database permission",
                ))
            },
            SETTLE,
        )
        .await;
        let scope = Scope::reserve(&unconfirmed_context).unwrap();
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            acknowledge(json!(0), &unconfirmed_params)
        };
        let (unconfirmed, _) = tokio::join!(
            supervise(
                json!(4),
                None,
                &scope,
                std::future::pending::<Result<Value, ServiceError>>(),
                Duration::from_millis(30),
            ),
            canceller
        );
        [cancelled, deadline, failure, unconfirmed]
    });
    // then
    let codes = actual.iter().map(code).collect::<Vec<_>>();
    assert_eq!(
        codes,
        [
            "CANCELLED",
            "DEADLINE_EXCEEDED",
            "WRITE_NOT_ALLOWED",
            "DRIVER_EXITED"
        ]
    );
    assert_eq!(
        actual[2]["error"]["message"],
        "SQL Server denied the requested database permission"
    );
    assert_eq!(
        actual[3]["error"]["message"],
        "SQL Server cancellation was not confirmed"
    );
}

#[test]
fn 취소된_deadline_미확인_실행은_deadline_초과로_보고한다() {
    // given
    let context = context(30);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        supervise(
            json!(1),
            None,
            &scope,
            std::future::pending::<Result<Value, ServiceError>>(),
            Duration::from_millis(30),
        )
        .await
    });
    // then
    assert_eq!(code(&actual), "DEADLINE_EXCEEDED");
}

#[test]
fn 취소와_겹친_sql_실패는_원래_코드를_유지한다() {
    // given
    let context = context(60_000);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        let action = async {
            registry::cancel(&context.connection_id, &context.request_id);
            Err(ServiceError::new(
                "WRITE_NOT_ALLOWED",
                "SQL Server denied the requested database permission",
            ))
        };
        supervise(json!(1), None, &scope, action, SETTLE).await
    });
    // then
    assert_eq!(code(&actual), "WRITE_NOT_ALLOWED");
}

#[test]
fn 쓰기_취소는_unknown이고_재전송_정보가_없다() {
    // given
    let context = context(60_000);
    let params = cancel_params(&context);
    // when
    let actual = on_worker_stack(async move {
        let scope = Scope::reserve(&context).unwrap();
        scope.mark_write();
        let action = async {
            scope.wait_cancelled().await;
            Err(cancelled_error())
        };
        let canceller = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            acknowledge(json!(0), &params)
        };
        tokio::join!(supervise(json!(1), None, &scope, action, SETTLE), canceller).0
    });
    // then
    assert_eq!(code(&actual), "OUTCOME_UNKNOWN");
    assert_eq!(actual["error"]["data"]["outcome"], "unknown");
    assert_eq!(actual["error"]["data"]["retryable"], false);
}

#[test]
fn 모든_반환_경로에서_registry_항목을_제거한다() {
    // given
    let ids = (0..5).map(|_| context(60_000)).collect::<Vec<_>>();
    let connections = ids
        .iter()
        .map(|c| c.connection_id.clone())
        .collect::<Vec<_>>();
    let params = cancel_params(&ids[1]);
    let expiring = context(20);
    let expiring_connection = expiring.connection_id.clone();
    // when
    let (active_before, active_after) = on_worker_stack(async move {
        let mut scopes = Vec::new();
        for context in &ids {
            scopes.push(Scope::reserve(context).unwrap());
        }
        let early = Scope::reserve(&expiring).unwrap();
        let before = connections
            .iter()
            .chain([&expiring_connection])
            .map(|id| registry::active_count(id))
            .sum::<usize>();
        supervise(json!(0), None, &scopes[0], async { Ok(json!(1)) }, SETTLE).await;
        supervise(
            json!(1),
            None,
            &scopes[1],
            async {
                acknowledge(json!(0), &params);
                Err(cancelled_error())
            },
            SETTLE,
        )
        .await;
        supervise(
            json!(2),
            None,
            &scopes[2],
            async { Err(ServiceError::argument("invalid")) },
            SETTLE,
        )
        .await;
        supervise(
            json!(3),
            None,
            &early,
            std::future::pending::<Result<Value, ServiceError>>(),
            Duration::from_millis(10),
        )
        .await;
        drop(scopes.remove(3));
        drop(scopes);
        drop(early);
        let after = connections
            .iter()
            .chain([&expiring_connection])
            .map(|id| registry::active_count(id))
            .sum::<usize>();
        (before, after)
    });
    // then
    assert_eq!(active_before, 6);
    assert_eq!(active_after, 0);
}

#[test]
fn 이미_정리된_scope의_drop은_새_실행의_항목을_지우지_않는다() {
    // given
    let first = context(60_000);
    let mut second = first.clone();
    second.deadline_ms = 50_000;
    let stale = Scope::reserve(&first).unwrap();
    drop(stale);
    // when
    let current = Scope::reserve(&second).unwrap();
    let ack = acknowledge(json!(1), &cancel_params(&second));
    // then
    assert_eq!(ack["result"]["cancellation_requested"], true);
    assert!(current.cancelled());
}

#[test]
fn 취소_입력_검증_오류는_원본_인자_오류로_응답한다() {
    // given
    let invalid = [
        json!({"connection_id":"c"}),
        json!({"connection_id":"c","request_id":""}),
        json!({"connection_id":"c","request_id":"r","extra":1}),
        json!(null),
    ];
    // when
    let actual = invalid.map(|params| acknowledge(json!(1), &params));
    // then
    for response in &actual {
        assert_eq!(code(response), "INVALID_ARGUMENT");
    }
}

#[test]
fn 오류_출력은_비밀과_접속_정보를_담지_않는다() {
    // given
    let connection = uuid::Uuid::new_v4().to_string();
    let params = json!({
        "params":{"driver":"sqlserver","host":"127.0.0.1","port":1,"username":"canary-user","password":SECRET,"database":"canary-db","ssl_mode":"disable"},
        "driver_context":{"protocol_version":1,"connection_id":connection,"request_id":"secrets","deadline_ms":30_000,"read_only":false,"auth":{"kind":"db_password","password":SECRET,"identity":"canary-identity"}},
        "input":{}
    });
    // when
    let actual = on_worker_stack(async move {
        crate::handlers::service::dispatch(json!("secrets"), &params, "service_test", None).await
    });
    // then
    let text = actual.to_string();
    assert!(actual.get("error").is_some());
    for canary in [
        SECRET,
        "canary-user",
        "canary-db",
        "canary-identity",
        "127.0.0.1",
    ] {
        assert!(!text.contains(canary), "{canary}");
    }
}
