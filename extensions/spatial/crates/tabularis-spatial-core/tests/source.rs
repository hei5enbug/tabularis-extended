use serde_json::json;
use tabularis_spatial_core::*;

fn wrapper(srid: Option<i32>, empty: bool) -> SpatialValue {
    SpatialValue { kind: SpatialKind::Spatial, native_type: NativeType::Geometry, srid, dimensions: Dimensions::XYZM, encoding: SpatialEncoding::EwkbBase64, value: "  AQEAAKARDwAA\nexact+bytes/==  ".into(), is_empty: empty }
}

fn source(srid: Option<u64>) -> LayerSource {
    LayerSource::QueryResult { result_id: "cached-result".into(), result_set_index: 2, column_index: 3, source_srid: srid, longitude_mode: LongitudeMode::Preserve, skip_invalid: false }
}

#[test]
fn 공간_wrapper의_원문과_zm과_empty를_그대로_왕복한다() {
    // given
    let input = wrapper(Some(3857), true);
    let wire = serde_json::to_value(&input).unwrap();
    // when
    let actual: SpatialValue = serde_json::from_value(wire.clone()).unwrap();
    // then
    assert_eq!(actual, input);
    assert_eq!(serde_json::to_value(actual).unwrap(), wire);
    assert_eq!(wire["value"], "  AQEAAKARDwAA\nexact+bytes/==  ");
    assert_eq!(wire["dimensions"], "XYZM");
    assert_eq!(wire["encoding"], "ewkb-base64");
}

#[test]
fn unknown_srid는_명시적인_source_srid를_요구한다() {
    // given
    let source = source(None);
    let values = vec![None, Some(wrapper(None, false))];
    // when
    let actual = source.validate_values(&values);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::UnknownSrid);
}

#[test]
fn 명시적_srid는_원본_unknown_wrapper를_수정하지_않는다() {
    // given
    let source = source(Some(4326));
    let values = vec![Some(wrapper(None, false))];
    let before = values.clone();
    // when
    let actual = source.validate_values(&values);
    // then
    assert!(actual.is_ok());
    assert_eq!(values, before);
}

#[test]
fn 알려진_srid와_override가_충돌하면_거부한다() {
    // given
    let source = source(Some(4326));
    let values = vec![Some(wrapper(Some(3857), false))];
    // when
    let actual = source.validate_values(&values);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::SridConflict);
}

#[test]
fn 서로_다른_known_srid는_각_행의_원본을_유지한다() {
    // given
    let source = source(None);
    let values = vec![Some(wrapper(Some(4326), false)), Some(wrapper(Some(3857), false))];
    // when
    let actual = source.validate_values(&values);
    // then
    assert!(actual.is_ok());
    assert_eq!(values[0].as_ref().unwrap().srid, Some(4326));
    assert_eq!(values[1].as_ref().unwrap().srid, Some(3857));
}

#[test]
fn 복합_pk는_json_형식과_열_순서를_보존한다() {
    // given
    let input = json!({"columns":["tenant","id","flag"],"values":["001",7,true]});
    // when
    let actual: RowIdentity = serde_json::from_value(input.clone()).unwrap();
    // then
    assert!(actual.validate().is_ok());
    assert_eq!(serde_json::to_value(actual).unwrap(), input);
}

#[test]
fn 잘못된_pk_길이나_중복열은_거부한다() {
    // given
    let cases = [RowIdentity { columns: vec!["id".into()], values: vec![] }, RowIdentity { columns: vec!["id".into(), "id".into()], values: vec![json!(1), json!(2)] }];
    // when
    let actual = cases.iter().map(RowIdentity::validate).collect::<Vec<_>>();
    // then
    assert!(actual.iter().all(Result::is_err));
}

#[test]
fn query_feature_id는_set과_행_ordinal을_포함한다() {
    // given
    let result_id = "snapshot";
    // when
    let actual = query_feature_id(result_id, 2, 19);
    // then
    assert_eq!(actual.unwrap(), "snapshot:2:19");
}

#[test]
fn table만_srid_filter와_viewport를_갖는다() {
    // given
    let input = json!({"kind":"table","table":{"database":null,"schema":"공간 schema","table":"\"roads"},"column":"g","viewport":{"west":179,"east":-179,"south":-10,"north":10},"source_srid":4326,"srid_filter":0,"longitude_mode":"shortest","skip_invalid":true});
    // when
    let actual: LayerSource = serde_json::from_value(input).unwrap();
    // then
    assert!(actual.validate().is_ok());
    assert!(matches!(actual, LayerSource::Table { srid_filter: Some(0), longitude_mode: LongitudeMode::Shortest, skip_invalid: true, .. }));
}

