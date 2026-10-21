use serde_json::json;
use tabularis_spatial_core::*;

fn query_layer() -> LayerState {
    LayerState::new("query", "connection", LayerSource::QueryResult { result_id: "cached".into(), result_set_index: 1, column_index: 2, source_srid: None, longitude_mode: LongitudeMode::Preserve, skip_invalid: false }, Style::default()).unwrap()
}

fn table_layer() -> LayerState {
    LayerState::new("table", "connection", LayerSource::Table { table: TableRef { database: None, schema: Some("public".into()), table: "roads".into() }, column: "g".into(), viewport: Viewport::default(), source_srid: None, srid_filter: Some(4326), longitude_mode: LongitudeMode::Preserve, skip_invalid: false }, Style::default()).unwrap()
}

fn map() -> MapState {
    let mut map = MapState::new("map", "빈 지도").unwrap();
    map.layers = vec![query_layer(), table_layer()];
    map
}

fn race(mut map: MapState) -> (Result<u64, CoreError>, Result<u64, CoreError>, MapState) {
    let expected = map.version;
    let first = map.update(expected, Some("첫 요청".into()), BasemapChange::Keep);
    let second = map.update(expected, Some("늦은 요청".into()), BasemapChange::Keep);
    (first, second, map)
}

fn validate_basemaps(urls: &[&str]) -> Vec<Result<(), CoreError>> {
    urls.iter().map(|url| Basemap { style_url: (*url).into(), attribution: "제공자 이름".into() }.validate()).collect()
}

fn validate_styles(styles: &[Style]) -> Vec<Result<(), CoreError>> { styles.iter().map(Style::validate).collect() }

#[test]
fn 새_지도는_세계_viewport와_빈_basemap으로_시작한다() {
    // given
    let name = "새 지도";
    // when
    let actual = MapState::new("map-id", name);
    // then
    assert_eq!(actual.as_ref().unwrap().version, 0);
    assert_eq!(actual.as_ref().unwrap().basemap, None);
    assert!(actual.as_ref().unwrap().viewport.is_full_globe());
    assert!(actual.as_ref().unwrap().layers.is_empty());
}

#[test]
fn 같은_expected_version의_경합은_한_번만_적용한다() {
    // given
    let map = map();
    // when
    let actual = race(map);
    // then
    assert_eq!(actual.0, Ok(1));
    assert_eq!(actual.1.as_ref().unwrap_err().code, ErrorCode::VersionConflict);
    assert_eq!(actual.1.as_ref().unwrap_err().current_version, Some(1));
    assert_eq!(actual.2.name, "첫 요청");
    assert_eq!(actual.2.version, 1);
}

#[test]
fn 버전_불일치는_다른_입력_오류보다_먼저_확인한다() {
    // given
    let mut map = map();
    let before = map.clone();
    // when
    let actual = map.update(7, Some(String::new()), BasemapChange::Keep);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::VersionConflict);
    assert_eq!(map, before);
}

#[test]
fn 유효하지_않은_변경은_상태와_버전을_바꾸지_않는다() {
    // given
    let mut map = map();
    let before = map.clone();
    let style = Style { point: Some(PointStyle { radius: Some(41.0), ..PointStyle::default() }), ..Style::default() };
    // when
    let actual = map.update_layer(0, "query", Some(false), Some(style));
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(map, before);
}

#[test]
fn viewport_변경은_table의_조회범위만_갱신한다() {
    // given
    let mut map = map();
    let query_before = map.layers[0].clone();
    let viewport = Viewport { west: 179.0, east: -179.0, south: -20.0, north: 20.0, world: false };
    // when
    let actual = map.set_viewport(0, viewport);
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.viewport, viewport);
    assert_eq!(map.layers[0], query_before);
    assert_eq!(map.layers[1].generation, 1);
    assert!(matches!(map.layers[1].source, LayerSource::Table { viewport: stored, .. } if stored == viewport));
}

