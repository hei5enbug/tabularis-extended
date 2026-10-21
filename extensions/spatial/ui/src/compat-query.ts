import { PAGE_LIMIT, SpatialUiError, countGeometry, type Feature, type Geometry, type Json } from "./models";

export interface CompatSpatialColumn {
  name: string;
  type: "geometry" | "geography";
  srid: number | null;
  dimensions: number | null;
}

export interface CompatTableQuery {
  schema: string;
  table: string;
  column: string;
  sourceSrid?: number;
}

export interface CompatFeaturePage {
  features: Feature[];
  coordinates: number;
  bytes: number;
  truncated: boolean;
}

export function sqlLiteral(value: string): string {
  if (value.includes("\0")) throw new SpatialUiError("INVALID_ARGUMENT", "PostgreSQL 이름에 NUL 문자를 사용할 수 없습니다.");
  return `'${value.replaceAll("'", "''")}'`;
}

export function sqlIdentifier(value: string): string {
  if (!value || value.includes("\0")) throw new SpatialUiError("INVALID_ARGUMENT", "PostgreSQL 식별자가 유효하지 않습니다.");
  return `"${value.replaceAll('"', '""')}"`;
}

export function buildSpatialColumnsQuery(schema: string, table: string): string {
  const schemaValue = sqlLiteral(schema);
  const tableValue = sqlLiteral(table);
  return `SELECT f_geometry_column AS column_name, 'geometry' AS spatial_type, srid, coord_dimension FROM public.geometry_columns WHERE f_table_schema = ${schemaValue} AND f_table_name = ${tableValue} UNION ALL SELECT f_geography_column AS column_name, 'geography' AS spatial_type, srid, coord_dimension FROM public.geography_columns WHERE f_table_schema = ${schemaValue} AND f_table_name = ${tableValue}`;
}

export function parseSpatialColumns(rows: unknown[][]): CompatSpatialColumn[] {
  const columns: CompatSpatialColumn[] = [];
  for (const row of rows) {
    if (!Array.isArray(row) || row.length < 4 || typeof row[0] !== "string" || !row[0] || (row[1] !== "geometry" && row[1] !== "geography")) continue;
    const sridValue = row[2] == null ? null : Number(row[2]);
    const dimensionsValue = row[3] == null ? null : Number(row[3]);
    columns.push({
      name: row[0],
      type: row[1],
      srid: sridValue !== null && Number.isSafeInteger(sridValue) && sridValue >= 0 ? sridValue : null,
      dimensions: dimensionsValue !== null && Number.isSafeInteger(dimensionsValue) && dimensionsValue > 0 ? dimensionsValue : null,
    });
  }
  return columns;
}

export function buildTableQuery(input: CompatTableQuery): string {
  if (input.sourceSrid !== undefined && (!Number.isSafeInteger(input.sourceSrid) || input.sourceSrid < 1 || input.sourceSrid > 999_999)) {
    throw new SpatialUiError("INVALID_ARGUMENT", "SRID는 1부터 999999 사이의 정수여야 합니다.");
  }
  const table = `${sqlIdentifier(input.schema)}.${sqlIdentifier(input.table)}`;
  const column = `"t".${sqlIdentifier(input.column)}::geometry`;
  const sourceGeometry = input.sourceSrid === undefined
    ? column
    : `(CASE WHEN ST_SRID(${column}) = 0 THEN ST_SetSRID(${column}, ${input.sourceSrid}) ELSE ${column} END)`;
  const emptyGeometry = `ST_Force2D(${sourceGeometry})`;
  const noSourceSridZero = input.sourceSrid === undefined ? `WHEN ST_SRID(${column}) = 0 THEN NULL ` : "";
  return `SELECT CASE ${noSourceSridZero}WHEN ST_IsEmpty(${emptyGeometry}) THEN NULL ELSE ST_AsGeoJSON(ST_Transform(${emptyGeometry}, 4326), 9, 0) END AS "__geojson", ST_SRID(${column}) AS "__source_srid", to_jsonb("t") - ${sqlLiteral(input.column)} AS "__properties" FROM ${table} AS "t" WHERE "t".${sqlIdentifier(input.column)} IS NOT NULL LIMIT ${PAGE_LIMIT.features + 1}`;
}

function parseGeometry(value: unknown): Geometry | null {
  if (value == null) return null;
  let parsed: unknown = value;
  if (typeof value === "string") {
    try { parsed = JSON.parse(value); }
    catch { throw new SpatialUiError("INVALID_GEOMETRY", "PostGIS에서 받은 GeoJSON을 읽을 수 없습니다."); }
  }
  countGeometry(parsed);
  return parsed as Geometry;
}

function parseProperties(value: unknown): Record<string, Json> {
  let parsed: unknown = value;
  if (typeof value === "string") {
    try { parsed = JSON.parse(value); }
    catch { throw new SpatialUiError("INVALID_ARGUMENT", "PostGIS에서 받은 속성을 읽을 수 없습니다."); }
  }
  if (parsed == null) return {};
  if (typeof parsed !== "object" || Array.isArray(parsed)) throw new SpatialUiError("INVALID_ARGUMENT", "PostGIS에서 받은 속성 객체가 유효하지 않습니다.");
  return parsed as Record<string, Json>;
}

export function parseTableRows(rows: unknown[][], sourceSrid?: number): CompatFeaturePage {
  if (sourceSrid !== undefined && (!Number.isSafeInteger(sourceSrid) || sourceSrid < 1 || sourceSrid > 999_999)) {
    throw new SpatialUiError("INVALID_ARGUMENT", "SRID는 1부터 999999 사이의 정수여야 합니다.");
  }
  const features: Feature[] = [];
  let coordinates = 0;
  let truncated = rows.length > PAGE_LIMIT.features;
  const encoder = new TextEncoder();
  const collectionPrefix = encoder.encode('{"type":"FeatureCollection","features":[').byteLength;
  const collectionSuffix = encoder.encode("]}").byteLength;
  let bytes = collectionPrefix;
  for (const [index, row] of rows.slice(0, PAGE_LIMIT.features).entries()) {
    if (!Array.isArray(row) || row.length < 3) throw new SpatialUiError("INVALID_ARGUMENT", "PostGIS 행 응답이 유효하지 않습니다.");
    const rowSrid = Number(row[1]);
    if (!Number.isSafeInteger(rowSrid) || rowSrid < 0) throw new SpatialUiError("INVALID_ARGUMENT", "행의 SRID를 확인할 수 없습니다.");
    if (rowSrid === 0 && sourceSrid === undefined) throw new SpatialUiError("SRID_REQUIRED", "SRID가 0인 행이 있습니다. 원본 좌표계 SRID를 입력한 뒤 다시 조회하세요.");
    const geometry = parseGeometry(row[0]);
    const properties = parseProperties(row[2]);
    const feature: Feature = { type: "Feature", id: `compat:${index + 1}`, geometry, properties };
    const featureCoordinates = geometry ? countGeometry(geometry) : 0;
    const nextCoordinates = coordinates + featureCoordinates;
    const nextBytes = bytes + (features.length ? 1 : 0) + encoder.encode(JSON.stringify(feature)).byteLength;
    if (nextCoordinates > PAGE_LIMIT.coordinates || nextBytes + collectionSuffix > PAGE_LIMIT.bytes) {
      truncated = true;
      break;
    }
    coordinates = nextCoordinates;
    bytes = nextBytes;
    features.push(feature);
  }
  bytes += collectionSuffix;
  return { features, coordinates, bytes, truncated };
}
