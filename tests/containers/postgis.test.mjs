import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import test, { before, after } from "node:test";
import pg from "pg";
import { GenericContainer, Wait } from "testcontainers";
import {
  buildPostgisSchemaQuery, buildSpatialColumnsQuery, buildTableQuery,
  parsePostgisSchema, parseSpatialColumns, parseTableRows, sqlIdentifier,
} from "../../ui/src/compat-query.ts";

const image = "postgis/postgis:17-3.5@sha256:01a6a70e41e6c4467c8f55f6063555ed72db2d6662cd0d571040d42eadaeb6f6";
const extensionSchema = "Geo Extensions";
const schema = 'data"schema';
const table = 'map"points';
const namespace = sqlIdentifier(extensionSchema);
const relation = `${sqlIdentifier(schema)}.${sqlIdentifier(table)}`;
let container;
let admin;
let client;

before(async () => {
  const password = randomUUID();
  container = await new GenericContainer(image)
    .withPlatform("linux/amd64")
    .withEnvironment({ POSTGRES_PASSWORD: password })
    .withExposedPorts(5432)
    .withWaitStrategy(Wait.forLogMessage("database system is ready to accept connections", 2))
    .withStartupTimeout(180_000)
    .start();
  const connection = {
    host: container.getHost(), port: container.getMappedPort(5432),
    user: "postgres", password, connectionTimeoutMillis: 10_000,
    statement_timeout: 15_000,
  };
  admin = new pg.Client({ ...connection, database: "postgres" });
  await admin.connect();
  await admin.query("CREATE DATABASE map_fixture TEMPLATE template0");
  client = new pg.Client({ ...connection, database: "map_fixture" });
  await client.connect();
  await client.query(`CREATE SCHEMA ${namespace}`);
  await client.query(`CREATE EXTENSION postgis SCHEMA ${namespace}`);
  await client.query(`CREATE SCHEMA ${sqlIdentifier(schema)}`);
  await client.query(`CREATE TABLE ${relation} (
    id integer PRIMARY KEY, title text,
    geom ${namespace}.geometry(Point, 3857),
    geog ${namespace}.geography(Point, 4326))`);
  await client.query(`INSERT INTO ${relation} VALUES
    (1, '서울', ${namespace}.st_transform(${namespace}.st_setsrid(${namespace}.st_makepoint(127, 37), 4326), 3857),
      ${namespace}.st_setsrid(${namespace}.st_makepoint(127, 37), 4326)::${namespace}.geography)`);
  await client.query(`CREATE TABLE ${sqlIdentifier(schema)}.unknown_srid (geom ${namespace}.geometry)`);
  await client.query(`INSERT INTO ${sqlIdentifier(schema)}.unknown_srid VALUES (${namespace}.st_makepoint(127, 37))`);
  await client.query(`CREATE TABLE ${sqlIdentifier(schema)}.many_points AS
    SELECT id, ${namespace}.st_setsrid(${namespace}.st_makepoint(127, 37), 4326) AS geom FROM generate_series(1, 1001) AS id`);
  await client.query(`CREATE TABLE ${sqlIdentifier(schema)}.empty_points (geom ${namespace}.geometry(Point,4326))`);
  await client.query("SET search_path TO pg_catalog");
}, { timeout: 240_000 });

after(async () => {
  try {
    await Promise.allSettled([client?.end(), admin?.end()]);
  } finally {
    await container?.stop();
  }
});

async function rows(sql, db = client) {
  return (await db.query({ text: sql, rowMode: "array" })).rows;
}

async function mapPage(options = {}) {
  const input = { schema, table, column: "geom", extensionSchema, ...options };
  return parseTableRows(await rows(buildTableQuery(input)), input.sourceSrid);
}

test("기본 public 설치에서도 PostGIS 스키마를 찾는다", async () => {
  // given
  const sql = buildPostgisSchemaQuery();
  // when
  const actual = parsePostgisSchema(await rows(sql, admin));
  // then
  assert.equal(actual, "public");
});

test("search_path에 없는 공백과 대문자 포함 PostGIS 스키마를 찾는다", async () => {
  // given
  const sql = buildPostgisSchemaQuery();
  // when
  const actual = parsePostgisSchema(await rows(sql));
  // then
  assert.equal(actual, extensionSchema);
});

test("별도 스키마의 geometry와 geography 열을 모두 찾는다", async () => {
  // given
  const sql = buildSpatialColumnsQuery(schema, table, extensionSchema);
  // when
  const actual = parseSpatialColumns(await rows(sql));
  // then
  assert.deepEqual(actual.map(({ name, type, srid }) => ({ name, type, srid })).sort((a, b) => a.name.localeCompare(b.name)), [
    { name: "geog", type: "geography", srid: 4326 },
    { name: "geom", type: "geometry", srid: 3857 },
  ]);
});

test("별도 스키마의 투영 좌표를 경위도로 변환하고 속성을 보존한다", async () => {
  // given
  const options = { column: "geom" };
  // when
  const actual = await mapPage(options);
  // then
  assert.deepEqual(actual.features[0].geometry, { type: "Point", coordinates: [127, 37] });
  assert.equal(actual.features[0].properties.title, "서울");
  assert.equal(actual.features[0].properties.id, 1);
  assert.equal(actual.truncated, false);
});

test("geography를 설치 스키마의 geometry로 변환해 지도에 표시한다", async () => {
  // given
  const options = { column: "geog" };
  // when
  const actual = await mapPage(options);
  // then
  assert.deepEqual(actual.features[0].geometry, { type: "Point", coordinates: [127, 37] });
});

test("원본 SRID가 0이면 좌표계를 추측하지 않는다", async () => {
  // given
  const options = { table: "unknown_srid" };
  // when
  const actual = await mapPage(options).catch(error => error);
  // then
  assert.equal(actual.code, "SRID_REQUIRED");
});

test("SRID가 0인 행에는 사용자가 지정한 원본 좌표계를 적용한다", async () => {
  // given
  const options = { table: "unknown_srid", sourceSrid: 4326 };
  // when
  const actual = await mapPage(options);
  // then
  assert.deepEqual(actual.features[0].geometry, { type: "Point", coordinates: [127, 37] });
});

test("실제 조회가 1000개 지형 한도를 넘으면 잘림을 표시한다", async () => {
  // given
  const options = { table: "many_points" };
  // when
  const actual = await mapPage(options);
  // then
  assert.equal(actual.features.length, 1000);
  assert.equal(actual.truncated, true);
});

test("공간 행이 없는 실제 테이블은 빈 지도 결과를 반환한다", async () => {
  // given
  const options = { table: "empty_points" };
  // when
  const actual = await mapPage(options);
  // then
  assert.deepEqual(actual.features, []);
  assert.equal(actual.truncated, false);
});