#[test]
fn visibility와_style_변경은_generation을_증가시킨다() {
    // given
    let mut map = map();
    let style = Style { line: Some(LineStyle { color: Some("#12345678".into()), opacity: Some(0.5), width: Some(20.0) }), ..Style::default() };
    // when
    let actual = map.update_layer(0, "query", Some(false), Some(style.clone()));
    // then
    assert_eq!(actual, Ok(1));
    assert!(!map.layers[0].visible);
    assert_eq!(map.layers[0].style, style);
    assert_eq!(map.layers[0].generation, 1);
}

#[test]
fn 오래된_generation의_표시_결과는_거부한다() {
    // given
    let mut layer = query_layer();
    layer.generation = 2;
    // when
    let actual = layer.ensure_generation(1);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::StaleGeneration);
    assert_eq!(layer.generation, 2);
}

#[test]
fn source_변경은_새_result_reference와_generation을_남긴다() {
    // given
    let mut map = map();
    let source = LayerSource::QueryResult { result_id: "new-result".into(), result_set_index: 3, column_index: 0, source_srid: None, longitude_mode: LongitudeMode::Shortest, skip_invalid: true };
    // when
    let actual = map.replace_layer_source(0, "query", source.clone());
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.layers[0].source, source);
    assert_eq!(map.layers[0].generation, 1);
    assert_eq!(map.layers[0].result_references, vec![ResultReference { result_id: "new-result".into(), result_set_index: 3 }]);
}

#[test]
fn layer_삭제는_해당_selection도_제거한다() {
    // given
    let mut map = map();
    map.selected_feature_refs = vec![FeatureRef { layer_id: "query".into(), feature_id: "cached:1:9".into() }, FeatureRef { layer_id: "table".into(), feature_id: "snapshot:4".into() }];
    // when
    let actual = map.remove_layer(0, "query");
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.layers.len(), 1);
    assert_eq!(map.selected_feature_refs, vec![FeatureRef { layer_id: "table".into(), feature_id: "snapshot:4".into() }]);
}

#[test]
fn selection은_중복이나_없는_layer를_허용하지_않는다() {
    // given
    let mut map = map();
    let before = map.clone();
    let reference = FeatureRef { layer_id: "query".into(), feature_id: "cached:1:0".into() };
    // when
    let actual = map.set_selection(0, vec![reference.clone(), reference]);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(map, before);
}

#[test]
fn safe_integer_최대_버전은_증가하지_않는다() {
    // given
    let mut map = map();
    map.version = MAX_SAFE_INTEGER;
    let before = map.clone();
    // when
    let actual = map.update(MAX_SAFE_INTEGER, Some("변경".into()), BasemapChange::Keep);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
    assert_eq!(map, before);
}

#[test]
fn table_generation_overflow도_전체_변경을_취소한다() {
    // given
    let mut map = map();
    map.layers[1].generation = MAX_SAFE_INTEGER;
    let before = map.clone();
    // when
    let actual = map.set_viewport(0, Viewport { north: 20.0, ..Viewport::default() });
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::ResourceLimit);
    assert_eq!(map, before);
}

#[test]
fn basemap은_https와_no_userinfo를_검증한다() {
    // given
    let urls = ["HTTPS://tiles.example/style.json?email=a@b", "https://[::1]:443/style.json", "http://tiles.example/style.json", "https://user:password@tiles.example/style.json", "https://tiles.example/a\\b", "https:///style.json", "https://tiles.example/a b"];
    // when
    let actual = validate_basemaps(&urls);
    // then
    assert!(actual[0].is_ok());
    assert!(actual[1].is_ok());
    assert!(actual[2..].iter().all(Result::is_err));
}

#[test]
fn basemap_clear는_누락된_변경과_구분한다() {
    // given
    let mut map = map();
    map.basemap = Some(Basemap { style_url: "https://tiles.example/style.json".into(), attribution: "제공자".into() });
    // when
    let actual = map.update(0, None, BasemapChange::Clear);
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.basemap, None);
}

