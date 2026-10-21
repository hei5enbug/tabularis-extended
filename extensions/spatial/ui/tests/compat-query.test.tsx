import { describe, expect, it } from "vitest";
import { PAGE_LIMIT, SpatialUiError } from "../src/models";
import { buildSpatialColumnsQuery, buildTableQuery, parseSpatialColumns, parseTableRows } from "../src/compat-query";

describe("기본 PostGIS 지도 쿼리", () => {
  it("공간 카탈로그 이름과 테이블 문자열을 안전하게 조회합니다.", () => {
    // given
    const schema = "public' OR 'x'='x";
    const table = "roads' UNION SELECT secret";
    // when
    const sql = buildSpatialColumnsQuery(schema, table);
    // then
    expect(sql).toContain("FROM public.geometry_columns");
    expect(sql).toContain("FROM public.geography_columns");
    expect(sql).toContain("f_table_schema = 'public'' OR ''x''=''x'");
    expect(sql).toContain("f_table_name = 'roads'' UNION SELECT secret'");
  });

  it("geometry와 geography 열 정보를 같은 형식으로 읽습니다.", () => {
    // given
    const rows = [["shape", "geometry", "4326", "2"], ["shore", "geography", 0, 2], ["ignore", "raster", 4326, 2]];
    // when
    const columns = parseSpatialColumns(rows);
    // then
    expect(columns).toEqual([
      { name: "shape", type: "geometry", srid: 4326, dimensions: 2 },
      { name: "shore", type: "geography", srid: 0, dimensions: 2 },
    ]);
  });

  it("테이블과 열 식별자를 인용하고 제한된 읽기 전용 조회를 만듭니다.", () => {
    // given
    const input = { schema: "city\"data", table: "roads; DROP TABLE users", column: "geom\"etry" };
    // when
    const sql = buildTableQuery(input);
    // then
    expect(sql).toContain('FROM "city""data"."roads; DROP TABLE users" AS "t"');
    expect(sql).toContain('"t"."geom""etry"::geometry');
    expect(sql).toContain(`to_jsonb("t") - 'geom"etry'`);
    expect(sql).toContain(`LIMIT ${PAGE_LIMIT.features + 1}`);
    expect(sql).toContain("ST_Transform(ST_Force2D");
    expect(sql).toContain("AS \"__geojson\"");
  });

  it("SRID 0 행은 양의 원본 SRID가 없으면 거부하고 0인 행에만 설정합니다.", () => {
    // given
    const rows = [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 0, { name: "A" }]];
    // when
    let failure: unknown;
    try { parseTableRows(rows); } catch (error) { failure = error; }
    const sql = buildTableQuery({ schema: "public", table: "roads", column: "geom", sourceSrid: 3857 });
    // then
    expect(failure).toBeInstanceOf(SpatialUiError);
    expect((failure as SpatialUiError).code).toBe("SRID_REQUIRED");
    expect(sql).toContain("WHEN ST_SRID(\"t\".\"geom\"::geometry) = 0 THEN ST_SetSRID");
    expect(sql).toContain("ELSE \"t\".\"geom\"::geometry END");
  });

  it("기존 geometry 검증을 적용하고 feature·좌표·바이트 상한을 지킵니다.", () => {
    // given
    const point = JSON.stringify({ type: "Point", coordinates: [1, 2] });
    const featureRows = Array.from({ length: PAGE_LIMIT.features + 1 }, () => [point, 4326, {}]);
    const line = JSON.stringify({ type: "LineString", coordinates: Array.from({ length: 2_000 }, () => [1, 2]) });
    const coordinateRows = Array.from({ length: 51 }, () => [line, 4326, {}]);
    const byteRows = [[point, 4326, { large: "x".repeat(PAGE_LIMIT.bytes) }]];
    // when
    const featurePage = parseTableRows(featureRows);
    const coordinatePage = parseTableRows(coordinateRows);
    const bytePage = parseTableRows(byteRows);
    // then
    expect(featurePage.features).toHaveLength(PAGE_LIMIT.features);
    expect(featurePage.truncated).toBe(true);
    expect(coordinatePage.coordinates).toBe(PAGE_LIMIT.coordinates);
    expect(coordinatePage.truncated).toBe(true);
    expect(bytePage.features).toHaveLength(0);
    expect(bytePage.truncated).toBe(true);
    expect(bytePage.bytes).toBeLessThanOrEqual(PAGE_LIMIT.bytes);
  });
});
