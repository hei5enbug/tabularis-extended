use super::{
    super::{
        contracts::*,
        sql_classifier::{prove_read, read_dialect, ProvenRead, ReadDialect},
    },
    support::*,
};
use serde_json::json;
use std::sync::atomic::Ordering;

const PG_WRAPPER: &str = "BEGIN; SET LOCAL statement_timeout = '30s'; SET LOCAL transaction_read_only = on; SELECT 1; ROLLBACK;";
const TSQL_BATCH: &str =
    "SET NOCOUNT ON; DECLARE @n int = 10; SELECT TOP (@n) 1 AS a OPTION (MAXDOP 1);";

type Case<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

fn proven(text: &str, settings: &[(&str, &str)]) -> ProvenRead {
    ProvenRead {
        text: text.to_owned(),
        settings: settings
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

#[test]
fn 단일_읽기는_모든_방언에서_그대로_증명된다() {
    // given
    let dialects = [
        None,
        Some(ReadDialect::Postgres),
        Some(ReadDialect::SqlServer),
    ];
    let sql = "SELECT * FROM t WHERE a = 'x;y'";

    // when
    let actual = dialects.map(|dialect| prove_read(sql, dialect));

    // then
    for result in actual {
        assert_eq!(result, Some(proven(sql, &[])));
    }
}

#[test]
fn 드라이버_id로_읽기_방언을_정한다() {
    // given
    let drivers = [
        "postgresql-plus",
        "sqlserver-plus",
        "postgresql",
        "mysql",
        "",
    ];

    // when
    let actual = drivers.map(read_dialect);

    // then
    assert_eq!(
        actual,
        [
            Some(ReadDialect::Postgres),
            Some(ReadDialect::SqlServer),
            None,
            None,
            None
        ]
    );
}

#[test]
fn postgres_래퍼는_읽기_문장과_허용된_설정만_남긴다() {
    // given
    let cases: [Case; 17] = [
        (PG_WRAPPER, "SELECT 1", &[("statement_timeout", "30s")]),
        (
            "SET statement_timeout = '5s'; SELECT * FROM t",
            "SELECT * FROM t",
            &[("statement_timeout", "5s")],
        ),
        ("BEGIN READ ONLY; SELECT 1; COMMIT", "SELECT 1", &[]),
        (
            "SET search_path TO a, \"B\"; SELECT 1",
            "SELECT 1",
            &[("search_path", "a, \"B\"")],
        ),
        (
            "SET LOCAL lock_timeout TO 1500; SELECT 1",
            "SELECT 1",
            &[("lock_timeout", "1500")],
        ),
        (
            "SET plan_cache_mode = force_custom_plan; SELECT 1",
            "SELECT 1",
            &[("plan_cache_mode", "force_custom_plan")],
        ),
        (
            "/* a */ BEGIN; -- note; still comment\n SET LOCAL work_mem = '64MB'; /* x */ SELECT 1 /* y */; COMMIT WORK",
            "/* x */ SELECT 1 /* y */",
            &[("work_mem", "64MB")],
        ),
        (
            "START TRANSACTION READ ONLY; SET TRANSACTION READ ONLY; SELECT 1; END TRANSACTION",
            "SELECT 1",
            &[],
        ),
        (
            "BEGIN WORK; SET SESSION default_transaction_read_only TO 'on'; SET LOCAL jit = off; SET enable_seqscan = off; SELECT 1; ABORT",
            "SELECT 1",
            &[("jit", "off"), ("enable_seqscan", "off")],
        ),
        (
            "SET statement_timeout = 1; SET statement_timeout = 2; SELECT 1",
            "SELECT 1",
            &[("statement_timeout", "1"), ("statement_timeout", "2")],
        ),
        (
            "SET LOCAL statement_timeout = '1s'; SELECT 'a;b'",
            "SELECT 'a;b'",
            &[("statement_timeout", "1s")],
        ),
        (
            "SET LOCAL statement_timeout = '1s'; SELECT 1 AS \"a;b\"",
            "SELECT 1 AS \"a;b\"",
            &[("statement_timeout", "1s")],
        ),
        (
            "SET search_path = 'my''schema', public; SELECT 1",
            "SELECT 1",
            &[("search_path", "my'schema, public")],
        ),
        (
            "begin; set local STATEMENT_TIMEOUT = '1s'; select 1; commit",
            "select 1",
            &[("statement_timeout", "1s")],
        ),
        (
            "SET LOCAL idle_in_transaction_session_timeout = 5min; SELECT 1",
            "SELECT 1",
            &[("idle_in_transaction_session_timeout", "5min")],
        ),
        (
            "SET LOCAL transaction_read_only = 'TRUE'; SET LOCAL random_page_cost = 1.5; SELECT 1",
            "SELECT 1",
            &[("random_page_cost", "1.5")],
        ),
        (
            "BEGIN; SET LOCAL max_parallel_workers_per_gather = 0; SELECT 1; ROLLBACK TRANSACTION",
            "SELECT 1",
            &[("max_parallel_workers_per_gather", "0")],
        ),
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|(sql, _, _)| prove_read(sql, Some(ReadDialect::Postgres)))
        .collect();

    // then
    for ((sql, text, settings), result) in cases.iter().zip(actual) {
        assert_eq!(result, Some(proven(text, settings)), "{sql}");
    }
}

#[test]
fn postgres_래퍼가_읽기_증명을_못하면_거부한다() {
    // given
    let cases = [
        "BEGIN; SET LOCAL transaction_read_only = off; SELECT 1; ROLLBACK",
        "BEGIN; SET LOCAL transaction_read_only = 'maybe'; SELECT 1; ROLLBACK",
        "BEGIN READ WRITE; SELECT 1; COMMIT",
        "BEGIN ISOLATION LEVEL SERIALIZABLE; SELECT 1; COMMIT",
        "BEGIN DEFERRABLE; SELECT 1; COMMIT",
        "START; SELECT 1; COMMIT",
        "START TRANSACTION READ WRITE; SELECT 1; COMMIT",
        "BEGIN; SELECT 1; COMMIT AND CHAIN",
        "BEGIN; SELECT 1; COMMIT PREPARED 'x'",
        "BEGIN; SELECT 1; ROLLBACK TO SAVEPOINT a",
        "BEGIN; SELECT 1; SELECT 2; COMMIT",
        "BEGIN; DELETE FROM t; COMMIT",
        "BEGIN; SELECT * INTO x FROM t; COMMIT",
        "BEGIN; SELECT nextval('s'); COMMIT",
        "BEGIN; SELECT set_config('a', 'b', false); COMMIT",
        "BEGIN; SELECT 1; DELETE FROM t; COMMIT",
        "SET role admin; SELECT 1",
        "SET statement_timeout = DEFAULT; SELECT 1",
        "SET statement_timeout FROM CURRENT; SELECT 1",
        "SET statement_timeout = pg_sleep(1); SELECT 1",
        "SET statement_timeout = -1; SELECT 1",
        "SET statement_timeout = E'1s'; SELECT 1",
        "SET search_path = U&'a'; SELECT 1",
        "SET work_mem = \"x\"; SELECT 1",
        "SET work_mem = 1, 2; SELECT 1",
        "SET search_path = a,; SELECT 1",
        "SET LOCAL application_name = 'x'; SELECT 1",
        "SET SESSION AUTHORIZATION admin; SELECT 1",
        "RESET statement_timeout; SELECT 1",
        "BEGIN; SELECT 1; COMMIT; SELECT 2",
        "SELECT 1; BEGIN",
        "BEGIN; COMMIT",
        "SELECT 1;;",
        "SET LOCAL statement_timeout = '1s'; SELECT $$;$$",
        "SET LOCAL statement_timeout = '1s'; SELECT 'a\\'; DELETE FROM t; --'",
        "BEGIN; SELECT /*! 1 */ 1; COMMIT",
        "BEGIN; /* /* nested */ SELECT 1 */ ; COMMIT",
        "BEGIN; /* open SELECT 1",
        "SET search_path = a; SELECT * FROM orders; SET search_path = b",
        "BEGIN; SELECT 1; SET LOCAL transaction_read_only = on; COMMIT",
        "SET LOCAL statement_timeout = '1s'; SELECT 1 --x\rDELETE FROM t",
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|sql| prove_read(sql, Some(ReadDialect::Postgres)))
        .collect();

    // then
    for (sql, result) in cases.iter().zip(actual) {
        assert_eq!(result, None, "{sql}");
    }
}

#[test]
fn sql_server_배치는_읽기만_할_때_그대로_증명된다() {
    // given
    let cases = [
        TSQL_BATCH,
        "SET NOCOUNT ON; DECLARE @from date = '2026-01-01', @n int = 10; WITH x AS (SELECT 1 AS a) SELECT a FROM x OPTION (MAXDOP 1);",
        "SELECT @n = COUNT(*) FROM t; IF @n > 10 THROW 50000, 'too many', 1; SELECT * FROM t",
        "DECLARE @n int; SELECT @n = COUNT(*) FROM t; IF @n > 10 THROW 50000, 'too many', 1; SELECT * FROM t",
        "SET STATISTICS XML ON; SELECT 1",
        "SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED; SELECT * FROM t",
        "SET NOCOUNT ON; BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW; END CATCH",
        "BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW; END CATCH",
        "BEGIN SELECT 1 AS x END",
        "WAITFOR DELAY '00:00:01'; SELECT 1",
        "SELECT * FROM t OPTION (USE HINT('DISABLE_PARAMETER_SNIFFING'))",
        "DECLARE @t TABLE (id int); SELECT * FROM @t",
        "DECLARE @set int = 1, @end int = 2; SELECT @set, @end",
        "DECLARE @i int = 0; WHILE @i < 3 BEGIN SET @i = @i + 1; END; SELECT @i",
        "PRINT 'x'; RAISERROR('x', 10, 1) WITH NOWAIT; SELECT 1",
        "SET NOCOUNT ON; SELECT 'DELETE FROM t; EXEC x'",
        "SET NOCOUNT ON; SELECT LOG(10), [insert], REPLACE('a', 'b', 'c') FROM t",
        "SET LOCK_TIMEOUT 1000; SET XACT_ABORT ON; SELECT 1 WHERE 1 = 0",
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|sql| prove_read(sql, Some(ReadDialect::SqlServer)))
        .collect();

    // then
    for (sql, result) in cases.iter().zip(actual) {
        assert_eq!(result, Some(proven(sql, &[])), "{sql}");
    }
}

#[test]
fn sql_server_배치가_읽기_증명을_못하면_거부한다() {
    // given
    let cases = [
        "sp_configure 'x', 1; SELECT 1",
        "EXEC sp_who; SELECT 1",
        "SELECT 1; EXEC sp_who",
        "SELECT 1; EXECUTE ('DELETE FROM t')",
        "SET SHOWPLAN_XML ON; SELECT 1",
        "SET IMPLICIT_TRANSACTIONS ON; SELECT 1",
        "SET NOEXEC ON; SELECT 1",
        "SET FMTONLY ON; SELECT 1",
        "SET IDENTITY_INSERT t ON; SELECT 1",
        "SET CONTEXT_INFO 0x01; SELECT 1",
        "SET ROLE x; SELECT 1",
        "SET TRANSACTION READ ONLY; SELECT 1",
        "SET STATISTICS BATCH ON; SELECT 1",
        "BEGIN TRAN; SELECT 1; COMMIT",
        "BEGIN TRANSACTION; SELECT 1",
        "SELECT 1; SAVE TRAN x",
        "SELECT * INTO #t FROM t",
        "SET NOCOUNT ON; SELECT 1 AS a INTO #t",
        "DECLARE c CURSOR FOR SELECT 1; OPEN c",
        "DECLARE @c CURSOR; SELECT 1",
        "DECLARE @t TABLE (id int); INSERT INTO @t VALUES (1); SELECT * FROM @t",
        "RAISERROR('x', 16, 1) WITH LOG",
        "SELECT 1 DELETE FROM t",
        "SELECT NEXT VALUE FOR s",
        "SELECT * FROM OPENQUERY(srv, 'select 1')",
        "SELECT 1\nGO\nSELECT 2",
        "SELECT 1; KILL 5",
        "SELECT 1; DBCC CHECKDB",
        "SELECT 1; ENABLE TRIGGER x ON y",
        "WAITFOR (RECEIVE * FROM q)",
        "WAITFOR RECEIVE",
        "DROP TABLE t",
        "INSERT INTO t VALUES (1)",
        "CREATE TABLE t (id int)",
        "SET NOCOUNT ON; SELECT /*! 1 */ 1",
        "SET NOCOUNT ON; SELECT [x; DELETE FROM t] FROM y",
        "SET NOCOUNT ON; SELECT 'it\\'s'; DELETE FROM t; --'",
        "SET NOCOUNT ON; SELECT U&\"x\" FROM t",
        "SET NOCOUNT ON; SELECT * FROM t OPTION (USE PLAN_X)",
        "BEGIN TRAN; SELECT 1",
        "BEGIN TRANSACTION SELECT 1 COMMIT",
        "BEGIN TRY DELETE FROM t END TRY BEGIN CATCH THROW; END CATCH",
        "\"sp_updatestats\"; SELECT 1",
        "\"dbo\".\"mark\" SELECT 1",
        "PRINT$wipe; SELECT 1",
        "SELECT$wipe SELECT 1",
        "SET NOCOUNT ON; SELECT 1; --x\rDELETE FROM t;",
        "WITH c AS (SELECT 1 AS n) SELECT REPLACE('a', 'a', 'b') FROM c",
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .map(|sql| prove_read(sql, Some(ReadDialect::SqlServer)))
        .collect();

    // then
    for (sql, result) in cases.iter().zip(actual) {
        assert_eq!(result, None, "{sql}");
    }
}

#[test]
fn 방언이_없으면_배치와_래퍼를_모두_거부한다() {
    // given
    let cases = [
        PG_WRAPPER,
        "SET statement_timeout = '5s'; SELECT 1",
        TSQL_BATCH,
        "BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW; END CATCH",
    ];
    let dialect = read_dialect("mysql");

    // when
    let actual: Vec<_> = cases.iter().map(|sql| prove_read(sql, dialect)).collect();

    // then
    assert!(actual.iter().all(Option::is_none));
}

#[test]
fn 어느_방언도_쓰기와_ddl을_읽기로_증명하지_않는다() {
    // given
    let cases = [
        "DELETE FROM t",
        "UPDATE t SET a = 1",
        "INSERT INTO t VALUES (1)",
        "DROP TABLE t",
        "CREATE TABLE t (id int)",
        "SELECT * INTO copy FROM t",
        "SELECT nextval('s')",
        "WITH c AS (DELETE FROM t RETURNING *) SELECT * FROM c",
        "EXPLAIN ANALYZE DELETE FROM t",
        "BEGIN; DROP TABLE t; COMMIT",
    ];
    let dialects = [
        None,
        Some(ReadDialect::Postgres),
        Some(ReadDialect::SqlServer),
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .flat_map(|sql| dialects.map(|dialect| prove_read(sql, dialect)))
        .collect();

    // then
    assert!(actual.iter().all(Option::is_none));
}

#[test]
fn 숫자에_붙여_숨긴_문장은_어느_방언에서도_읽기가_아니다() {
    // given
    let cases = [
        "SELECT 1DELETE FROM t",
        "SELECT 0xFFUPDATE t SET a = 1",
        "SELECT 1e5DROP TABLE t",
        "WITH c AS (SELECT 1 AS a) SELECT 1DELETE FROM t",
        "EXPLAIN ANALYZE SELECT 1DELETE FROM t",
        "SET NOCOUNT ON; SELECT 1EXEC sp_who",
        "DECLARE @n int = 1; SELECT 2INSERT INTO t VALUES (1)",
        "BEGIN; SET LOCAL statement_timeout = '5s'; SELECT 1DELETE FROM t; ROLLBACK",
    ];
    let dialects = [
        None,
        Some(ReadDialect::Postgres),
        Some(ReadDialect::SqlServer),
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .flat_map(|sql| dialects.map(|dialect| prove_read(sql, dialect)))
        .collect();

    // then
    assert!(actual.iter().all(Option::is_none));
}

#[test]
fn 맨_앞_이름이나_짝없는_cr로_숨긴_실행은_단일_읽기로도_증명되지_않는다() {
    // given
    let cases = [
        "\"dbo\".\"mark\" SELECT 1",
        "\"mark\" SELECT 1",
        "SELECT$wipe SELECT 1",
        "SELECT_x SELECT 1",
        "SELECT 1 --x\rDELETE FROM t",
    ];
    let dialects = [
        None,
        Some(ReadDialect::Postgres),
        Some(ReadDialect::SqlServer),
    ];

    // when
    let actual: Vec<_> = cases
        .iter()
        .flat_map(|sql| dialects.map(|dialect| prove_read(sql, dialect)))
        .collect();

    // then
    assert!(actual.iter().all(Option::is_none));
}

#[test]
fn 주석과_괄호_뒤의_키워드와_crlf는_단일_읽기로_증명된다() {
    // given
    let sql = "-- note\r\n/* c */ (SELECT 1)\r\nUNION ALL (SELECT 2)";

    // when
    let actual = prove_read(sql, None);

    // then
    assert_eq!(actual, Some(proven(sql, &[])));
}

#[test]
fn 숫자_뒤에_키워드가_아닌_글자가_붙은_읽기는_그대로_증명된다() {
    // given
    let sql = "SELECT 1from_count, 2e5, 0xFF FROM t";

    // when
    let actual = prove_read(sql, Some(ReadDialect::SqlServer));

    // then
    assert_eq!(actual, Some(proven(sql, &[])));
}

#[test]
fn sql_server_배치는_offset_fetch_페이지_넘김을_읽기로_증명한다() {
    // given
    let sql = "DECLARE @skip int = 20; SELECT id FROM t ORDER BY id OFFSET @skip ROWS FETCH NEXT 10 ROWS ONLY;";

    // when
    let actual = prove_read(sql, Some(ReadDialect::SqlServer));

    // then
    assert_eq!(actual, Some(proven(sql, &[])));
}

#[test]
fn sql_server_배치의_커서_fetch는_거부한다() {
    // given
    let sql = "DECLARE @n int = 1; FETCH NEXT FROM c INTO @n;";

    // when
    let actual = prove_read(sql, Some(ReadDialect::SqlServer));

    // then
    assert_eq!(actual, None);
}

fn read_request(text: &str) -> ServiceRequest {
    request(
        Operation::QueryExecute,
        json!({"language":"sql","text":text,"parameters":[],"page_size":10}),
    )
}

fn harness_for(driver: &str) -> Harness {
    let harness = Harness::new();
    {
        let mut saved = harness.connections.saved.lock().unwrap();
        let connection = saved.get_mut("conn-a").unwrap();
        connection.driver = driver.into();
        connection.settings =
            json!({"host":"localhost","database":"fixture","integrated_auth":"true"});
    }
    harness
}

async fn rejected(cases: &[(&str, &str)]) -> Vec<(ErrorCode, usize)> {
    let mut actual = Vec::new();
    for (driver, text) in cases {
        let harness = harness_for(driver);
        harness.driver.readonly.store(false, Ordering::SeqCst);
        let response = harness.call(&read_request(text)).await;
        actual.push((
            response.error.unwrap().code,
            harness.driver.calls.load(Ordering::SeqCst),
        ));
    }
    actual
}

#[tokio::test]
async fn postgresql_plus_연결의_읽기_래퍼는_쓰기_의도_없이_읽기로_실행한다() {
    // given
    let harness = harness_for("postgresql-plus");

    // when
    let actual = harness.call(&read_request(PG_WRAPPER)).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data["readonly_guarantee"], "read_only_transaction");
    assert!(harness.driver.contexts.lock().unwrap()[0].1);
}

#[tokio::test]
async fn postgresql_plus_세션_안의_읽기_래퍼는_쓰기_의도를_요구한다() {
    // given
    let harness = harness_for("postgresql-plus");
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"read_only"}),
        ))
        .await;
    let mut input = read_request(PG_WRAPPER);
    input.session_id = Some(opened.data["session_id"].as_str().unwrap().into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.error.unwrap().code, ErrorCode::WriteIntentRequired);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn postgresql_plus_세션_안의_단일_읽기는_그대로_실행한다() {
    // given
    let harness = harness_for("postgresql-plus");
    let opened = harness
        .call(&request(
            Operation::SessionOpen,
            json!({"database":"fixture","mode":"read_only"}),
        ))
        .await;
    let mut input = read_request("SELECT 1");
    input.session_id = Some(opened.data["session_id"].as_str().unwrap().into());

    // when
    let actual = harness.call(&input).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(harness.driver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sqlserver_plus_읽기_보장이_없어도_읽기_배치는_두_관문을_통과한다() {
    // given
    let harness = harness_for("sqlserver-plus");
    harness.driver.readonly.store(false, Ordering::SeqCst);

    // when
    let actual = harness.call(&read_request(TSQL_BATCH)).await;

    // then
    assert_eq!(actual.status, JobStatus::Succeeded);
    assert_eq!(actual.data["readonly_guarantee"], "classification_only");
    assert!(harness.driver.contexts.lock().unwrap()[0].1);
}

#[tokio::test]
async fn 다른_드라이버나_맞지_않는_방언이면_배치를_쓰기_의도_없이_거부한다() {
    // given
    let cases = [
        ("postgresql", PG_WRAPPER),
        ("postgresql", TSQL_BATCH),
        ("mysql", PG_WRAPPER),
        ("mysql", TSQL_BATCH),
        ("postgresql-plus", TSQL_BATCH),
        ("sqlserver-plus", PG_WRAPPER),
    ];

    // when
    let actual = rejected(&cases).await;

    // then
    assert_eq!(actual, [(ErrorCode::WriteIntentRequired, 0); 6]);
}

#[test]
fn as_바로_뒤의_별칭은_문장_키워드여도_읽기로_증명된다() {
    // given
    let queries = [
        "SELECT COUNT(*) AS CNT, STRING_AGG(CAST(t.name AS varchar(max)), ';') AS BACKUP FROM dbo.some_table t",
        "SELECT 1 AS LOAD, 2 AS lock, 3 AS Save, 4 AS copy FROM t",
        "SELECT 1 AS\n  IMPORT FROM t",
    ];

    // when
    let actual: Vec<_> = queries
        .iter()
        .flat_map(|sql| {
            [None, Some(ReadDialect::SqlServer)]
                .map(|dialect| (sql, dialect, prove_read(sql, dialect)))
        })
        .collect();

    // then
    for (sql, dialect, result) in actual {
        assert_eq!(result, Some(proven(sql, &[])), "{dialect:?}: {sql}");
    }
}

#[test]
fn 따옴표로_감싼_as나_별칭_다음의_문장_키워드는_읽기로_증명하지_않는다() {
    // given
    let queries = [
        "SELECT 1 [AS] DELETE FROM t",
        "SELECT 1 \"AS\" DELETE FROM t",
        "SELECT 1 `AS` DELETE FROM t",
        "SELECT 1 AS x DELETE FROM t",
        "SELECT 1 AS BACKUP DELETE FROM t",
        "SELECT 1 AS INSERT INTO t2",
    ];

    // when
    let actual: Vec<_> = queries
        .iter()
        .flat_map(|sql| {
            [
                None,
                Some(ReadDialect::Postgres),
                Some(ReadDialect::SqlServer),
            ]
            .map(|dialect| (sql, dialect, prove_read(sql, dialect)))
        })
        .collect();

    // then
    for (sql, dialect, result) in actual {
        assert_eq!(result, None, "{dialect:?}: {sql}");
    }
}
