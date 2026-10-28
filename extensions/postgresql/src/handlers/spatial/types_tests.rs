use super::{DriverContext, MAX_DEADLINE_MS};
use serde_json::json;

fn parse_with_deadline(deadline_ms: u64) -> Result<(), String> {
    let params = json!({
        "params": {"database": "fixture", "username": "synthetic"},
        "driver_context": {
            "protocol_version": 1,
            "connection_id": "synthetic-connection",
            "request_id": "synthetic-request",
            "deadline_ms": deadline_ms,
            "read_only": true
        },
        "input": {}
    });
    DriverContext::parse(&params, false)
        .map(|_| ())
        .map_err(|error| error.code)
}

#[test]
fn 컨텍스트는_한_시간까지의_기한을_받고_그보다_길면_거부한다() {
    // given
    let deadlines = [1, 120_000, 120_001, MAX_DEADLINE_MS, MAX_DEADLINE_MS + 1, 0];

    // when
    let actual: Vec<Result<(), String>> = deadlines.into_iter().map(parse_with_deadline).collect();

    // then
    assert_eq!(
        actual,
        vec![
            Ok(()),
            Ok(()),
            Ok(()),
            Ok(()),
            Err("INVALID_ARGUMENT".to_string()),
            Err("INVALID_ARGUMENT".to_string()),
        ]
    );
}
