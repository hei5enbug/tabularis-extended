#[path = "support/service_sqlserver/mod.rs"]
mod support;
use serde_json::{json, Value};
use support::{Fixture, Plugin};

#[test]
fn 제한된_principal의_select는_원본_metadata와_null_count를_반환한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.exact(
        "SELECT CAST(1 AS decimal(38,18)) AS precise,value FROM dbo.items",
        &fixture.reader,
        true,
        "read",
    );
    // when
    let actual = plugin.call(&request);
    // then
    assert_eq!(actual["result"]["rows"][0][0], "1");
    assert_eq!(actual["result"]["rows"][0][1], "initial");
    assert_eq!(actual["result"]["affected_rows"], Value::Null);
    assert_eq!(actual["result"]["column_metadata"][0]["scale"], 18);
    assert_eq!(
        actual["result"]["column_metadata"][0]["precision"],
        Value::Null
    );
    assert_eq!(
        actual["result"]["warnings"],
        json!(["affected_rows_unavailable", "column_precision_unavailable"])
    );
}

#[test]
fn 세션은_같은_physical_connection을_유지하고_rollback을_확인한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = plugin.call(&fixture.request(
        "service_session_open",
        json!({"database":fixture.database,"mode":"write"}),
        "sa",
        false,
        "open",
    ));
    let handle = opened["result"]["session_handle"].as_str().unwrap();
    let run = |plugin: &mut Plugin| {
        let mut query = fixture.exact(
            "UPDATE dbo.items SET value='session'; SELECT @@SPID AS spid",
            "sa",
            false,
            "query",
        );
        query["params"]["driver_context"]["session_handle"] = json!(handle);
        let first = plugin.call(&query);
        let mut next = fixture.exact("SELECT @@SPID AS spid", "sa", false, "next");
        next["params"]["driver_context"]["session_handle"] = json!(handle);
        let second = plugin.call(&next);
        let mut finish = fixture.request(
            "service_session_finish",
            json!({"action":"rollback"}),
            "sa",
            false,
            "rollback",
        );
        finish["params"]["driver_context"]["session_handle"] = json!(handle);
        let finished = plugin.call(&finish);
        let after = plugin.call(&fixture.exact(
            "SELECT value FROM dbo.items",
            &fixture.reader,
            true,
            "after",
        ));
        (first, second, finished, after)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(
        actual.0["result"]["rows"][0][0],
        actual.1["result"]["rows"][0][0]
    );
    assert_eq!(actual.2["result"]["rollback_confirmed"], true);
    assert_eq!(actual.3["result"]["rows"], json!([["initial"]]));
}
#[test]
fn readonly_세션의_권한_승격과_다른_주체를_거부하고_기존_세션을_보존한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = plugin.call(&fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "open",
    ));
    let handle = opened["result"]["session_handle"].as_str().unwrap();
    let run = |plugin: &mut Plugin| {
        let mut upgrade = fixture.exact(
            "UPDATE dbo.items SET value='upgrade'",
            &fixture.reader,
            false,
            "upgrade",
        );
        upgrade["params"]["driver_context"]["session_handle"] = json!(handle);
        let denied = plugin.call(&upgrade);
        let mut principal = fixture.exact("SELECT 1", &fixture.writer, true, "principal");
        principal["params"]["driver_context"]["session_handle"] = json!(handle);
        let wrong_principal = plugin.call(&principal);
        let mut read = fixture.exact("SELECT value FROM dbo.items", &fixture.reader, true, "read");
        read["params"]["driver_context"]["session_handle"] = json!(handle);
        (denied, wrong_principal, plugin.call(&read))
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "WRITE_NOT_ALLOWED");
    assert_eq!(actual.1["error"]["data"]["code"], "WRITE_NOT_ALLOWED");
    assert_eq!(actual.2["result"]["rows"], json!([["initial"]]));
}
#[test]
fn catalog와_generated_row는_native_lease와_정확한_count를_사용한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let table = json!({"database":fixture.database,"schema":"dbo","table":"items"});
    let run = |plugin: &mut Plugin| {
        let catalog = plugin.call(&fixture.request(
            "service_catalog",
            json!({"operation":"catalog.describe","table":table}),
            &fixture.reader,
            true,
            "catalog",
        ));
        let insert = plugin.call(&fixture.request(
            "service_row",
            json!({"operation":"row.insert","table":table,"values":{"id":2,"value":"created"}}),
            "sa",
            false,
            "insert",
        ));
        let update = plugin.call(&fixture.request("service_row", json!({"operation":"row.update","table":table,"identity":{"columns":["id"],"values":[2]},"changes":{"value":"changed"}}), "sa", false, "update"));
        let delete = plugin.call(&fixture.request("service_row", json!({"operation":"row.delete","table":table,"identity":{"columns":["id"],"values":[2]}}), "sa", false, "delete"));
        (catalog, insert, update, delete)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["columns"][0]["name"], "id");
    assert_eq!(actual.0["result"]["columns"][0]["is_pk"], true);
    assert_eq!(actual.0["result"]["columns"][0]["precision"], 10);
    assert_eq!(actual.1["result"]["affected_rows"], 1);
    assert_eq!(actual.2["result"]["affected_rows"], 1);
    assert_eq!(actual.3["result"]["affected_rows"], 1);
}
#[test]
fn 잘못된_frame_뒤에도_control과_서비스_negotiation을_처리한다() {
    // given
    let mut plugin = Plugin::new();
    let mut oversized = vec![b'x'; 16 * 1024 * 1024 + 1];
    oversized.push(b'\n');
    plugin.raw(&oversized);
    let oversized = plugin.receive(&Value::Null);
    plugin.raw(&[0xff, b'\n']);
    let invalid_utf8 = plugin.receive(&Value::Null);
    let request =
        json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"service_protocol":1}});
    // when
    let actual = plugin.call(&request);
    // then
    assert_eq!(oversized["error"]["code"], -32700);
    assert_eq!(invalid_utf8["error"]["code"], -32700);
    assert_eq!(
        actual["result"]["service_capabilities"]["exact_affected_rows"],
        false
    );
    assert!(actual["result"]["service_capabilities"]
        .get("readonly_guarantee")
        .is_none());
}
#[test]
fn logout은_인증값_없이_pool과_모든_세션을_무효화한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = plugin.call(&fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "open",
    ));
    let handle = opened["result"]["session_handle"].as_str().unwrap();
    let mut logout = fixture.request(
        "service_invalidate_auth",
        json!({}),
        &fixture.reader,
        true,
        "logout",
    );
    logout["params"]["driver_context"]
        .as_object_mut()
        .unwrap()
        .remove("auth");
    logout["params"]["params"] = json!({"driver":"sqlserver","connection_id":"service-fixture"});
    let run = |plugin: &mut Plugin| {
        let result = plugin.call(&logout);
        let mut next = fixture.exact("SELECT 1", &fixture.reader, true, "next");
        next["params"]["driver_context"]["session_handle"] = json!(handle);
        (result, plugin.call(&next))
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["invalidated"], true);
    assert_eq!(actual.1["error"]["data"]["code"], "INVALID_ARGUMENT");
}
#[test]
fn writable_user와_sa의_readonly_select도_probe_없이_실행한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let requests =
        [&fixture.writer, "sa"].map(|user| fixture.exact("SELECT 1 AS allowed", user, true, user));
    // when
    let actual = requests.map(|request| plugin.call(&request));
    // then
    assert!(actual
        .iter()
        .all(|response| response["result"]["rows"] == json!([[1]])));
}
#[test]
fn readonly_batch와_cte와_dynamic_write는_db_권한을_우회하지_못한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let texts = [
        "SELECT 1; UPDATE dbo.items SET value='batch'",
        "WITH target AS (SELECT * FROM dbo.items) UPDATE target SET value='cte'",
        "EXEC sys.sp_executesql N'UPDATE dbo.items SET value=''dynamic'''",
        "EXEC(N'UPDATE dbo.items SET value=''exec''')",
    ];
    let run = |plugin: &mut Plugin| {
        let failures = texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                plugin.call(&fixture.exact(text, &fixture.reader, true, &format!("blocked{index}")))
            })
            .collect::<Vec<_>>();
        let after = plugin.call(&fixture.exact(
            "SELECT value FROM dbo.items",
            &fixture.reader,
            true,
            "after",
        ));
        (failures, after)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert!(actual
        .0
        .iter()
        .all(|response| response["error"]["data"]["code"] == "WRITE_NOT_ALLOWED"));
    assert_eq!(actual.1["result"]["rows"], json!([["initial"]]));
}
#[test]
fn named_parameters는_문자열과_주석을_치환하지_않고_전달한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.request("execute_query_exact", json!({"text":"/* @value */ SELECT '@value' AS literal,@value AS actual,@n AS numeric,@empty AS empty","parameters":[{"name":"@value","value":"한글 ' OR 1=1 --"},{"name":"@n","value":42},{"name":"@empty","value":null}]}), &fixture.reader, true, "parameters");
    // when
    let actual = plugin.call(&request);
    // then
    assert_eq!(
        actual["result"]["rows"],
        json!([["@value", "한글 ' OR 1=1 --", 42, null]])
    );
}
#[test]
fn 빈_select와_추가_result_set의_metadata를_유지한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.exact("SELECT id,value FROM dbo.items WHERE 1=0; SELECT CAST(123.456 AS decimal(38,3)) AS precise", &fixture.reader, true, "empty");
    // when
    let actual = plugin.call(&request);
    // then
    assert_eq!(actual["result"]["columns"], json!(["id", "value"]));
    assert_eq!(actual["result"]["rows"], json!([]));
    assert_eq!(
        actual["result"]["column_metadata"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        actual["result"]["additional_results"][0]["rows"],
        json!([["123.456"]])
    );
    assert_eq!(
        actual["result"]["warnings"],
        json!(["affected_rows_unavailable", "column_precision_unavailable"])
    );
}

