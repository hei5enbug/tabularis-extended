import { describe, expect, it } from "vitest";
import { PAGE_LIMIT, SpatialUiError } from "../src/models";
import { buildPostgisSchemaQuery, buildSpatialColumnsQuery, buildTableQuery, parsePostgisSchema, parseSpatialColumns, parseTableRows } from "../src/compat-query";

const extensionSchemas = [
  { name: "public", quoted: '"public"' },
  { name: "postgis_ext", quoted: '"postgis_ext"' },
  { name: 'post"gis\'; DROP SCHEMA public; --', quoted: '"post""gis\'; DROP SCHEMA public; --"' },
];

describe("기본 PostGIS 지도 쿼리", () => {
  it("시스템 카탈로그를 읽어 PostGIS 확장의 설치 스키마를 찾습니다.", () => {
    // given
    const extensionName = "postgis";
    // when
    const sql = buildPostgisSchemaQuery();
    // then
    expect(sql).toMatch(/^SELECT /);
    expect(sql).toContain("FROM pg_catalog.pg_extension AS e");
    expect(sql).toContain("JOIN pg_catalog.pg_namespace AS n ON n.oid = e.extnamespace");
    expect(sql).toContain(`WHERE e.extname = '${extensionName}'`);
    expect(sql).not.toContain("public");
  });

  it.each(extensionSchemas)("확장 스키마 $name의 원래 이름을 보존합니다.", ({ name }) => {
    // given
    const rows = [[name]];
    // when
    const schema = parsePostgisSchema(rows);
    // then
    expect(schema).toBe(name);
  });

  it("PostGIS 확장이 없으면 public으로 대체하지 않고 오류를 알립니다.", () => {
    // given
    const rows: unknown[][] = [];
    // when
    const parse = () => parsePostgisSchema(rows);
    // then
    expect(parse).toThrow("PostGIS 확장이 설치되어 있지 않습니다");
  });

  it.each([
    { reason: "여러 스키마가 반환되면", rows: [["public"], ["other"]] },
    { reason: "이름이 비어 있으면", rows: [[""]] },
    { reason: "이름이 null이면", rows: [[null]] },
    { reason: "이름이 숫자이면", rows: [[123]] },
    { reason: "이름이 없으면", rows: [[]] },
    { reason: "이름에 NUL 문자가 있으면", rows: [["post\0gis"]] },
  ])("$reason 잘못된 확장 스키마 응답을 거부합니다.", ({ rows }) => {
    // given
    const response: unknown[][] = rows;
    // when
    const parse = () => parsePostgisSchema(response);
    // then
    expect(parse).toThrow("유효한 스키마 하나가 필요합니다");
  });

  it.each(extensionSchemas)("$name 확장의 공간 카탈로그와 테이블 문자열을 안전하게 조회합니다.", ({ name, quoted }) => {
    // given
    const schema = "city' OR 'x'='x";
    const table = "roads' UNION SELECT secret";
    // when
    const sql = buildSpatialColumnsQuery(schema, table, name);
    // then
    expect(sql).toContain(`FROM ${quoted}.geometry_columns`);
    expect(sql).toContain(`FROM ${quoted}.geography_columns`);
    expect(sql).toContain("f_table_schema = 'city'' OR ''x''=''x'");
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

  it.each(extensionSchemas)("$name 스키마의 함수와 geometry 타입을 지정하고 테이블 식별자를 인용합니다.", ({ name, quoted }) => {
    // given
    const input = { schema: 'city"data', table: "roads; DROP TABLE users", column: 'geom"etry', extensionSchema: name, sourceSrid: 3857 };
    // when
    const sql = buildTableQuery(input);
    // then
    expect(sql).toContain('FROM "city""data"."roads; DROP TABLE users" AS "t"');
    expect(sql).toContain(`"t"."geom""etry"::${quoted}.geometry`);
    expect(sql).toContain(`to_jsonb("t") - 'geom"etry'`);
    expect(sql).toContain(`LIMIT ${PAGE_LIMIT.features + 1}`);
    expect(sql).toContain(`${quoted}.st_transform(${quoted}.st_force2d`);
    expect(sql).toContain(`${quoted}.st_srid(`);
    expect(sql).toContain(`${quoted}.st_setsrid(`);
    expect(sql).toContain(`${quoted}.st_isempty(`);
    expect(sql).toContain(`${quoted}.st_asgeojson(`);
    expect(sql).not.toMatch(/(?<!\.)\bst_\w+\(/i);
    expect(sql).not.toContain("::geometry");
    expect(sql).toContain('AS "__geojson"');
  });

  it.each([
    { kind: "확장 스키마", build: (_: string) => buildPostgisSchemaQuery() },
    { kind: "공간 카탈로그", build: (name: string) => buildSpatialColumnsQuery(name, name, name) },
    { kind: "테이블", build: (name: string) => buildTableQuery({ schema: name, table: name, column: name, extensionSchema: name }) },
  ])("$kind 조회에 SQL 구문처럼 보이는 이름을 넣어도 읽기 전용 SELECT 하나를 유지합니다.", ({ build }) => {
    // given
    const name = 'spatial"; SET search_path TO public; DELETE FROM secrets; --\'';
    // when
    const sql = build(name).replace(/"(?:[^"]|"")*"|'(?:[^']|'')*'/g, "quoted");
    // then
    expect(sql).toMatch(/^SELECT /);
    expect(sql).not.toMatch(/;|--|\b(?:SET|CREATE|INSERT|UPDATE|DELETE|DROP|ALTER|TRUNCATE)\b/i);
  });

  it("SRID 0 행은 양의 원본 SRID가 없으면 거부합니다.", () => {
    // given
    const rows = [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 0, { name: "A" }]];
    // when
    let failure: unknown;
    try { parseTableRows(rows); } catch (error) { failure = error; }
    // then
    expect(failure).toBeInstanceOf(SpatialUiError);
    expect((failure as SpatialUiError).code).toBe("SRID_REQUIRED");
  });

  it("SRID 0 행에만 원본 SRID를 설정하고 저장된 geometry와 SRID를 보존합니다.", () => {
    // given
    const input = { schema: "city", table: "roads", column: "geom", extensionSchema: "postgis_ext", sourceSrid: 3857 };
    // when
    const sql = buildTableQuery(input);
    // then
    expect(sql).toContain('WHEN "postgis_ext".st_srid("t"."geom"::"postgis_ext".geometry) = 0 THEN "postgis_ext".st_setsrid');
    expect(sql).toContain('ELSE "t"."geom"::"postgis_ext".geometry END');
    expect(sql).toContain('"postgis_ext".st_srid("t"."geom"::"postgis_ext".geometry) AS "__source_srid"');
    expect(sql).toContain(", 4326), 9, 0)");
  });

  it("기본 조회는 SRID 0 행을 변환하지 않고 원본 SRID를 반환합니다.", () => {
    // given
    const input = { schema: "public", table: "roads", column: "geom", extensionSchema: "public" };
    // when
    const sql = buildTableQuery(input);
    // then
    expect(sql).toContain('CASE WHEN "public".st_srid("t"."geom"::"public".geometry) = 0 THEN NULL');
    expect(sql).not.toContain("st_setsrid");
    expect(sql).toContain('"public".st_srid("t"."geom"::"public".geometry) AS "__source_srid"');
  });

  it.each([
    { limit: "feature", rows: () => Array.from({ length: PAGE_LIMIT.features + 1 }, () => [JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, {}]), count: PAGE_LIMIT.features, coordinates: PAGE_LIMIT.features },
    { limit: "좌표", rows: () => Array.from({ length: 51 }, () => [JSON.stringify({ type: "LineString", coordinates: Array.from({ length: 2_000 }, () => [1, 2]) }), 4326, {}]), count: 50, coordinates: PAGE_LIMIT.coordinates },
    { limit: "바이트", rows: () => [[JSON.stringify({ type: "Point", coordinates: [1, 2] }), 4326, { large: "x".repeat(PAGE_LIMIT.bytes) }]], count: 0, coordinates: 0 },
  ])("$limit 상한을 넘는 행을 제외하고 잘림을 표시합니다.", ({ rows, count, coordinates }) => {
    // given
    const response = rows();
    // when
    const page = parseTableRows(response);
    // then
    expect(page.features).toHaveLength(count);
    expect(page.coordinates).toBe(coordinates);
    expect(page.truncated).toBe(true);
    expect(page.bytes).toBeLessThanOrEqual(PAGE_LIMIT.bytes);
  });
});
