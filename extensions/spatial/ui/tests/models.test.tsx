import { describe, expect, it } from "vitest";
import { validateRequest, validateResponse } from "./schema";
import { cameraBounds, countGeometry, PAGE_LIMIT, parsePage, validateBasemap, WORLD } from "../src/models";
import { displayFeatures } from "../src/render";
import { pageFixture, rawWrapper, response } from "./fixtures";
import postgisPage from "./fixtures/postgis-page.json";

function captureError(action: () => unknown): unknown { try { action(); return null; } catch (error) { return error; } }

describe("공간 wire와 좌표", () => {
  it("실제_P1_PostGIS_page의_EWKB와_SRID를_손실없이_소비한다", () => {
    // given
    const input = structuredClone(postgisPage);
    // when
    const actual = parsePage(input);
    // then
    expect(actual.features[0].geometry).toEqual({ type: "Point", coordinates: [10,0] });
    expect(actual.features[0].properties.g).toEqual({ kind: "spatial", native_type: "geometry", srid: 3857, dimensions: "XYZ", encoding: "ewkb-base64", value: "AQEAAKARDwAAn0du6Gr8MEEAAAAAAAAAAAAAAAAAABxA", is_empty: false });
    expect(actual.row_references[0].identity).toEqual({ columns: ["id"], values: [1] });
    expect(actual.page.resume_mode).toBe("native");
    expect(actual.limits.truncated).toBe(false);
  });

  it("지원불가_geometry_type은_명확한_코드를_반환한다", () => {
    // given
    const input = { type: "CircularString", coordinates: [] };
    // when
    const actual = captureError(() => countGeometry(input));
    // then
    expect(actual).toMatchObject({ code: "UNSUPPORTED_TYPE" });
  });
  it("모든 geometry와 collection의_xy_좌표를_집계한다", () => {
    // given
    const inputs = [{ type: "Point", coordinates: [1,2] }, { type: "LineString", coordinates: [[179,2],[-179,3]] }, { type: "Polygon", coordinates: [[[0,0],[2,0],[0,2],[0,0]],[[0.1,0.1],[0.1,0.2],[0.2,0.1],[0.1,0.1]]] }, { type: "MultiPoint", coordinates: [[1,2],[3,4]] }, { type: "MultiLineString", coordinates: [[[1,2],[3,4]],[[5,6],[7,8]]] }, { type: "MultiPolygon", coordinates: [[[[0,0],[2,0],[0,2],[0,0]]]] }, { type: "GeometryCollection", geometries: [{ type: "Point", coordinates: [1,2] }, { type: "GeometryCollection", geometries: [{ type: "MultiPoint", coordinates: [[1,2],[3,4]] }] }] }];
    // when
    const actual = inputs.map(value => countGeometry(value));
    // then
    expect(actual).toEqual([1,2,8,2,4,4,3]);
  });

  it("잘못된_xy와_형식은_typed_error로_거부한다", () => {
    // given
    const invalid = [{ type: "Point", coordinates: [1,2,3] }, { type: "Point", coordinates: [Infinity,2] }, { type: "Point", coordinates: [0,91] }, { type: "LineString", coordinates: [[1,2]] }, { type: "Polygon", coordinates: [[[0,0],[1,0],[1,1],[0,1]]] }];
    // when
    const actual = invalid.map(input => { try { countGeometry(input); return null; } catch (error) { return error; } });
    // then
    expect(actual).toEqual(invalid.map(() => expect.objectContaining({ code: "INVALID_GEOMETRY" })));
  });

  it("원본_EWKB와_properties를_MapLibre에_넣지_않는다", () => {
    // given
    const input = structuredClone(pageFixture.features);
    // when
    const actual = displayFeatures(input, "layer");
    // then
    expect(actual[0].properties).toEqual({ feature_id: input[0].id, layer_id: "layer" });
    expect(JSON.stringify(actual)).not.toContain(rawWrapper.value);
    expect(input[0].properties.g).toEqual(rawWrapper);
    expect(actual[0].geometry).toEqual(input[0].geometry);
  });

  it("collection을_평면화해도_line_순서와_feature_identity를_보존한다", () => {
    // given
    const input = { ...pageFixture.features[0], geometry: { type: "GeometryCollection" as const, geometries: [{ type: "LineString" as const, coordinates: [[179,2],[-179,3]] as [number,number][] }, { type: "MultiPoint" as const, coordinates: [[1,2],[3,4]] as [number,number][] }] } };
    // when
    const actual = displayFeatures([input], "layer");
    // then
    expect(actual).toHaveLength(2);
    expect(actual[0].geometry).toEqual(input.geometry.geometries[0]);
    expect(actual.map(feature => feature.properties.feature_id)).toEqual([input.id,input.id]);
  });

  it("EMPTY는_null로_정규화하고_원본_wrapper는_수정하지_않는다", () => {
    // given
    const input = structuredClone(pageFixture);
    (input.features[0] as unknown as { geometry: unknown }).geometry = { type: "Point", coordinates: [] };
    // when
    const actual = parsePage(input);
    // then
    expect(actual.features[0].geometry).toBeNull();
    expect(actual.features[0].properties.g).toEqual(rawWrapper);
    expect(input.features[0].geometry).toEqual({ type: "Point", coordinates: [] });
  });

  it("카메라만_극지_위도를_clamp한다", () => {
    // given
    const input = structuredClone(WORLD);
    // when
    const actual = cameraBounds(input);
    // then
    expect(actual).toEqual([[-180,-85.05112878],[180,85.05112878]]);
    expect(input.north).toBe(90);
    expect(pageFixture.features[0].geometry).toEqual({ type: "Point", coordinates: [179,89] });
  });

  it("dateline과_zero_width와_world를_구분한다", () => {
    // given
    const inputs = [{ west: 179, east: -179, south: -10, north: 10, world: false }, { west: 10, east: 10, south: -10, north: 10, world: false }, { west: 0, east: 0, south: -10, north: 10, world: true }];
    // when
    const actual = inputs.map(cameraBounds);
    // then
    expect(actual).toEqual([[[179,-10],[181,10]],[[10,-10],[10,10]],[[-180,-10],[180,10]]]);
  });

  it("page_상한과_utf8_properties_bytes를_검증한다", () => {
    // given
    const input = structuredClone(pageFixture);
    input.features[0].properties.large = "한".repeat(PAGE_LIMIT.bytes / 3 + 1);
    // when
    const actual = captureError(() => parsePage(input));
    // then
    expect(actual).toMatchObject({ code: "RESOURCE_LIMIT" });
  });

  it("userinfo_HTTP_basemap은_거부한다", () => {
    // given
    const input = { style_url: "https://user:secret@tiles.example/style.json", attribution: "제공자" };
    // when
    const actual = captureError(() => validateBasemap(input));
    // then
    expect(actual).toMatchObject({ code: "INVALID_ARGUMENT" });
  });

  it("실제_공유_schema가_snapshot_request와_spatial_response를_검증한다", () => {
    // given
    const request = { protocol_version: 1 as const, operation: "spatial.query_result" as const, request_id: "fixture-request", connection_id: "fixture-connection", input: { result_id: "snapshot", result_set_index: 2, column_index: 4, skip_invalid: false, longitude_mode: "preserve" as const } };
    const reply = response(request, pageFixture, "detail-cache");
    // when
    const actual = [validateRequest(request), validateResponse(reply)];
    // then
    expect(actual.every(result => result.valid)).toBe(true);
  });
});