#[test]
fn 읽기_batch의_앞선_set_옵션과_declare도_모든_result_set을_반환한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.exact(
        "SET NOCOUNT ON; DECLARE @n int = 2; SELECT @n AS n; SELECT 1 AS one;",
        &fixture.reader,
        true,
        "set-read",
    );
    // when
    let actual = plugin.call(&request);
    // then
    assert!(actual.get("error").is_none(), "{actual}");
    assert_eq!(actual["result"]["columns"], json!(["n"]));
    assert_eq!(actual["result"]["rows"], json!([[2]]));
    assert_eq!(
        actual["result"]["additional_results"][0]["columns"],
        json!(["one"])
    );
    assert_eq!(
        actual["result"]["additional_results"][0]["rows"],
        json!([[1]])
    );
}

#[test]
fn 쓰기_작업의_앞선_set은_실행_전에_거부한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.exact("SET NOCOUNT ON; SELECT 1 AS one", "sa", false, "set-write");
    // when
    let actual = plugin.call(&request);
    // then
    assert_eq!(actual["error"]["data"]["code"], "UNSUPPORTED_OPERATION");
}

#[test]
fn 읽기_취소는_attention을_보내고_physical_connection을_폐기한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    fixture.attention_capture();
    plugin.send(&fixture.exact(
        "WAITFOR DELAY '00:00:05'; SELECT 1",
        &fixture.reader,
        true,
        "pending",
    ));
    let spid = support::wait_active(&mut plugin, &fixture);
    let connection = support::connection(&mut plugin, &fixture, spid);
    let cancel = json!({"jsonrpc":"2.0","id":"cancel","method":"cancel_request","params":{"connection_id":"service-fixture","request_id":"pending"}});
    let run = |plugin: &mut Plugin| {
        let ack = plugin.call(&cancel);
        let terminal = plugin.receive(&json!("pending"));
        let closed = support::gone(plugin, &fixture, &connection);
        let attention = support::attention(plugin, &fixture, spid);
        (ack, terminal, closed, attention)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["cancellation_requested"], true);
    assert_eq!(actual.1["error"]["data"]["code"], "CANCELLED");
    assert_eq!(actual.1["error"]["data"]["outcome"], "not_applied");
    assert!(actual.2);
    assert!(actual.3 >= 1);
}
#[test]
fn 쓰기_취소는_unknown이며_자동으로_다시_전송하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(
        &fixture.database,
        "CREATE TABLE dbo.audit(value nvarchar(100));",
    );
    plugin.send(&fixture.exact("INSERT dbo.audit VALUES('once'); WAITFOR DELAY '00:00:05'; UPDATE dbo.items SET value='late'", "sa", false, "write"));
    let spid = support::wait_active(&mut plugin, &fixture);
    let connection = support::connection(&mut plugin, &fixture, spid);
    let cancel = json!({"jsonrpc":"2.0","id":"cancel","method":"cancel_request","params":{"connection_id":"service-fixture","request_id":"write"}});
    let run = |plugin: &mut Plugin| {
        let ack = plugin.call(&cancel);
        let terminal = plugin.receive(&json!("write"));
        let closed = support::gone(plugin, &fixture, &connection);
        let after = plugin.call(&fixture.observe(
            "SELECT COUNT(*) FROM dbo.audit; SELECT value FROM dbo.items",
            "after",
        ));
        (ack, terminal, closed, after)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["cancellation_requested"], true);
    assert_eq!(actual.1["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert_eq!(actual.1["error"]["data"]["outcome"], "unknown");
    assert_eq!(actual.1["error"]["data"]["retryable"], false);
    assert!(actual.2);
    assert_eq!(actual.3["result"]["rows"], json!([[1]]));
    assert_eq!(
        actual.3["result"]["additional_results"][0]["rows"],
        json!([["initial"]])
    );
}
#[test]
fn deadline은_읽기_connection을_폐기하고_기한_초과를_반환한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut request = fixture.exact(
        "WAITFOR DELAY '00:00:05'; SELECT 1",
        &fixture.reader,
        true,
        "deadline",
    );
    request["params"]["driver_context"]["deadline_ms"] = json!(1500);
    plugin.send(&request);
    let spid = support::wait_active(&mut plugin, &fixture);
    let connection = support::connection(&mut plugin, &fixture, spid);
    let run = |plugin: &mut Plugin| {
        let terminal = plugin.receive(&request["id"]);
        let closed = support::gone(plugin, &fixture, &connection);
        (terminal, closed)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "DEADLINE_EXCEEDED");
    assert_eq!(actual.0["error"]["data"]["outcome"], "not_applied");
    assert!(actual.1);
}
#[test]
fn stdin_eof는_pending_쓰기를_취소하고_stdout과_stderr에_비밀을_노출하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(
        &fixture.database,
        "CREATE TABLE dbo.audit(value nvarchar(100));",
    );
    plugin.send(&fixture.exact(
        "INSERT dbo.audit VALUES('SQL_SECRET_CANARY'); WAITFOR DELAY '00:00:05'; SELECT 1",
        "sa",
        false,
        "write",
    ));
    let spid = support::wait_active(&mut plugin, &fixture);
    let connection = support::connection(&mut plugin, &fixture, spid);
    let run = |plugin: &mut Plugin| {
        plugin.eof();
        let terminal = plugin.receive(&json!("write"));
        let stderr = plugin.wait();
        let mut observer = Plugin::new();
        let count = support::scalar(
            &mut observer,
            &fixture,
            "SELECT COUNT(*) FROM dbo.audit",
            "after",
        );
        let closed = support::gone(&mut observer, &fixture, &connection);
        (terminal, stderr, count, closed)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert_eq!(actual.0["error"]["data"]["outcome"], "unknown");
    assert!(!actual.0.to_string().contains("SQL_SECRET_CANARY"));
    assert!(!actual.0.to_string().contains("Str0ng!Passw0rd"));
    assert!(actual.1.is_empty());
    assert_eq!(actual.2, 1);
    assert!(actual.3);
}
#[test]
fn sdk_오류는_sql과_비밀번호를_stdout과_stderr에_포함하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let request = fixture.exact(
        "RAISERROR('SQL_ERROR_SECRET_CANARY',16,1)",
        "sa",
        false,
        "error",
    );
    let run = |plugin: &mut Plugin| {
        let terminal = plugin.call(&request);
        plugin.eof();
        let stderr = plugin.wait();
        (terminal, stderr)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert!(!actual.0.to_string().contains("SQL_ERROR_SECRET_CANARY"));
    assert!(!actual.0.to_string().contains("Str0ng!Passw0rd"));
    assert!(actual.1.is_empty());
}

