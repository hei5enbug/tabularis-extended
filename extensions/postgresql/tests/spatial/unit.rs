use crate::handlers::spatial::{
    ewkb::{inspect, SpatialValue},
    exact::validate_text,
    types::Viewport,
};

#[path = "export_unit.rs"]
mod export;

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|part| u8::from_str_radix(std::str::from_utf8(part).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn 원본_ewkb의_바이트와_z차원을_그대로_보존한다() {
    // given
    let raw = bytes("01010000a0e6100000000000000000f03f0000000000000040000000000000f87f");
    // when
    let wrapper = SpatialValue::from_bytes("geometry", &raw).unwrap();
    // then
    assert_eq!(wrapper.bytes().unwrap().0, raw);
    assert_eq!(wrapper.srid, Some(4326));
    assert_eq!(wrapper.dimensions, "XYZ");
    assert!(!wrapper.is_empty);
    assert!(!wrapper.bytes().unwrap().1.invalid_xy);
}

#[test]
fn 빈_점은_sql_null과_다른_공간값으로_보존한다() {
    // given
    let raw = bytes("01010000a0e6100000000000000000f87f000000000000f87f000000000000f87f");
    // when
    let wrapper = SpatialValue::from_bytes("geometry", &raw).unwrap();
    // then
    assert!(wrapper.is_empty);
    assert_eq!(wrapper.bytes().unwrap().0, raw);
    assert_eq!(wrapper.bytes().unwrap().1.positions, 0);
}

#[test]
fn 무한대_xy도_원본은_보존하고_표시_검증에_표시한다() {
    // given
    let raw = bytes("0101000020e6100000000000000000f07f0000000000000040");
    // when
    let wrapper = SpatialValue::from_bytes("geometry", &raw).unwrap();
    // then
    assert_eq!(wrapper.bytes().unwrap().0, raw);
    assert!(wrapper.bytes().unwrap().1.invalid_xy);
}

#[test]
fn 메타데이터가_원본_바이트와_다른_wrapper는_거부한다() {
    // given
    let mut wrapper = SpatialValue::from_bytes(
        "geometry",
        &bytes("0101000020e6100000000000000000f03f0000000000000040"),
    )
    .unwrap();
    wrapper.srid = Some(3857);
    // when
    let result = wrapper.bytes();
    // then
    assert_eq!(result.unwrap_err().code, "INVALID_GEOMETRY");
}

#[test]
fn 잘린_ewkb는_명시적인_공간_오류를_반환한다() {
    // given
    let raw = bytes("0101000020e6100000");
    // when
    let result = inspect(&raw);
    // then
    assert_eq!(result.unwrap_err().code, "INVALID_GEOMETRY");
}

#[test]
fn 영_너비_viewport는_세계를_뜻하지_않는다() {
    // given
    let viewport = Viewport {
        west: 0.0,
        south: -10.0,
        east: 0.0,
        north: 10.0,
        world: false,
    };
    // when
    let pieces = viewport.pieces(false).unwrap();
    // then
    assert_eq!(pieces, vec![[0.0, -10.0, 0.0, 10.0]]);
}

#[test]
fn 날짜변경선_viewport는_두_조각으로_분리한다() {
    // given
    let viewport = Viewport {
        west: 170.0,
        south: -10.0,
        east: -170.0,
        north: 10.0,
        world: false,
    };
    // when
    let pieces = viewport.pieces(false).unwrap();
    // then
    assert_eq!(
        pieces,
        vec![[170.0, -10.0, 180.0, 10.0], [-180.0, -10.0, -170.0, 10.0]]
    );
}

#[test]
fn geography의_세계_viewport는_반구보다_작은_경도_조각을_사용한다() {
    // given
    let viewport = Viewport {
        west: 0.0,
        south: -90.0,
        east: 0.0,
        north: 90.0,
        world: true,
    };
    // when
    let pieces = viewport.pieces(true).unwrap();
    // then
    assert_eq!(pieces.len(), 4);
    assert!(pieces.iter().all(|p| p[2] - p[0] < 180.0));
}

#[test]
fn 중첩_주석_뒤의_트랜잭션_제어도_거부한다() {
    // given
    let text = " -- 이전 주석\n /* 바깥 /* 안 */ 끝 */ COMMIT";
    // when
    let result = validate_text(text);
    // then
    assert_eq!(result.unwrap_err().code, "UNSUPPORTED_OPERATION");
}

#[test]
fn 문자열과_dollar_quote의_commit은_제어_명령으로_오인하지_않는다() {
    // given
    let text = "SELECT 'COMMIT', $$ROLLBACK$$, 1 /* SET ROLE */";
    // when
    let result = validate_text(text);
    // then
    assert!(result.is_ok());
}

#[test]
fn prepare와_transaction_사이의_주석을_정확히_읽는다() {
    // given
    let text = "PREPARE /* /* 중첩 */ 주석 */ TRANSACTION 'x'";
    // when
    let result = validate_text(text);
    // then
    assert_eq!(result.unwrap_err().code, "UNSUPPORTED_OPERATION");
}