#[test]
fn query_result에_table_filter를_섞으면_거부한다() {
    // given
    let input = json!({"kind":"query_result","result_id":"r","result_set_index":0,"column_index":0,"srid_filter":4326});
    // when
    let actual = serde_json::from_value::<LayerSource>(input);
    // then
    assert!(actual.is_err());
}

#[test]
fn 공개_source_srid는_driver_지원범위와_분리한다() {
    // given
    let source = source(Some(MAX_SAFE_INTEGER));
    // when
    let actual = source.validate();
    // then
    assert!(actual.is_ok());
}

#[test]
fn 날짜변경선은_두_조각이고_zero_width는_세계가_아니다() {
    // given
    let crossing = Viewport { west: 179.0, east: -179.0, south: -10.0, north: 10.0, world: false };
    let zero = Viewport { west: 10.0, east: 10.0, ..crossing };
    // when
    let actual = [crossing, zero].iter().map(Viewport::longitude_pieces).collect::<Vec<_>>();
    // then
    assert_eq!(actual[0].as_ref().unwrap(), &vec![LongitudePiece { west: 179.0, east: 180.0 }, LongitudePiece { west: -180.0, east: -179.0 }]);
    assert_eq!(actual[1].as_ref().unwrap(), &vec![LongitudePiece { west: 10.0, east: 10.0 }]);
    assert!(!zero.is_full_globe());
}

#[test]
fn world는_경도_전체만_뜻하고_위도_범위를_유지한다() {
    // given
    let viewport = Viewport { west: 0.0, east: 0.0, south: -10.0, north: 10.0, world: true };
    // when
    let actual = viewport.longitude_pieces();
    // then
    assert_eq!(actual.unwrap(), vec![LongitudePiece { west: -180.0, east: 180.0 }]);
    assert_eq!(viewport.south, -10.0);
    assert_eq!(viewport.north, 10.0);
    assert!(!viewport.is_full_globe());
    assert!(Viewport::default().is_full_globe());
}

#[test]
fn 카메라만_mercator_위도로_제한하고_원본은_유지한다() {
    // given
    let viewport = Viewport::default();
    let point = Geometry::Point { coordinates: vec![10.0, 89.0] };
    // when
    let actual = viewport.mercator_camera();
    // then
    assert_eq!(actual.as_ref().unwrap().north, MERCATOR_MAX_LATITUDE);
    assert_eq!(actual.as_ref().unwrap().south, -MERCATOR_MAX_LATITUDE);
    assert_eq!(viewport.north, 90.0);
    assert_eq!(point.coordinate_count().unwrap(), 1);
    assert_eq!(serde_json::to_value(point).unwrap()["coordinates"], json!([10.0,89.0]));
}

#[test]
fn 역전된_위도와_비유한_viewport는_거부한다() {
    // given
    let inputs = [Viewport { south: 10.0, north: -10.0, ..Viewport::default() }, Viewport { west: f64::NAN, ..Viewport::default() }, Viewport { east: 181.0, ..Viewport::default() }];
    // when
    let actual = inputs.iter().map(Viewport::validate).collect::<Vec<_>>();
    // then
    assert!(actual.iter().all(|error| error.as_ref().unwrap_err().code == ErrorCode::InvalidArgument));
}

#[test]
fn source_srid와_filter의_허용범위를_넘으면_거부한다() {
    // given
    let sources = [source(Some(0)), source(Some(MAX_SAFE_INTEGER + 1)), LayerSource::Table { table: TableRef { database: None, schema: None, table: "roads".into() }, column: "g".into(), viewport: Viewport::default(), source_srid: None, srid_filter: Some(1_000_000), longitude_mode: LongitudeMode::Preserve, skip_invalid: false }];
    // when
    let actual = sources.iter().map(LayerSource::validate).collect::<Vec<_>>();
    // then
    assert!(actual.iter().all(Result::is_err));
}

#[test]
fn null_공간값은_empty_wrapper와_구분한다() {
    // given
    let values = vec![None, Some(wrapper(Some(4326), true))];
    let wire = serde_json::to_value(&values).unwrap();
    // when
    let actual: Vec<Option<SpatialValue>> = serde_json::from_value(wire).unwrap();
    // then
    assert_eq!(actual, values);
    assert_eq!(actual[0], None);
    assert!(actual[1].as_ref().unwrap().is_empty);
    assert_eq!(actual[1].as_ref().unwrap().value, "  AQEAAKARDwAA\nexact+bytes/==  ");
}
