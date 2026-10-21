use serde_json::{json, Value};
use tabularis_spatial_core::{ErrorCode, Geometry};

fn parse_cases(cases: &[Value]) -> Vec<Result<Option<Geometry>, tabularis_spatial_core::CoreError>> {
    cases.iter().map(Geometry::from_json).collect()
}

fn nested_collection(depth: usize) -> Value {
    let mut value = json!({"type":"Point","coordinates":[1,2]});
    for _ in 1..depth { value = json!({"type":"GeometryCollection","geometries":[value]}); }
    value
}

#[test]
fn 모든_기본_공간형의_xy_좌표를_표현한다() {
    // given
    let cases = vec![
        json!({"type":"Point","coordinates":[1.0,2.0]}),
        json!({"type":"LineString","coordinates":[[1.0,2.0],[3.0,4.0]]}),
        json!({"type":"Polygon","coordinates":[[[0.0,0.0],[2.0,0.0],[0.0,2.0],[0.0,0.0]]]}),
        json!({"type":"MultiPoint","coordinates":[[1.0,2.0],[3.0,4.0]]}),
        json!({"type":"MultiLineString","coordinates":[[[1.0,2.0],[3.0,4.0]],[[5.0,6.0],[7.0,8.0]]]}),
        json!({"type":"MultiPolygon","coordinates":[[[[0.0,0.0],[2.0,0.0],[0.0,2.0],[0.0,0.0]]],[[[3.0,3.0],[4.0,3.0],[3.0,4.0],[3.0,3.0]]]]}),
    ];
    // when
    let actual = parse_cases(&cases);
    // then
    assert!(actual.iter().all(Result::is_ok));
    assert_eq!(actual.iter().map(|value| serde_json::to_value(value.as_ref().unwrap().as_ref().unwrap()).unwrap()).collect::<Vec<_>>(), cases);
}

#[test]
fn 중첩_collection과_구멍과_multipart의_모든_좌표를_센다() {
    // given
    let geometry = Geometry::from_json(&json!({"type":"GeometryCollection","geometries":[
        {"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,4],[0,0]],[[1,1],[1,2],[2,2],[2,1],[1,1]]]},
        {"type":"GeometryCollection","geometries":[
            {"type":"MultiLineString","coordinates":[[[179,0],[-179,0]],[[3,4],[5,6],[7,8]]]},
            {"type":"MultiPoint","coordinates":[[8,9],[10,11]]}
        ]}
    ]})).unwrap().unwrap();
    // when
    let actual = geometry.coordinate_count();
    // then
    assert_eq!(actual.unwrap(), 17);
}

#[test]
fn 날짜변경선을_넘는_line의_좌표_순서를_보존한다() {
    // given
    let input = json!({"type":"LineString","coordinates":[[179.0,20.0],[-179.0,10.0],[175.0,5.0]]});
    // when
    let actual = Geometry::from_json(&input);
    // then
    assert_eq!(serde_json::to_value(actual.unwrap().unwrap()).unwrap(), input);
}

#[test]
fn null과_empty_표시는_null로_정규화한다() {
    // given
    let inputs = vec![Value::Null, json!({"type":"Point","coordinates":[]}), json!({"type":"LineString","coordinates":[]}), json!({"type":"Polygon","coordinates":[]})];
    // when
    let actual = parse_cases(&inputs);
    // then
    assert!(actual.iter().all(|result| matches!(result, Ok(None))));
}

#[test]
fn 비어있는_multi와_collection은_오류가_아니다() {
    // given
    let inputs = vec![json!({"type":"MultiPoint","coordinates":[]}), json!({"type":"MultiLineString","coordinates":[]}), json!({"type":"MultiPolygon","coordinates":[]}), json!({"type":"GeometryCollection","geometries":[]})];
    // when
    let actual = parse_cases(&inputs);
    // then
    assert!(actual.iter().all(Result::is_ok));
}

#[test]
fn 알려지지_않은_형식은_지원불가_오류다() {
    // given
    let input = json!({"type":"GeometryCollection","geometries":[{"type":"CircularString","coordinates":[[1,2],[3,4],[5,6]]}]});
    // when
    let actual = Geometry::from_json(&input);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::UnsupportedType);
}

#[test]
fn 잘못된_xy와_닫히지_않은_ring은_오류다() {
    // given
    let cases = vec![json!({"type":"Point","coordinates":[1,2,3]}), json!({"type":"Point","coordinates":[181,0]}), json!({"type":"Point","coordinates":[0,-91]}), json!({"type":"LineString","coordinates":[[1,2]]}), json!({"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}), json!({"type":"Point","coordinates":["1",2]})];
    // when
    let actual = parse_cases(&cases);
    // then
    assert!(actual.iter().all(|result| result.as_ref().unwrap_err().code == ErrorCode::InvalidGeometry));
}

#[test]
fn 비유한_xy는_지도_표시에서_거부한다() {
    // given
    let geometry = Geometry::Point { coordinates: vec![f64::INFINITY, f64::NAN] };
    // when
    let actual = geometry.coordinate_count();
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidGeometry);
}

#[test]
fn 중첩_128단계는_허용하고_129단계는_거부한다() {
    // given
    let inputs = vec![nested_collection(128), nested_collection(129)];
    // when
    let actual = parse_cases(&inputs);
    // then
    assert!(actual[0].is_ok());
    assert_eq!(actual[1].as_ref().unwrap_err().code, ErrorCode::InvalidGeometry);
}
