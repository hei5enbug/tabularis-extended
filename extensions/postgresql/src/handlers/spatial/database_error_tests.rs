use super::*;

#[test]
fn 구문_오류는_문구와_sqlstate와_선택_필드를_담는다() {
    // given
    let message = "ERROR: division by zero\n\nDbError { code: 22012 }";

    // when
    let actual = value(
        message,
        "22012",
        Some("row 1"),
        Some("check divisor"),
        Some(8),
    )
    .unwrap();

    // then
    assert_eq!(
        actual,
        json!({
            "engine": "postgresql",
            "message": message,
            "sqlstate": "22012",
            "detail": "row 1",
            "hint": "check divisor",
            "position": 8,
        })
    );
}

#[test]
fn 선택_필드가_없으면_키를_싣지_않는다() {
    // given
    let sqlstate = "42P01";

    // when
    let actual = value("ERROR: relation does not exist", sqlstate, None, None, None).unwrap();

    // then
    assert_eq!(
        actual.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["engine", "message", "sqlstate"]
    );
}

#[test]
fn 연결과_인증과_취소_상태는_구문_오류로_싣지_않는다() {
    for sqlstate in [
        "08006", "08001", "28000", "28P01", "3D000", "53300", "57014", "57P01", "57P02", "57P03",
    ] {
        // given
        let message = "ERROR: refused";

        // when
        let actual = value(message, sqlstate, None, None, None);

        // then
        assert!(actual.is_none(), "sqlstate {sqlstate}");
    }
}

#[test]
fn 문구는_사천_바이트로_글자_경계에서_자른다() {
    // given
    let message = "가".repeat(2000);

    // when
    let actual = value(&message, "XX000", None, None, None).unwrap();

    // then
    let bounded = actual["message"].as_str().unwrap();
    assert_eq!(bounded.len(), 3999);
    assert!(message.starts_with(bounded));
}

#[test]
fn 서버가_만들지_않은_오류에는_database_error가_없다() {
    // given
    let error = tokio_postgres::Error::__private_api_timeout();

    // when
    let actual = super::super::SpatialError::database(error);

    // then
    assert_eq!(actual.code, "DRIVER_EXITED");
    assert!(actual.details.is_null());
}
