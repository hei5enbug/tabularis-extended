use serde_json::{json, Map};
use tabularis_spatial_core::*;

fn pair(index: u64, geometry: Option<Geometry>) -> (Feature, RowReference) {
    let id = format!("result:0:{index}");
    let feature = Feature::new(&id, geometry, Map::from_iter([("label".into(), json!("한글 😀")), ("raw".into(), json!({"kind":"spatial","value":"original=ewkb+/","dimensions":"XYZM"}))]));
    let reference = RowReference { feature_id: id, identity: None, snapshot_id: Some("snapshot".into()), row_ordinal: index };
    (feature, reference)
}

fn point() -> Option<Geometry> { Some(Geometry::Point { coordinates: vec![10.0, 20.0] }) }

fn scenario(page_limits: PageLimits, layer_limits: LayerLimits, prior: LayerBudget, geometries: Vec<Option<Geometry>>) -> (Vec<Result<PushResult, CoreError>>, FeaturePage) {
    let mut builder = PageBuilder::new(page_limits, layer_limits, prior).unwrap();
    let results = geometries.into_iter().enumerate().map(|(index, geometry)| {
        let (feature, reference) = pair(index as u64, geometry);
        builder.try_push(feature, reference)
    }).collect();
    (results, builder.page().clone())
}

fn byte_boundary_attempts(expected_bytes: u64) -> Vec<Result<PushResult, CoreError>> {
    [expected_bytes - 1, expected_bytes, expected_bytes + 1].into_iter().map(|max_bytes| {
        let mut builder = PageBuilder::new(PageLimits { max_bytes, ..PageLimits::default() }, LayerLimits::default(), LayerBudget::default()).unwrap();
        let (feature, reference) = pair(0, point());
        builder.try_push(feature, reference)
    }).collect()
}

fn capped_page() -> (FeaturePage, LayerBudget) {
    let mut builder = PageBuilder::new(PageLimits { max_features: 1, ..PageLimits::default() }, LayerLimits::default(), LayerBudget::default()).unwrap();
    let (feature, reference) = pair(0, point());
    builder.try_push(feature, reference).unwrap();
    builder.set_metadata(PageInfo { next_token: Some("native-token".into()), has_more: true, resume_mode: ResumeMode::Native }, LimitStatus { truncated: true, reasons: vec!["UPSTREAM_CAP".into(), "SKIPPED_FEATURES".into()] }, vec!["DIMENSIONS_REDUCED_TO_XY".into()], vec![FeatureError { index: 4, code: "UNSUPPORTED_TYPE".into(), message: "skipped by explicit request".into() }]).unwrap();
    builder.finish().unwrap()
}

fn consecutive_pages() -> (Vec<PushResult>, FeaturePage, LayerBudget) {
    let page_limits = PageLimits { max_features: 1, ..PageLimits::default() };
    let layer_limits = LayerLimits { max_features: 2, ..LayerLimits::default() };
    let mut first = PageBuilder::new(page_limits, layer_limits, LayerBudget::default()).unwrap();
    let (feature, reference) = pair(0, point());
    let a = first.try_push(feature, reference).unwrap();
    let (feature, reference) = pair(1, point());
    let b = first.try_push(feature, reference).unwrap();
    let (_, prior) = first.finish().unwrap();
    let mut second = PageBuilder::new(page_limits, layer_limits, prior).unwrap();
    let (feature, reference) = pair(1, point());
    let c = second.try_push(feature, reference).unwrap();
    let (feature, reference) = pair(2, point());
    let d = second.try_push(feature, reference).unwrap();
    let (page, budget) = second.finish().unwrap();
    (vec![a,b,c,d], page, budget)
}

fn metadata_failure() -> (Result<(), CoreError>, FeaturePage, FeaturePage) {
    let mut builder = PageBuilder::new(PageLimits { max_bytes: 600, ..PageLimits::default() }, LayerLimits::default(), LayerBudget::default()).unwrap();
    let (feature, reference) = pair(0, point());
    builder.try_push(feature, reference).unwrap();
    let before = builder.page().clone();
    let error = builder.set_metadata(PageInfo::default(), LimitStatus::default(), vec!["경고".repeat(1000)], vec![]);
    (error, before, builder.page().clone())
}