#[test]
fn style은_hex와_opacity와_크기_상한을_검증한다() {
    // given
    let styles = [
        Style { point: Some(PointStyle { color: Some("#aB12ef".into()), opacity: Some(0.0), radius: Some(1.0) }), line: Some(LineStyle { color: Some("#12345678".into()), opacity: Some(1.0), width: Some(20.0) }), polygon: Some(PolygonStyle { color: Some("#000000".into()), opacity: Some(0.5) }) },
        Style { point: Some(PointStyle { color: Some("red".into()), ..PointStyle::default() }), ..Style::default() },
        Style { line: Some(LineStyle { width: Some(0.0), ..LineStyle::default() }), ..Style::default() },
        Style { polygon: Some(PolygonStyle { opacity: Some(f64::NAN), ..PolygonStyle::default() }), ..Style::default() },
    ];
    // when
    let actual = validate_styles(&styles);
    // then
    assert!(actual[0].is_ok());
    assert!(actual[1..].iter().all(Result::is_err));
}

#[test]
fn 지도_직렬화에는_좌표나_sql이나_자격증명_필드가_없다() {
    // given
    let map = map();
    let expected_keys = ["basemap", "layers", "map_id", "name", "selected_feature_refs", "version", "viewport"];
    // when
    let actual = serde_json::to_value(map).unwrap();
    // then
    assert_eq!(actual.as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>(), expected_keys);
    assert!(actual["layers"][0]["source"].get("sql").is_none());
    assert!(actual["layers"][0].get("credentials").is_none());
    assert!(actual["layers"][0].get("coordinates").is_none());
    assert_eq!(actual["layers"][0]["result_references"], json!([{"result_id":"cached","result_set_index":1}]));
}

#[test]
fn 없는_layer의_selection은_지도에_남기지_않는다() {
    // given
    let mut map = map();
    let before = map.clone();
    // when
    let actual = map.set_selection(0, vec![FeatureRef { layer_id: "missing".into(), feature_id: "feature".into() }]);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(map, before);
}

#[test]
fn 중복_layer_추가는_기존_layer를_덮어쓰지_않는다() {
    // given
    let mut map = map();
    let before = map.clone();
    let layer = query_layer();
    // when
    let actual = map.add_layer(0, layer);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(map, before);
}

#[test]
fn 이름_수정은_기존_basemap과_layer_generation을_유지한다() {
    // given
    let mut map = map();
    let basemap = Basemap { style_url: "https://tiles.example/style.json".into(), attribution: "제공자".into() };
    map.basemap = Some(basemap.clone());
    let layers = map.layers.clone();
    // when
    let actual = map.update(0, Some("다른 이름".into()), BasemapChange::Keep);
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.name, "다른 이름");
    assert_eq!(map.basemap, Some(basemap));
    assert_eq!(map.layers, layers);
}

#[test]
fn 정상_layer_추가는_한_버전을_적용한다() {
    // given
    let mut map = MapState::new("map", "지도").unwrap();
    let layer = query_layer();
    // when
    let actual = map.add_layer(0, layer.clone());
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.layers, vec![layer]);
    assert_eq!(map.layers[0].generation, 0);
}

#[test]
fn 정상_selection은_feature_id를_그대로_저장한다() {
    // given
    let mut map = map();
    let references = vec![FeatureRef { layer_id: "query".into(), feature_id: "cached:1:19".into() }];
    let generations = map.layers.iter().map(|layer| layer.generation).collect::<Vec<_>>();
    // when
    let actual = map.set_selection(0, references.clone());
    // then
    assert_eq!(actual, Ok(1));
    assert_eq!(map.selected_feature_refs, references);
    assert_eq!(map.layers.iter().map(|layer| layer.generation).collect::<Vec<_>>(), generations);
}

#[test]
fn version_conflict의_wire_code와_current_version을_노출한다() {
    // given
    let error = CoreError::version_conflict(7);
    // when
    let actual = serde_json::to_value(&error).unwrap();
    // then
    assert_eq!(actual["code"], "VERSION_CONFLICT");
    assert_eq!(actual["current_version"], 7);
    assert_eq!(error.code.wire_code(), "VERSION_CONFLICT");
}
