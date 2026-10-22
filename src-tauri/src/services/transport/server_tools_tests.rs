use super::super::super::sql_classifier::classify_query_kind;
use super::*;

const FLAVORS: [SqlFlavor; 2] = [SqlFlavor::Postgres, SqlFlavor::Mssql];
const COMMON_COLUMNS: [&str; 11] = [
    "process_id",
    "user_name",
    "database_name",
    "application_name",
    "client",
    "state",
    "wait",
    "blocking_process_id",
    "query_start",
    "running_ms",
    "query",
];

#[test]
fn 활동_조회문은_두_방언_모두_읽기로_분류된다() {
    // given
    let cases = [
        (SqlFlavor::Postgres, false),
        (SqlFlavor::Postgres, true),
        (SqlFlavor::Mssql, false),
        (SqlFlavor::Mssql, true),
    ];

    // when
    let actual: Vec<&str> = cases
        .iter()
        .map(|(flavor, idle)| classify_query_kind(&activity_sql(*flavor, *idle, 50).unwrap()))
        .collect();

    // then
    assert!(actual.iter().all(|kind| *kind == "select"), "{actual:?}");
}

#[test]
fn 활동_조회문은_두_방언이_같은_열_이름을_쓰고_질의_본문을_이천자로_자른다() {
    // given
    let statements: Vec<String> = FLAVORS
        .iter()
        .map(|flavor| activity_sql(*flavor, true, 50).unwrap())
        .collect();

    // when
    let missing: Vec<(&str, &str)> = statements
        .iter()
        .flat_map(|sql| {
            COMMON_COLUMNS
                .iter()
                .filter(move |column| !sql.contains(&format!(" AS {column}")))
                .map(|column| (sql.as_str(), *column))
        })
        .collect();

    // then
    assert!(missing.is_empty(), "{missing:?}");
    assert!(statements[0].contains("left(a.query, 2000)"));
    assert!(statements[1].contains("LEFT(t.text, 2000)"));
    for column in ["command", "cpu_ms", "logical_reads"] {
        assert!(statements[1].contains(&format!(" AS {column}")), "{column}");
        assert!(
            !statements[0].contains(&format!(" AS {column}")),
            "{column}"
        );
    }
}

#[test]
fn postgresql_활동_조회문은_클라이언트_백엔드만_보고_자기_세션과_유휴를_뺀다() {
    // given
    let busy_only = activity_sql(SqlFlavor::Postgres, false, 12).unwrap();
    let with_idle = activity_sql(SqlFlavor::Postgres, true, 12).unwrap();

    // when
    let actual = [&busy_only, &with_idle].map(|sql| {
        (
            sql.contains("FROM pg_stat_activity"),
            sql.contains("a.backend_type = 'client backend'"),
            sql.contains("a.pid <> pg_backend_pid()"),
            sql.contains("ORDER BY a.query_start"),
            sql.contains("clock_timestamp() - a.query_start"),
            sql.ends_with("LIMIT 12"),
            sql.contains("IS DISTINCT FROM 'idle'"),
        )
    });

    // then
    assert_eq!(actual[0], (true, true, true, true, true, true, true));
    assert_eq!(actual[1], (true, true, true, true, true, true, false));
}

#[test]
fn sqlserver_활동_조회문은_사용자_세션만_보고_자기_세션을_빼고_요청_없는_세션은_유휴일_때만_넣는다()
{
    // given
    let busy_only = activity_sql(SqlFlavor::Mssql, false, 9).unwrap();
    let with_idle = activity_sql(SqlFlavor::Mssql, true, 9).unwrap();

    // when
    let actual = [&busy_only, &with_idle].map(|sql| {
        (
            sql.starts_with("SELECT TOP (9) "),
            sql.contains("FROM sys.dm_exec_sessions s LEFT JOIN sys.dm_exec_requests r"),
            sql.contains("OUTER APPLY sys.dm_exec_sql_text(r.sql_handle)"),
            sql.contains("s.is_user_process = 1"),
            sql.contains("s.session_id <> @@SPID"),
            sql.contains("AND r.session_id IS NOT NULL"),
        )
    });

    // then
    assert_eq!(actual[0], (true, true, true, true, true, true));
    assert_eq!(actual[1], (true, true, true, true, true, false));
}

#[test]
fn 종료문은_정수에서만_만들어지고_쓰기로_분류된다() {
    // given
    let cases = [
        (
            SqlFlavor::Postgres,
            4242,
            "SELECT pg_cancel_backend(4242) AS cancelled",
        ),
        (SqlFlavor::Mssql, 77, "KILL 77"),
        (SqlFlavor::Mssql, 2_147_483_647, "KILL 2147483647"),
    ];

    // when
    let actual: Vec<(String, &str)> = cases
        .iter()
        .map(|(flavor, id, _)| {
            let sql = cancel_sql(*flavor, *id).unwrap();
            let kind = classify_query_kind(&sql);
            (sql, kind)
        })
        .collect();

    // then
    for ((_, _, expected), (sql, kind)) in cases.iter().zip(&actual) {
        assert_eq!(sql, expected);
        assert_eq!(*kind, "write", "{sql}");
    }
}

#[test]
fn 두_방언이_아니면_조회문도_종료문도_만들지_않는다() {
    // given
    let flavors = [SqlFlavor::Mysql, SqlFlavor::Sqlite, SqlFlavor::Other];

    // when
    let actual: Vec<(Option<ErrorCode>, Option<ErrorCode>)> = flavors
        .iter()
        .map(|flavor| {
            (
                activity_sql(*flavor, false, 50)
                    .err()
                    .map(|error| error.code),
                cancel_sql(*flavor, 1).err().map(|error| error.code),
            )
        })
        .collect();

    // then
    assert!(actual.iter().all(|codes| {
        *codes
            == (
                Some(ErrorCode::CapabilityUnavailable),
                Some(ErrorCode::CapabilityUnavailable),
            )
    }));
}

#[test]
fn 종료_도구_설명은_엔진_차이와_쓰기_조건과_서버_권한을_밝힌다() {
    // given
    let text = description("server_cancel");

    // when
    let missing: Vec<&str> = [
        "pg_cancel_backend",
        "keeps the session",
        "KILL",
        "rolls back its open transaction",
        "write must be true",
        "read-only mode refuses it",
        "approval mode",
        "pg_signal_backend",
        "ALTER ANY CONNECTION",
    ]
    .into_iter()
    .filter(|part| !text.contains(part))
    .collect();

    // then
    assert!(missing.is_empty(), "{missing:?}");
}

#[test]
fn 도구_스키마는_한도와_필수_인자를_계약으로_고정한다() {
    // given
    let activity = activity_schema();
    let cancel = cancel_schema();

    // when
    let actual = (
        activity["properties"]["limit"].clone(),
        activity["required"].clone(),
        cancel["properties"]["process_id"].clone(),
        cancel["required"].clone(),
        cancel["properties"].get("write").is_some(),
        activity["properties"].get("write").is_some(),
    );

    // then
    assert_eq!(actual.0["minimum"], 1);
    assert_eq!(actual.0["maximum"], 200);
    assert_eq!(actual.0["default"], 50);
    assert_eq!(actual.1, json!(["connection_id"]));
    assert_eq!(actual.2["minimum"], 1);
    assert_eq!(actual.2["maximum"], 2_147_483_647_u64);
    assert_eq!(actual.3, json!(["connection_id", "process_id"]));
    assert!(actual.4 && !actual.5);
}