fn default_limits() -> (PageLimits, LayerLimits) { (PageLimits::default(), LayerLimits::default()) }

fn second_feature_byte_boundaries(bytes: u64) -> Vec<Result<PushResult, CoreError>> {
    [bytes - 1, bytes, bytes + 1].into_iter().map(|max_bytes| {
        let mut builder = PageBuilder::new(PageLimits { max_bytes, ..PageLimits::default() }, LayerLimits::default(), LayerBudget::default()).unwrap();
        let (feature, reference) = pair(0, point());
        builder.try_push(feature, reference).unwrap();
        let (feature, reference) = pair(1, point());
        builder.try_push(feature, reference)
    }).collect()
}

#[test]
fn page와_layer의_기본_상한은_계약값을_따른다() {
    // given
    let expected_page = PageLimits { max_features: 1000, max_coordinates: 100_000, max_bytes: 8 * 1024 * 1024 };
    let expected_layer = LayerLimits { max_features: 10_000, max_coordinates: 250_000, max_bytes: 32 * 1024 * 1024 };
    // when
    let actual = default_limits();
    // then
    assert_eq!(actual.0, expected_page);
    assert_eq!(actual.1, expected_layer);
}

#[test]
fn feature_상한은_정확히_허용하고_다음_feature만_다음_page로_보낸다() {
    // given
    let limits = PageLimits { max_features: 2, ..PageLimits::default() };
    // when
    let actual = scenario(limits, LayerLimits::default(), LayerBudget::default(), vec![point(), point(), point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::Added), Ok(PushResult::PageFull)]);
    assert_eq!(actual.1.features.len(), 2);
    assert_eq!(actual.1.row_references.len(), 2);
    assert!(!actual.1.limits.truncated);
}

#[test]
fn 좌표_상한은_중첩_collection의_합계를_사용한다() {
    // given
    let geometry = Some(Geometry::GeometryCollection { geometries: vec![Geometry::LineString { coordinates: vec![vec![1.0,2.0],vec![3.0,4.0]] }, Geometry::MultiPoint { coordinates: vec![vec![5.0,6.0]] }] });
    let limits = PageLimits { max_coordinates: 3, ..PageLimits::default() };
    // when
    let actual = scenario(limits, LayerLimits::default(), LayerBudget::default(), vec![geometry, point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::PageFull)]);
    assert_eq!(actual.1.coordinate_count().unwrap(), 3);
    assert_eq!(actual.1.features.len(), actual.1.row_references.len());
}

#[test]
fn utf8_properties와_wrapper와_reference와_envelope_전체_bytes를_센다() {
    // given
    let (feature, reference) = pair(0, point());
    let expected = FeaturePage { features: vec![feature], row_references: vec![reference], ..FeaturePage::default() };
    let serialized = serde_json::to_string(&expected).unwrap();
    let expected_bytes = serialized.len() as u64;
    // when
    let actual = byte_boundary_attempts(expected_bytes);
    // then
    assert_eq!(actual[0].as_ref().unwrap_err().code, ErrorCode::FeatureTooLarge);
    assert_eq!(actual[1], Ok(PushResult::Added));
    assert_eq!(actual[2], Ok(PushResult::Added));
    assert!(serialized.len() > serialized.chars().count());
    assert_eq!(expected.serialized_bytes().unwrap(), expected_bytes);
}

#[test]
fn 두번째_feature의_utf8_bytes와_배열_구분자도_정확히_센다() {
    // given
    let (first, first_reference) = pair(0, point());
    let (second, second_reference) = pair(1, point());
    let expected = FeaturePage { features: vec![first, second], row_references: vec![first_reference, second_reference], ..FeaturePage::default() };
    let bytes = serde_json::to_vec(&expected).unwrap().len() as u64;
    // when
    let actual = second_feature_byte_boundaries(bytes);
    // then
    assert_eq!(actual, vec![Ok(PushResult::PageFull), Ok(PushResult::Added), Ok(PushResult::Added)]);
}

