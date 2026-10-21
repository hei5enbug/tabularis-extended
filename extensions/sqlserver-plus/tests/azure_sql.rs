#[path = "support/azure_sql_harness/mod.rs"]
mod harness;

#[test]
#[allow(non_snake_case)]
fn 실제Azure_SQL_전용_조회는_인증_TLS_세션과_취소를_SELECT만으로_검증한다() {
    // given
    let expected_writes = 0;
    // when
    let actual = harness::run::run_live_from_stdin();
    // then
    assert!(
        actual.success,
        "Azure manual harness failed with sanitized code: {:?}",
        actual.error
    );
    assert_eq!(actual.writes_dispatched, expected_writes);
    assert_eq!(actual.evidence_kind, "actual_azure");
    assert!(!actual.integration_complete);
}
