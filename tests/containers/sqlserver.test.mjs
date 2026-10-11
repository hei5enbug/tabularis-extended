import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { access } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import test, { before, after } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { GenericContainer, Wait } from "testcontainers";
import { RpcProcess } from "./rpc-process.mjs";

const image = "mcr.microsoft.com/mssql/server:2022-latest@sha256:4402d880dd4c34bfa7d8705e56a86cd6c88da80a1f6bbbe741f999e76264a090";
const binary = fileURLToPath(new URL(`../../target/debug/sqlserver-plugin${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
let container;
let plugin;
let probe;
let connection;

before(async () => {
  await access(binary);
  const password = `Tc!a1-${randomUUID()}`;
  container = await new GenericContainer(image)
    .withPlatform("linux/amd64")
    .withEnvironment({ ACCEPT_EULA: "Y", MSSQL_PID: "Developer", MSSQL_SA_PASSWORD: password })
    .withExposedPorts(1433)
    .withWaitStrategy(Wait.forLogMessage("SQL Server is now ready for client connections"))
    .withStartupTimeout(240_000)
    .start();
  connection = {
    driver: "sqlserver-plus", host: container.getHost(), port: container.getMappedPort(1433),
    username: "sa", password, database: "master", ssl_mode: "require",
    connection_id: "owned-container-fixture",
  };
  plugin = new RpcProcess(binary);
  probe = new RpcProcess(binary);
  for (const process of [plugin, probe]) {
    await ok(process, "initialize", { settings: { max_pool_size: 8, connect_timeout_seconds: 20 } });
    await query(process, "SELECT 1 AS ready");
  }
}, { timeout: 300_000 });

after(async () => {
  try {
    await Promise.allSettled([plugin?.stop(), probe?.stop()]);
  } finally {
    await container?.stop();
  }
});

async function ok(process, method, params) {
  const response = await process.request(method, params).result;
  assert.equal(response.error, undefined, `Unexpected RPC error code: ${response.error?.code}; message: ${response.error?.message}`);
  return response.result;
}

function query(process, sql) { return ok(process, "execute_query", { params: connection, query: sql }); }

function blocked(tag) {
  return plugin.request("execute_query", {
    params: connection,
    query: `SELECT /* ${tag} */ SUM(CONVERT(bigint, a.object_id) + b.object_id + c.object_id + d.object_id)
      FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c CROSS JOIN sys.all_objects d OPTION (MAXDOP 1)`,
  }, 45_000);
}

async function activeCount(tag) {
  const escaped = tag.replaceAll("'", "''");
  const result = await query(probe, `SELECT COUNT(*) FROM sys.dm_exec_requests r
    CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) t
    WHERE r.session_id <> @@SPID AND t.text LIKE '%${escaped}%'`);
  return Number(result.rows[0][0]);
}

async function awaitActive(tag, expected) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (await activeCount(tag) === expected) return;
    await delay(50);
  }
  throw new Error(`Owned query did not reach expected server activity count ${expected}`);
}

test("실제 SQL Server가 중복 열 이름과 값 타입을 순서대로 반환한다", async () => {
  // given
  const sql = "SELECT CAST(7 AS int) AS same, CAST(N'한글' AS nvarchar(20)) AS same, CAST(NULL AS int) AS missing";
  // when
  const actual = await query(plugin, sql);
  // then
  assert.deepEqual(actual.columns, ["same", "same", "missing"]);
  assert.deepEqual(actual.rows, [[7, "한글", null]]);
});

test("실제 빈 결과도 열 이름을 보존한다", async () => {
  // given
  const sql = "SELECT CAST(1 AS int) AS empty_column WHERE 1 = 0";
  // when
  const actual = await query(plugin, sql);
  // then
  assert.deepEqual(actual.columns, ["empty_column"]);
  assert.deepEqual(actual.rows, []);
});

test("여러 결과 집합은 각각의 열과 행을 보존한다", async () => {
  // given
  const sql = "SELECT 1 AS first_set; SELECT 2 AS second_set";
  // when
  const actual = await query(plugin, sql);
  // then
  assert.deepEqual(actual.rows, [[1]]);
  assert.deepEqual(actual.additional_results[0].columns, ["second_set"]);
  assert.deepEqual(actual.additional_results[0].rows, [[2]]);
});

test("실제 결과가 만 행을 넘으면 잘림을 명시한다", async () => {
  // given
  const sql = "SELECT TOP (10001) ROW_NUMBER() OVER (ORDER BY a.object_id, b.object_id) AS n FROM sys.all_objects a CROSS JOIN sys.all_objects b";
  // when
  const actual = await query(plugin, sql);
  // then
  assert.equal(actual.rows.length, 10_000);
  assert.equal(actual.truncated, true);
});

test("취소 완료 응답은 해당 서버 실행의 종료와 일치한다", { timeout: 60_000 }, async () => {
  // given
  const tag = `owned_cancel_${randomUUID().replaceAll("-", "")}`;
  const request = blocked(tag);
  let actual;
  let remaining;
  try {
    await awaitActive(tag, 1);
    // when
    plugin.notify("cancel", { id: request.id });
    actual = await request.result;
    remaining = await activeCount(tag);
  } finally {
    plugin.notify("cancel", { id: request.id });
    await Promise.allSettled([request.result]);
  }
  // then
  assert.deepEqual(actual.error, {
    code: -32800, message: "Query cancelled",
    data: { kind: "query_cancelled", request_id: request.id, server_termination_confirmed: true },
  });
  assert.equal(remaining, 0);
});

test("취소한 연결을 처리한 뒤 새 SELECT가 정상 완료된다", async () => {
  // given
  const sql = "SELECT 42 AS healthy";
  // when
  const actual = await query(plugin, sql);
  // then
  assert.deepEqual(actual.rows, [[42]]);
  assert.deepEqual(plugin.unexpected, []);
});

test("네 작업이 실행 중이어도 취소가 통과하고 이웃 쿼리는 유지된다", { timeout: 60_000 }, async () => {
  // given
  const tag = `owned_full_${randomUUID().replaceAll("-", "")}`;
  const requests = Array.from({ length: 4 }, () => blocked(tag));
  let actual;
  let neighbors;
  try {
    await awaitActive(tag, 4);
    // when
    plugin.notify("cancel", { id: requests[0].id });
    actual = await requests[0].result;
    neighbors = await activeCount(tag);
  } finally {
    for (const request of requests) plugin.notify("cancel", { id: request.id });
    await Promise.allSettled(requests.map(request => request.result));
  }
  // then
  assert.equal(actual.error.code, -32800);
  assert.equal(actual.error.data.server_termination_confirmed, true);
  assert.equal(neighbors, 3);
  assert.deepEqual(plugin.unexpected, []);
});