#[test]
fn 한_feature가_너무_크면_분할하거나_일부_추가하지_않는다() {
    // given
    let limits = PageLimits { max_coordinates: 1, ..PageLimits::default() };
    let geometry = Some(Geometry::LineString { coordinates: vec![vec![1.0,2.0],vec![3.0,4.0]] });
    // when
    let actual = scenario(limits, LayerLimits::default(), LayerBudget::default(), vec![geometry]);
    // then
    assert_eq!(actual.0[0].as_ref().unwrap_err().code, ErrorCode::FeatureTooLarge);
    assert!(actual.1.features.is_empty());
    assert!(actual.1.row_references.is_empty());
}

#[test]
fn 누적_layer_feature_상한은_이전_page를_포함한다() {
    // given
    let prior = LayerBudget { features: 1, ..LayerBudget::default() };
    let limits = LayerLimits { max_features: 2, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, prior, vec![point(), point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::LayerFull)]);
    assert_eq!(actual.1.features.len(), 1);
}

#[test]
fn 누적_layer_좌표_상한은_이전_page를_포함한다() {
    // given
    let prior = LayerBudget { coordinates: 2, ..LayerBudget::default() };
    let limits = LayerLimits { max_coordinates: 3, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, prior, vec![point(), point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::LayerFull)]);
    assert_eq!(actual.1.coordinate_count().unwrap(), 1);
}

#[test]
fn 누적_layer_bytes는_정확한_경계까지_허용한다() {
    // given
    let (feature, reference) = pair(0, point());
    let expected_bytes = FeaturePage { features: vec![feature], row_references: vec![reference], ..FeaturePage::default() }.serialized_bytes().unwrap();
    let prior = LayerBudget { bytes: 900, ..LayerBudget::default() };
    let limits = LayerLimits { max_bytes: 900 + expected_bytes + 128, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, prior, vec![point(), point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::LayerFull)]);
    assert!(actual.1.serialized_bytes().unwrap() > expected_bytes);
    assert!(actual.1.serialized_bytes().unwrap() <= expected_bytes + 128);
    assert!(actual.1.limits.truncated);
    assert_eq!(actual.1.limits.reasons, vec!["LAYER_LIMIT"]);
}

#[test]
fn 누적_bytes_정확한_상한의_첫_feature는_허용한다() {
    // given
    let (feature, reference) = pair(0, point());
    let bytes = FeaturePage { features: vec![feature], row_references: vec![reference], ..FeaturePage::default() }.serialized_bytes().unwrap();
    let prior = LayerBudget { bytes: 900, ..LayerBudget::default() };
    let limits = LayerLimits { max_bytes: 900 + bytes, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, prior, vec![point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added)]);
    assert_eq!(actual.1.serialized_bytes().unwrap(), bytes);
}

#[test]
fn truncation_envelope도_bytes_안에_들도록_feature와_reference를_함께_제거한다() {
    // given
    let (feature, reference) = pair(0, point());
    let bytes = FeaturePage { features: vec![feature], row_references: vec![reference], ..FeaturePage::default() }.serialized_bytes().unwrap();
    let limits = LayerLimits { max_bytes: bytes, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, LayerBudget::default(), vec![point(), point()]);
    // then
    assert_eq!(actual.0, vec![Ok(PushResult::Added), Ok(PushResult::LayerFull)]);
    assert!(actual.1.features.is_empty());
    assert!(actual.1.row_references.is_empty());
    assert!(actual.1.limits.truncated);
    assert!(!actual.1.page.has_more);
    assert!(actual.1.serialized_bytes().unwrap() <= bytes);
}

#[test]
fn pagefull의_미적용_feature는_다음_page의_누적_budget에만_추가한다() {
    // given
    let expected = vec![PushResult::Added, PushResult::PageFull, PushResult::Added, PushResult::LayerFull];
    // when
    let actual = consecutive_pages();
    // then
    assert_eq!(actual.0, expected);
    assert_eq!(actual.2.features, 2);
    assert_eq!(actual.2.coordinates, 2);
    assert_eq!(actual.1.features[0].id, "result:0:1");
    assert_eq!(actual.1.limits.reasons, vec!["LAYER_LIMIT"]);
}