#[test]
fn 비예약_checkout은_새_login으로_temp와_set과_session_context를_격리한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let first = fixture.exact("SELECT CONVERT(nvarchar(36),connection_id) FROM sys.dm_exec_connections WHERE session_id=@@SPID; CREATE TABLE #private_state(id int); SET DATEFIRST 1; EXEC sys.sp_set_session_context @key=N'private_state',@value=N'synthetic'; SELECT CAST(SESSION_CONTEXT(N'private_state') AS nvarchar(100)),@@DATEFIRST,CASE WHEN OBJECT_ID('tempdb..#private_state') IS NOT NULL THEN 1 ELSE 0 END", "sa", false, "first");
    let next = fixture.exact("SELECT CONVERT(nvarchar(36),connection_id),OBJECT_ID('tempdb..#private_state'),@@DATEFIRST,CAST(SESSION_CONTEXT(N'private_state') AS nvarchar(100)) FROM sys.dm_exec_connections WHERE session_id=@@SPID", "sa", false, "next");
    let run = |plugin: &mut Plugin| (plugin.call(&first), plugin.call(&next));
    // when
    let actual = run(&mut plugin);
    // then
    assert!(actual.0.get("error").is_none());
    assert!(actual.1.get("error").is_none());
    assert_eq!(
        actual.0["result"]["additional_results"][0]["rows"],
        json!([["synthetic", 1, 1]])
    );
    assert_ne!(
        actual.0["result"]["rows"][0][0],
        actual.1["result"]["rows"][0][0]
    );
    assert_eq!(actual.1["result"]["rows"][0][1], Value::Null);
    assert_eq!(actual.1["result"]["rows"][0][2], 7);
    assert_eq!(actual.1["result"]["rows"][0][3], Value::Null);
}
#[test]
fn 다음_physical_login은_이전_password가_아닌_현재_인증을_사용한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let initial = plugin.call(&fixture.exact("SELECT 1", &fixture.reader, true, "first"));
    support::sql(
        "master",
        &format!(
            "ALTER LOGIN [{}] WITH PASSWORD='SyntheticRotated!42';",
            fixture.reader
        ),
    );
    let stale = fixture.exact("SELECT 1", &fixture.reader, true, "stale");
    let mut refreshed = fixture.exact("SELECT 2", &fixture.reader, true, "refreshed");
    refreshed["params"]["driver_context"]["auth"]["password"] = json!("SyntheticRotated!42");
    let run = |plugin: &mut Plugin| (plugin.call(&stale), plugin.call(&refreshed));
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(initial["result"]["rows"], json!([[1]]));
    assert_eq!(actual.0["error"]["data"]["code"], "AUTH_REQUIRED");
    assert_eq!(actual.1["result"]["rows"], json!([[2]]));
}
#[test]
fn write_startup_timeout도_unknown이며_이미_적용한_행을_재전송하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(
        &fixture.database,
        "CREATE TABLE dbo.audit(value nvarchar(100));",
    );
    let mut request = fixture.request("service_test", json!({}), "sa", false, "startup");
    request["params"]["params"]["startup_script"] =
        json!("INSERT dbo.audit VALUES('startup-once'); WAITFOR DELAY '00:00:05'");
    request["params"]["driver_context"]["deadline_ms"] = json!(2000);
    let run = |plugin: &mut Plugin| {
        let terminal = plugin.call(&request);
        let count = support::scalar(plugin, &fixture, "SELECT COUNT(*) FROM dbo.audit", "after");
        (terminal, count)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert_eq!(actual.0["error"]["data"]["outcome"], "unknown");
    assert_eq!(actual.0["error"]["data"]["retryable"], false);
    assert_eq!(actual.1, 1);
}
#[test]
fn 다른_연결의_동일한_request_id는_독립적으로_취소한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut left = fixture.exact(
        "WAITFOR DELAY '00:00:05'; SELECT 1",
        &fixture.reader,
        true,
        "left",
    );
    left["params"]["driver_context"]["connection_id"] = json!("left");
    left["params"]["driver_context"]["request_id"] = json!("shared");
    plugin.send(&left);
    let left_spid = support::wait_active(&mut plugin, &fixture);
    let left_connection = support::connection(&mut plugin, &fixture, left_spid);
    let mut right = fixture.exact(
        "WAITFOR DELAY '00:00:01'; SELECT 2",
        &fixture.reader,
        true,
        "right",
    );
    right["params"]["driver_context"]["connection_id"] = json!("right");
    right["params"]["driver_context"]["request_id"] = json!("shared");
    plugin.send(&right);
    let cancel = json!({"jsonrpc":"2.0","id":"cancel","method":"cancel_request","params":{"connection_id":"left","request_id":"shared"}});
    let run = |plugin: &mut Plugin| {
        let ack = plugin.call(&cancel);
        let left = plugin.receive(&json!("left"));
        let right = plugin.receive(&json!("right"));
        let closed = support::gone(plugin, &fixture, &left_connection);
        let next = plugin.call(&fixture.exact("SELECT 3", &fixture.reader, true, "next"));
        (ack, left, right, closed, next)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["cancellation_requested"], true);
    assert_eq!(actual.1["error"]["data"]["code"], "CANCELLED");
    assert_eq!(actual.2["result"]["rows"], json!([[2]]));
    assert!(actual.3);
    assert_eq!(actual.4["result"]["rows"], json!([[3]]));
}
#[test]
fn queue가_가득해도_control_cancel은_우회하고_connection당_네개만_실행한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut observer = Plugin::new();
    for index in 0..100 {
        let mut request = fixture.exact(
            "WAITFOR DELAY '00:00:10'; SELECT 1",
            "sa",
            false,
            &format!("queued{index}"),
        );
        request["params"]["driver_context"]["deadline_ms"] = json!(30_000);
        plugin.send(&request);
    }
    let active = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let count = support::scalar(&mut observer, &fixture, "SELECT COUNT(*) FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND wait_type='WAITFOR'", "active");
            if count >= 4 || std::time::Instant::now() >= deadline {
                break count;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let cancel = json!({"jsonrpc":"2.0","id":"cancel","method":"cancel_request","params":{"connection_id":"service-fixture","request_id":"queued60"}});
    let run = |plugin: &mut Plugin| {
        let started = std::time::Instant::now();
        let ack = plugin.call(&cancel);
        let elapsed = started.elapsed();
        let terminal = plugin.receive(&json!("queued60"));
        let overflow = plugin.receive(&json!("queued99"));
        plugin.eof();
        let stderr = plugin.wait();
        (ack, elapsed, terminal, overflow, stderr)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(active, 4);
    assert_eq!(actual.0["result"]["cancellation_requested"], true);
    assert!(actual.1 < std::time::Duration::from_secs(1));
    assert_eq!(actual.2["error"]["data"]["code"], "CANCELLED");
    assert_eq!(actual.2["error"]["data"]["outcome"], "not_applied");
    assert_eq!(actual.3["error"]["data"]["code"], "RESOURCE_LIMIT");
    assert!(actual.4.is_empty());
}
#[test]
fn 쓰기와_예약한_session의_overflow는_성공_prefix를_반환하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(&fixture.database, "CREATE TABLE dbo.audit(value int);");
    let opened = plugin.call(&fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "open",
    ));
    let mut rows = fixture.exact(
        "SELECT id FROM dbo.items UNION ALL SELECT id FROM dbo.items",
        &fixture.reader,
        true,
        "rows",
    );
    rows["params"]["input"]["max_rows"] = json!(1);
    rows["params"]["driver_context"]["session_handle"] = opened["result"]["session_handle"].clone();
    let mut stale = rows.clone();
    stale["id"] = json!("stale");
    stale["params"]["driver_context"]["request_id"] = json!("stale");
    let mut write = fixture.exact(
        "INSERT dbo.audit VALUES(1); SELECT id FROM dbo.items UNION ALL SELECT id FROM dbo.items",
        "sa",
        false,
        "write",
    );
    write["params"]["input"]["max_rows"] = json!(1);
    let bytes = fixture.exact("SELECT REPLICATE(CAST(N'a' AS nvarchar(max)),1000000) FROM (VALUES(1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11),(12),(13),(14),(15),(16),(17)) source(id)", "sa", false, "bytes");
    let run = |plugin: &mut Plugin| {
        let rows = plugin.call(&rows);
        let stale = plugin.call(&stale);
        let write = plugin.call(&write);
        let bytes = plugin.call(&bytes);
        let write_session = plugin.call(&fixture.request(
            "service_session_open",
            json!({"mode":"write"}),
            "sa",
            false,
            "write_open",
        ));
        let mut reserved = fixture.exact("INSERT dbo.audit VALUES(2); SELECT id FROM dbo.items UNION ALL SELECT id FROM dbo.items", "sa", false, "write_reserved");
        reserved["params"]["driver_context"]["session_handle"] =
            write_session["result"]["session_handle"].clone();
        reserved["params"]["input"]["max_rows"] = json!(1);
        let reserved = plugin.call(&reserved);
        let audit = support::scalar(plugin, &fixture, "SELECT COUNT(*) FROM dbo.audit", "audit");
        (rows, stale, write, bytes, audit, reserved)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"]["data"]["code"], "RESOURCE_LIMIT");
    assert_eq!(actual.0["error"]["data"]["details"]["session_closed"], true);
    assert_eq!(actual.1["error"]["data"]["code"], "INVALID_ARGUMENT");
    assert_eq!(actual.1["error"]["data"]["details"]["session_closed"], true);
    assert_eq!(actual.2["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert_eq!(actual.3["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert!(actual.0.get("result").is_none());
    assert!(actual.1.get("result").is_none());
    assert_eq!(actual.4, 1);
    assert_eq!(actual.5["error"]["data"]["code"], "OUTCOME_UNKNOWN");
    assert_eq!(actual.5["error"]["data"]["details"]["session_closed"], true);
}
#[test]
fn 여러_connection의_query도_global_여덟개를_넘겨_실행하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut observer = Plugin::new();
    for index in 0..20 {
        let mut request = fixture.exact(
            "WAITFOR DELAY '00:00:10'; SELECT 1",
            "sa",
            false,
            &format!("global{index}"),
        );
        request["params"]["driver_context"]["connection_id"] =
            json!(format!("connection{}", index % 3));
        request["params"]["driver_context"]["deadline_ms"] = json!(30_000);
        plugin.send(&request);
    }
    let count_query = "SELECT COUNT(*) FROM sys.dm_exec_requests WHERE database_id=DB_ID() AND wait_type='WAITFOR'";
    let active = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let count = support::scalar(&mut observer, &fixture, count_query, "active");
            if count >= 8 || std::time::Instant::now() >= deadline {
                break count;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let mut run = |plugin: &mut Plugin| {
        let still_active = support::scalar(&mut observer, &fixture, count_query, "before_eof");
        plugin.eof();
        let stderr = plugin.wait();
        let remaining = support::scalar(&mut observer, &fixture, count_query, "after_eof");
        (still_active, stderr, remaining)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(active, 8);
    assert_eq!(actual.0, 8);
    assert!(actual.1.is_empty());
    assert_eq!(actual.2, 0);
}
#[test]
fn binding의_네_세션이_pool을_점유해도_다른_connection은_진행한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = (0..4)
        .map(|index| {
            plugin.call(&fixture.request(
                "service_session_open",
                json!({"mode":"write"}),
                "sa",
                false,
                &format!("open{index}"),
            ))
        })
        .collect::<Vec<_>>();
    let mut fifth = fixture.request(
        "service_session_open",
        json!({"mode":"write"}),
        "sa",
        false,
        "fifth",
    );
    fifth["params"]["driver_context"]["deadline_ms"] = json!(1000);
    plugin.send(&fifth);
    let other = fixture.observe("SELECT 42", "independent");
    let run = |plugin: &mut Plugin| {
        let other = plugin.call(&other);
        let fifth = plugin.receive(&json!("fifth"));
        (other, fifth)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert!(opened
        .iter()
        .all(|response| response["result"]["in_transaction"] == true));
    assert_eq!(actual.0["result"]["rows"], json!([[42]]));
    assert_eq!(actual.1["error"]["data"]["code"], "DEADLINE_EXCEEDED");
    assert_eq!(actual.1["error"]["data"]["outcome"], "not_applied");
}
#[test]
fn pool_wait_뒤의_create도_대기한_request의_현재_인증을_사용한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = (0..4)
        .map(|index| {
            plugin.call(&fixture.request(
                "service_session_open",
                json!({"mode":"write"}),
                &fixture.writer,
                false,
                &format!("open{index}"),
            ))
        })
        .collect::<Vec<_>>();
    support::sql(
        "master",
        &format!(
            "ALTER LOGIN [{}] WITH PASSWORD='SyntheticWaitRotated!42';",
            fixture.writer
        ),
    );
    let mut fifth = fixture.request(
        "service_session_open",
        json!({"mode":"write"}),
        &fixture.writer,
        false,
        "waiting",
    );
    fifth["params"]["driver_context"]["auth"]["password"] = json!("SyntheticWaitRotated!42");
    plugin.send(&fifth);
    let mut release = fixture.request(
        "service_session_finish",
        json!({"action":"rollback"}),
        &fixture.writer,
        false,
        "release",
    );
    release["params"]["driver_context"]["session_handle"] =
        opened[0]["result"]["session_handle"].clone();
    let run = |plugin: &mut Plugin| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let finished = plugin.call(&release);
        let next = plugin.receive(&json!("waiting"));
        let mut read = fixture.exact("SELECT USER_NAME()", &fixture.writer, false, "current");
        read["params"]["driver_context"]["auth"]["password"] = json!("SyntheticWaitRotated!42");
        read["params"]["driver_context"]["session_handle"] =
            next["result"]["session_handle"].clone();
        (finished, next, plugin.call(&read))
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["rollback_confirmed"], true);
    assert_eq!(actual.1["result"]["in_transaction"], true);
    assert_eq!(actual.2["result"]["rows"], json!([[fixture.writer]]));
}
#[test]
fn 잘못된_service_rpc_id는_sql을_실행하기_전에_거부한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(&fixture.database, "CREATE TABLE dbo.audit(value int);");
    let requests = [
        json!({"opaque":"claim"}),
        json!("x".repeat(129)),
        json!(9_007_199_254_740_992_u64),
        json!(1.5),
    ]
    .map(|id| {
        let mut request = fixture.exact("INSERT dbo.audit VALUES(1)", "sa", false, "invalid");
        request["id"] = id;
        request
    });
    let run = |plugin: &mut Plugin| {
        let rejected = requests
            .iter()
            .map(|request| {
                plugin.send(request);
                plugin.receive(&Value::Null)
            })
            .collect::<Vec<_>>();
        let rows = support::scalar(plugin, &fixture, "SELECT COUNT(*) FROM dbo.audit", "after");
        (rejected, rows)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert!(actual.0.iter().all(|response| response["id"] == Value::Null
        && response["error"]["data"]["code"] == "INVALID_ARGUMENT"
        && response["error"]["data"]["outcome"] == "not_applied"));
    assert_eq!(actual.1, 0);
}
#[test]
fn stdout_pipe가_끊기면_pending_쓰기의_attention과_폐기를_확인한다() {
    // given
    let fixture = Fixture::new();
    let mut owner = support::PipeOwner::new();
    let mut observer = Plugin::new();
    fixture.attention_capture();
    support::sql(&fixture.database, "CREATE TABLE dbo.audit(value int);");
    owner.send(&fixture.exact(
        "INSERT dbo.audit VALUES(1); WAITFOR DELAY '00:00:05'; INSERT dbo.audit VALUES(2)",
        "sa",
        false,
        "write",
    ));
    let spid = support::wait_active(&mut observer, &fixture);
    let connection = support::connection(&mut observer, &fixture, spid);
    let mut run = |owner: &mut support::PipeOwner| {
        owner.fail_stdout();
        let stderr = owner.wait();
        let closed = support::gone(&mut observer, &fixture, &connection);
        let attention = support::attention(&mut observer, &fixture, spid);
        let rows = support::scalar(
            &mut observer,
            &fixture,
            "SELECT COUNT(*) FROM dbo.audit",
            "after",
        );
        (stderr, closed, attention, rows)
    };
    // when
    let actual = run(&mut owner);
    // then
    assert!(actual.0.is_empty());
    assert!(actual.1);
    assert!(actual.2 >= 1);
    assert_eq!(actual.3, 1);
}
#[test]
fn idle_session_discard는_pool_wait가_차지한_dispatch_대기를_우회한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let opened = (0..4)
        .map(|index| {
            plugin.call(&fixture.request(
                "service_session_open",
                json!({"mode":"write"}),
                "sa",
                false,
                &format!("open{index}"),
            ))
        })
        .collect::<Vec<_>>();
    for index in 0..4 {
        plugin.send(&fixture.request(
            "service_session_open",
            json!({"mode":"write"}),
            "sa",
            false,
            &format!("waiting{index}"),
        ));
    }
    let mut discard = fixture.request("service_session_discard", json!({}), "sa", false, "discard");
    discard["params"]["driver_context"]["session_handle"] =
        opened[0]["result"]["session_handle"].clone();
    let run = |plugin: &mut Plugin| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let start = std::time::Instant::now();
        let discarded = plugin.call(&discard);
        let elapsed = start.elapsed();
        let opened = plugin.receive_any(
            &(0..4)
                .map(|index| json!(format!("waiting{index}")))
                .collect::<Vec<_>>(),
        );
        plugin.eof();
        let stderr = plugin.wait();
        (discarded, elapsed, opened, stderr)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["in_transaction"], false);
    assert_eq!(actual.0["result"]["rollback_confirmed"], false);
    assert!(actual.1 < std::time::Duration::from_secs(1));
    assert_eq!(actual.2["result"]["in_transaction"], true);
    assert!(actual.3.is_empty());
}
#[test]
fn logout은_다른_connection의_활성_세션_query를_기다리거나_취소하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut target = fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "target",
    );
    target["params"]["driver_context"]["connection_id"] = json!("target");
    let target = plugin.call(&target);
    let mut other = fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "other",
    );
    other["params"]["driver_context"]["connection_id"] = json!("other");
    let other = plugin.call(&other);
    let mut pending = fixture.exact(
        "WAITFOR DELAY '00:00:01'; SELECT 42",
        &fixture.reader,
        true,
        "pending",
    );
    pending["params"]["driver_context"]["connection_id"] = json!("other");
    pending["params"]["driver_context"]["session_handle"] =
        other["result"]["session_handle"].clone();
    plugin.send(&pending);
    support::wait_active(&mut plugin, &fixture);
    let mut logout = fixture.request(
        "service_invalidate_auth",
        json!({}),
        &fixture.reader,
        true,
        "logout",
    );
    logout["params"]["driver_context"]["connection_id"] = json!("target");
    logout["params"]["driver_context"]["deadline_ms"] = json!(200);
    logout["params"]["driver_context"]
        .as_object_mut()
        .unwrap()
        .remove("auth");
    let mut stale = fixture.exact("SELECT 1", &fixture.reader, true, "stale");
    stale["params"]["driver_context"]["connection_id"] = json!("target");
    stale["params"]["driver_context"]["session_handle"] =
        target["result"]["session_handle"].clone();
    let run = |plugin: &mut Plugin| {
        let start = std::time::Instant::now();
        let logout = plugin.call(&logout);
        let elapsed = start.elapsed();
        let other = plugin.receive(&json!("pending"));
        let stale = plugin.call(&stale);
        (logout, elapsed, other, stale)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["invalidated"], true);
    assert!(actual.1 < std::time::Duration::from_millis(200));
    assert_eq!(actual.2["result"]["rows"], json!([[42]]));
    assert_eq!(actual.3["error"]["data"]["code"], "INVALID_ARGUMENT");
}
#[test]
fn 지원하는_variant_값과_실제_null을_무손실로_보존한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let requests = [
        fixture.exact(
            "SELECT CAST(42 AS sql_variant)",
            &fixture.reader,
            true,
            "variant",
        ),
        fixture.exact(
            "SELECT CAST(NULL AS sql_variant)",
            &fixture.reader,
            true,
            "null_variant",
        ),
    ];
    // when
    let actual = requests.map(|request| plugin.call(&request));
    // then
    assert_eq!(actual[0]["result"]["rows"], json!([[42]]));
    assert_eq!(actual[1]["result"]["rows"], json!([[null]]));
}
#[test]
fn 최소_logout은_혼잡한_target만_정리하고_새_login과_queued_sql을_실행하지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    plugin.call(&json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"settings":{"application_name":"A1 invalidate guard"}}}));
    let mut other_open = fixture.request(
        "service_session_open",
        json!({"mode":"read_only"}),
        &fixture.reader,
        true,
        "other_open",
    );
    other_open["params"]["driver_context"]["connection_id"] = json!("other");
    let other = plugin.call(&other_open);
    let mut other_query = fixture.exact(
        "WAITFOR DELAY '00:00:05'; SELECT 42",
        &fixture.reader,
        true,
        "other_active",
    );
    other_query["params"]["driver_context"]["connection_id"] = json!("other");
    other_query["params"]["driver_context"]["session_handle"] =
        other["result"]["session_handle"].clone();
    plugin.send(&other_query);
    for index in 0..4 {
        let mut query = fixture.exact(
            "WAITFOR DELAY '00:00:10'; SELECT 1",
            "sa",
            false,
            &format!("target{index}"),
        );
        query["params"]["driver_context"]["connection_id"] = json!("target");
        query["params"]["driver_context"]["deadline_ms"] = json!(30_000);
        plugin.send(&query);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while support::sql_scalar(&fixture.database, "SELECT COUNT(*) FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s ON r.session_id=s.session_id WHERE r.database_id=DB_ID() AND r.wait_type='WAITFOR' AND s.program_name='A1 invalidate guard'") != 5 { assert!(std::time::Instant::now() < deadline); std::thread::sleep(std::time::Duration::from_millis(10)); }
    support::sql("master", &format!("CREATE EVENT SESSION [{}] ON SERVER ADD EVENT sqlserver.login(WHERE ([sqlserver].[client_app_name]=N'A1 invalidate guard')), ADD EVENT sqlserver.rpc_starting(WHERE ([sqlserver].[like_i_sql_unicode_string]([statement],N'%a1_queued_canary%'))) ADD TARGET package0.ring_buffer; ALTER EVENT SESSION [{}] ON SERVER STATE=START;", fixture.database, fixture.database));
    let mut queued = fixture.exact(
        "SELECT 64211 AS a1_queued_canary",
        "sa",
        false,
        "target_queued",
    );
    queued["params"]["driver_context"]["connection_id"] = json!("target");
    plugin.send(&queued);
    let logout = json!({"jsonrpc":"2.0","id":"logout","method":"service_invalidate_auth","params":{"params":{"driver":"sqlserver","connection_id":"target"},"driver_context":{"protocol_version":1,"connection_id":"target","request_id":"logout","deadline_ms":10_000,"read_only":true},"input":{}}});
    let run = |plugin: &mut Plugin| {
        let started = std::time::Instant::now();
        let response = plugin.call(&logout);
        let elapsed = started.elapsed();
        let target = (0..4)
            .map(|index| plugin.receive(&json!(format!("target{index}"))))
            .collect::<Vec<_>>();
        let queued = plugin.receive(&json!("target_queued"));
        let other = plugin.receive(&json!("other_active"));
        let mut next = other_query.clone();
        next["id"] = json!("other_next");
        next["params"]["driver_context"]["request_id"] = json!("other_next");
        next["params"]["input"]["text"] = json!("SELECT 43");
        let other_next = plugin.call(&next);
        let logins = support::sql_scalar(&fixture.database, &format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"login\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database));
        let sent = support::sql_scalar(&fixture.database, &format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"rpc_starting\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database));
        let mut control = fixture.exact("SELECT 14", "sa", false, "login_control");
        control["params"]["driver_context"]["connection_id"] = json!("capture-control");
        let control = plugin.call(&control);
        let captured = support::sql_scalar(&fixture.database, &format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"login\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database));
        (
            response, elapsed, target, queued, other, logins, sent, other_next, control, captured,
        )
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["invalidated"], true);
    assert!(actual.1 < std::time::Duration::from_secs(1));
    assert!(actual
        .2
        .iter()
        .all(|response| response["error"]["data"]["code"] == "OUTCOME_UNKNOWN"));
    assert_eq!(actual.3["error"]["data"]["code"], "CANCELLED");
    assert_eq!(actual.3["error"]["data"]["outcome"], "not_applied");
    assert_eq!(actual.4["result"]["rows"], json!([[42]]));
    assert_eq!(actual.5, 0);
    assert_eq!(actual.6, 0);
    assert_eq!(actual.7["result"]["rows"], json!([[43]]));
    assert_eq!(actual.8["result"]["rows"], json!([[14]]));
    assert_eq!(actual.9, 1);
}
#[test]
fn row_변경은_실제_복합_pk를_검증한_뒤_한_행만_수정한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    support::sql(&fixture.database, "CREATE TABLE dbo.[key]]table]([a]]id] int,[b key] nvarchar(10),value nvarchar(100),PRIMARY KEY([b key],[a]]id])); INSERT dbo.[key]]table] VALUES(1,'a','same'),(2,'b','same'); CREATE TABLE dbo.keyless(id int,value nvarchar(100)); INSERT dbo.keyless VALUES(1,'same'),(2,'same');");
    support::sql(
        &fixture.database,
        "CREATE VIEW dbo.key_view AS SELECT [a]]id],[b key],value FROM dbo.[key]]table];",
    );
    support::sql("master", &format!("CREATE EVENT SESSION [{}] ON SERVER ADD EVENT sqlserver.rpc_starting(WHERE ([sqlserver].[like_i_sql_unicode_string]([statement],N'%UPDATE %') OR [sqlserver].[like_i_sql_unicode_string]([statement],N'%DELETE %'))) ADD TARGET package0.ring_buffer; ALTER EVENT SESSION [{}] ON SERVER STATE=START;", fixture.database, fixture.database));
    let table = json!({"database":fixture.database,"schema":"dbo","table":"key]table"});
    let keys = [
        json!({"columns":["value"],"values":["same"]}),
        json!({"columns":["a]id"],"values":[1]}),
        json!({"columns":["a]id","b key"],"values":[1,"a"]}),
        json!({"columns":["a]id","a]id"],"values":[1,1]}),
        json!({"columns":["b key","a]id"],"values":[null,1]}),
    ];
    let observed = format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"rpc_starting\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database);
    let run = |plugin: &mut Plugin| {
        let invalid = keys.iter().enumerate().map(|(index, identity)| plugin.call(&fixture.request("service_row", json!({"operation":"row.update","table":table,"identity":identity,"changes":{"value":"bad"}}), "sa", false, &format!("invalid{index}")))).collect::<Vec<_>>();
        let keyless = ["keyless", "key_view"].map(|name| plugin.call(&fixture.request("service_row", json!({"operation":"row.delete","table":{"database":fixture.database,"schema":"dbo","table":name},"identity":{"columns":["id"],"values":[1]}}), "sa", false, name)));
        let sent_before = support::sql_scalar(&fixture.database, &observed);
        let before = plugin.call(&fixture.exact(
            "SELECT value FROM dbo.[key]]table] ORDER BY [a]]id]",
            "sa",
            false,
            "before",
        ));
        let described = plugin.call(&fixture.request(
            "service_catalog",
            json!({"operation":"catalog.describe","table":table}),
            "sa",
            false,
            "describe",
        ));
        let nonkey = plugin.call(&fixture.request("service_catalog", json!({"operation":"catalog.describe","table":{"database":fixture.database,"schema":"dbo","table":"keyless"}}), "sa", false, "describe_keyless"));
        let missing = plugin.call(&fixture.request("service_catalog", json!({"operation":"catalog.describe","table":{"database":fixture.database,"schema":"dbo","table":"missing"}}), "sa", false, "describe_missing"));
        let mut keys = described["result"]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|column| {
                column["primary_key_position"]
                    .as_u64()
                    .map(|position| (position, column["name"].as_str().unwrap().to_owned()))
            })
            .collect::<Vec<_>>();
        keys.sort_by_key(|(position, _)| *position);
        let columns = keys.into_iter().map(|(_, name)| name).collect::<Vec<_>>();
        let identity = |values: Value| json!({"columns":columns,"values":values});
        let updated = plugin.call(&fixture.request("service_row", json!({"operation":"row.update","table":table,"identity":identity(json!(["a",1])),"changes":{"value":"changed"}}), "sa", false, "updated"));
        let absent = plugin.call(&fixture.request("service_row", json!({"operation":"row.update","table":table,"identity":identity(json!(["c",3])),"changes":{"value":"absent"}}), "sa", false, "absent"));
        let deleted = plugin.call(&fixture.request(
            "service_row",
            json!({"operation":"row.delete","table":table,"identity":identity(json!(["b",2]))}),
            "sa",
            false,
            "deleted",
        ));
        let delete_absent = plugin.call(&fixture.request(
            "service_row",
            json!({"operation":"row.delete","table":table,"identity":identity(json!(["c",3]))}),
            "sa",
            false,
            "delete_absent",
        ));
        let inserted = plugin.call(&fixture.request("service_row", json!({"operation":"row.insert","table":{"database":fixture.database,"schema":"dbo","table":"keyless"},"values":{"id":3,"value":"inserted"}}), "sa", false, "keyless_insert"));
        let after = plugin.call(&fixture.exact(
            "SELECT [a]]id],[b key],value FROM dbo.[key]]table]",
            "sa",
            false,
            "after",
        ));
        let sent_after = support::sql_scalar(&fixture.database, &observed);
        (
            invalid,
            keyless,
            sent_before,
            before,
            [updated, absent, deleted, delete_absent],
            inserted,
            after,
            sent_after,
            described,
            nonkey,
            missing,
        )
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert!(actual
        .0
        .iter()
        .all(
            |response| response["error"]["data"]["code"] == "INVALID_ARGUMENT"
                && response["error"]["data"]["outcome"] == "not_applied"
        ));
    assert!(actual
        .1
        .iter()
        .all(|response| response["error"]["data"]["code"] == "UNSUPPORTED_OPERATION"));
    assert_eq!(actual.2, 0);
    assert_eq!(actual.3["result"]["rows"], json!([["same"], ["same"]]));
    assert_eq!(
        actual
            .4
            .map(|response| response["result"]["affected_rows"].clone()),
        [json!(1), json!(0), json!(1), json!(0)]
    );
    assert_eq!(actual.5["result"]["affected_rows"], 1);
    assert_eq!(actual.6["result"]["rows"], json!([[1, "a", "changed"]]));
    assert_eq!(actual.7, 4);
    assert_eq!(
        actual.8["result"]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|column| (&column["name"], &column["primary_key_position"]))
            .collect::<Vec<_>>(),
        vec![
            (&json!("a]id"), &json!(2)),
            (&json!("b key"), &json!(1)),
            (&json!("value"), &Value::Null)
        ]
    );
    assert!(actual.9["result"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .all(|column| column["primary_key_position"] == Value::Null));
    assert_eq!(actual.10["result"]["columns"], json!([]));
}
#[test]
fn 독립_readonly는_더_많은_행을_확인한_때에만_저장한_prefix를_반환한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut overflow = fixture.exact(
        "SELECT id FROM (VALUES(1),(2),(3)) source(id) ORDER BY id",
        &fixture.reader,
        true,
        "overflow",
    );
    overflow["params"]["input"]["max_rows"] = json!(2);
    let mut exact = fixture.exact(
        "SELECT id FROM (VALUES(1),(2)) source(id) ORDER BY id",
        &fixture.reader,
        true,
        "exact",
    );
    exact["params"]["input"]["max_rows"] = json!(2);
    // when
    let actual = [overflow, exact].map(|request| plugin.call(&request));
    // then
    assert_eq!(actual[0]["result"]["rows"], json!([[1], [2]]));
    assert_eq!(actual[0]["result"]["truncated"], true);
    assert_eq!(
        actual[0]["result"]["limits"],
        json!({"truncated":true,"reasons":["rows"]})
    );
    assert_eq!(actual[1]["result"]["rows"], json!([[1], [2]]));
    assert_eq!(actual[1]["result"]["truncated"], false);
    assert_eq!(
        actual[1]["result"]["limits"],
        json!({"truncated":false,"reasons":[]})
    );
}
#[test]
fn readonly_byte_prefix는_전체_frame을_맞추고_metadata와_단일_초과행은_거부한다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut prefix = fixture.exact("SELECT REPLICATE(CAST(N'x' AS nvarchar(max)),1000) AS value FROM (VALUES(1),(2),(3)) source(id)", &fixture.reader, true, "byte_prefix");
    prefix["params"]["input"]["max_bytes"] = json!(3000);
    let mut metadata = fixture.exact(
        &format!("SELECT 1 AS [{}] WHERE 1=0", "m".repeat(128)),
        &fixture.reader,
        true,
        "metadata",
    );
    metadata["params"]["input"]["max_bytes"] = json!(100);
    let mut row = prefix.clone();
    row["id"] = json!("large_row");
    row["params"]["driver_context"]["request_id"] = json!("large_row");
    row["params"]["input"]["max_bytes"] = json!(600);
    let run = |plugin: &mut Plugin| {
        let prefix = plugin.call(&prefix);
        let size = serde_json::to_vec(&prefix).unwrap().len();
        (prefix, size, plugin.call(&metadata), plugin.call(&row))
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["result"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(
        actual.0["result"]["limits"],
        json!({"truncated":true,"reasons":["bytes"]})
    );
    assert!(actual.1 <= 3000);
    assert_eq!(actual.2["error"]["data"]["code"], "RESOURCE_LIMIT");
    assert_eq!(
        actual.2["error"]["message"],
        "The response metadata exceeds its byte limit"
    );
    assert_eq!(actual.3["error"]["data"]["code"], "RESOURCE_LIMIT");
    assert_eq!(
        actual.3["error"]["message"],
        "A single SQL row exceeds its byte limit"
    );
}
#[test]
fn readonly_prefix는_한번_실행한_물리를_폐기하고_cursor나_재송신을_남기지_않는다() {
    // given
    let fixture = Fixture::new();
    let mut plugin = Plugin::new();
    let mut observer = Plugin::new();
    support::sql("master", &format!("CREATE EVENT SESSION [{}] ON SERVER ADD EVENT sqlserver.rpc_starting(WHERE ([sqlserver].[like_i_sql_unicode_string]([statement],N'%a1_prefix_canary%'))) ADD TARGET package0.ring_buffer; ALTER EVENT SESSION [{}] ON SERVER STATE=START;", fixture.database, fixture.database));
    let mut request = fixture.exact("SELECT @@SPID AS spid,id AS a1_prefix_canary FROM (VALUES(1),(2)) source(id); WAITFOR DELAY '00:00:01'; SELECT @@SPID AS spid,3 AS a1_prefix_canary; RAISERROR(N'prefix flush',0,1) WITH NOWAIT; WAITFOR DELAY '00:00:10'; SELECT 4", &fixture.reader, true, "prefix");
    request["params"]["input"]["max_rows"] = json!(2);
    request["params"]["driver_context"]["deadline_ms"] = json!(30_000);
    plugin.send(&request);
    let spid = support::wait_active(&mut observer, &fixture);
    let connection = support::connection(&mut observer, &fixture, spid);
    let mut run = |plugin: &mut Plugin| {
        let started = std::time::Instant::now();
        let response = plugin.receive(&json!("prefix"));
        let elapsed = started.elapsed();
        let gone = support::gone(&mut observer, &fixture, &connection);
        let executions = support::sql_scalar(&fixture.database, &format!("SELECT CAST(t.target_data AS xml).value('count(/RingBufferTarget/event[@name=\"rpc_starting\"])','int') FROM sys.dm_xe_session_targets t JOIN sys.dm_xe_sessions s ON t.event_session_address=s.address WHERE s.name='{}'", fixture.database));
        (response, gone, executions, elapsed)
    };
    // when
    let actual = run(&mut plugin);
    // then
    assert_eq!(actual.0["error"], Value::Null);
    assert_eq!(actual.0["result"]["rows"].as_array().unwrap().len(), 2);
    assert_eq!(actual.0["result"]["truncated"], true);
    assert!(actual.0["result"].get("cursor").is_none());
    assert!(actual.1);
    assert_eq!(actual.2, 1);
    assert!(actual.3 < std::time::Duration::from_secs(3));
}