#[test]
fn metadata_bytes_초과도_기존_page를_변경하지_않는다() {
    // given
    let expected_code = ErrorCode::ResourceLimit;
    // when
    let actual = metadata_failure();
    // then
    assert_eq!(actual.0.unwrap_err().code, expected_code);
    assert_eq!(actual.1, actual.2);
}

#[test]
fn counter_overflow는_상한_통과로_오판하지_않는다() {
    // given
    let prior = LayerBudget { features: u64::MAX, ..LayerBudget::default() };
    let limits = LayerLimits { max_features: u64::MAX, ..LayerLimits::default() };
    // when
    let actual = scenario(PageLimits::default(), limits, prior, vec![point()]);
    // then
    assert_eq!(actual.0[0].as_ref().unwrap_err().code, ErrorCode::ResourceLimit);
    assert!(actual.1.features.is_empty());
}

#[test]
fn upstream_상한과_skip_이유를_유지하고_native_page와_구분한다() {
    // given
    let expected_reasons = vec!["UPSTREAM_CAP", "SKIPPED_FEATURES"];
    // when
    let actual = capped_page();
    // then
    assert!(actual.0.limits.truncated);
    assert_eq!(actual.0.limits.reasons, expected_reasons);
    assert_eq!(actual.0.page.resume_mode, ResumeMode::Native);
    assert!(actual.0.page.has_more);
    assert_eq!(actual.0.feature_errors[0].index, 4);
    assert_eq!(actual.1.bytes, actual.0.serialized_bytes().unwrap());
    assert_eq!(actual.1.coordinates, 1);
    assert_eq!(actual.1.features, 1);
}

#[test]
fn no_pk_snapshot_reference는_null_identity와_ordinal을_보존한다() {
    // given
    let wire = json!({"features":[{"type":"Feature","id":"snapshot:19","geometry":null,"properties":{"id":null}}],"row_references":[{"feature_id":"snapshot:19","identity":null,"snapshot_id":"snapshot","row_ordinal":19}],"page":{"next_token":"continuation","has_more":true,"resume_mode":"materialized"},"limits":{"truncated":false,"reasons":[]},"warnings":[],"feature_errors":[]});
    // when
    let actual: FeaturePage = serde_json::from_value(wire.clone()).unwrap();
    // then
    assert!(actual.validate().is_ok());
    assert_eq!(serde_json::to_value(actual).unwrap(), wire);
}

#[test]
fn 잘못된_page_token이나_feature_reference는_거부한다() {
    // given
    let (feature, mut reference) = pair(0, point());
    reference.feature_id = "other".into();
    let page = FeaturePage { features: vec![feature], row_references: vec![reference], ..FeaturePage::default() };
    // when
    let actual = page.validate();
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn driver의_empty_geometry는_null이지만_properties의_원본은_남는다() {
    // given
    let wire = json!({"type":"Feature","id":"empty","geometry":{"type":"Point","coordinates":[]},"properties":{"g":{"kind":"spatial","native_type":"geometry","srid":4326,"dimensions":"XYZ","encoding":"ewkb-base64","value":"AQEA-original==","is_empty":true}}});
    // when
    let actual: Feature = serde_json::from_value(wire.clone()).unwrap();
    // then
    assert_eq!(actual.geometry, None);
    assert_eq!(serde_json::to_value(actual).unwrap()["properties"], wire["properties"]);
}

#[test]
fn feature의_지원불가_형식은_typed_error를_유지한다() {
    // given
    let wire = json!({"type":"Feature","id":"curve","geometry":{"type":"CircularString","coordinates":[]},"properties":{}});
    // when
    let actual = Feature::from_json(&wire);
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::UnsupportedType);
}

#[test]
fn continuation은_resume_mode없이_발급하지_않는다() {
    // given
    let page = PageInfo { next_token: Some("token".into()), has_more: true, resume_mode: ResumeMode::None };
    // when
    let actual = page.validate();
    // then
    assert_eq!(actual.unwrap_err().code, ErrorCode::InvalidArgument);
}
