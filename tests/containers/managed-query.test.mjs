import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { access, chmod, copyFile, cp, mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test, { after, before } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { GenericContainer, Wait } from "testcontainers";
import { RpcProcess } from "./rpc-process.mjs";

const sqlImage = "mcr.microsoft.com/mssql/server:2022-latest@sha256:4402d880dd4c34bfa7d8705e56a86cd6c88da80a1f6bbbe741f999e76264a090";
const pgImage = "postgis/postgis:17-3.5@sha256:01a6a70e41e6c4467c8f55f6063555ed72db2d6662cd0d571040d42eadaeb6f6";
const suffix = process.platform === "win32" ? ".exe" : "";
const containers = [];
const probes = new Map();
const connections = [];
const secrets = [];
let root;
let profile;
let host;
let hostBinary;
let hostEnv;

function requiredPath(name) {
  const value = process.env[name];
  assert.ok(value && path.isAbsolute(value), `${name} must name an absolute local artifact path`);
  return value;
}

async function rpc(process, method, params) {
  const response = await process.request(method, params).result;
  assert.equal(response.error, undefined, `RPC failed: ${response.error?.code}; ${response.error?.data?.kind ?? "unknown"}`);
  return response.result;
}

async function tool(name, args = {}, process = host) {
  const result = await rpc(process, "tools/call", { name, arguments: args });
  assert.notEqual(result.isError, true);
  return JSON.parse(result.content[0].text);
}

async function startHost(env = hostEnv) {
  const process = new RpcProcess(hostBinary, ["--mcp"], env);
  await rpc(process, "initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "owned-container-test", version: "1" } });
  process.notify("notifications/initialized", {});
  return process;
}

before(async () => {
  hostBinary = requiredPath("TABULARIS_TEST_HOST_BINARY");
  const sqlPackage = requiredPath("TABULARIS_TEST_SQL_PACKAGE_DIR");
  const pgSource = requiredPath("TABULARIS_TEST_POSTGRES_SOURCE_DIR");
  for (const file of [hostBinary, path.join(sqlPackage, `sqlserver-plugin${suffix}`), path.join(pgSource, "target", "debug", `postgresql-plugin${suffix}`)]) await access(file);
  root = await mkdtemp(path.join(os.tmpdir(), "tabularis-extended-managed-"));
  await chmod(root, 0o700);
  profile = path.join(root, "profile");
  const plugins = path.join(root, "plugins");
  await mkdir(profile);
  await mkdir(path.join(plugins, "drivers"), { recursive: true });
  await cp(sqlPackage, path.join(plugins, "drivers", "sqlserver-plus"), { recursive: true, dereference: false });
  const pgPackage = path.join(plugins, "drivers", "postgresql");
  await mkdir(pgPackage);
  await copyFile(path.join(pgSource, ".tabularium"), path.join(pgPackage, ".tabularium"));
  await copyFile(path.join(pgSource, "target", "debug", `postgresql-plugin${suffix}`), path.join(pgPackage, `postgresql-plugin${suffix}`));
  await chmod(path.join(pgPackage, `postgresql-plugin${suffix}`), 0o755);

  for (const driver of ["sqlserver-plus", "postgresql"]) {
    const isSql = driver === "sqlserver-plus";
    const password = `Tc!a1-${randomUUID()}`;
    secrets.push(password);
    const container = await new GenericContainer(isSql ? sqlImage : pgImage)
      .withPlatform("linux/amd64")
      .withEnvironment(isSql ? { ACCEPT_EULA: "Y", MSSQL_PID: "Developer", MSSQL_SA_PASSWORD: password } : { POSTGRES_PASSWORD: password })
      .withExposedPorts(isSql ? 1433 : 5432)
      .withWaitStrategy(isSql ? Wait.forLogMessage("SQL Server is now ready for client connections") : Wait.forLogMessage("database system is ready to accept connections", 2))
      .withStartupTimeout(240_000)
      .start();
    containers.push(container);
    const params = { driver, host: container.getHost(), port: container.getMappedPort(isSql ? 1433 : 5432), database: isSql ? "master" : "postgres", username: isSql ? "sa" : "postgres", password, ssl_mode: isSql ? "require" : "disable", save_in_keychain: false };
    connections.push({ id: driver, name: driver, params });
    const probe = new RpcProcess(path.join(plugins, "drivers", driver, `${isSql ? "sqlserver" : "postgresql"}-plugin${suffix}`));
    probes.set(driver, probe);
    await rpc(probe, "initialize", { settings: { max_pool_size: 8, connect_timeout_seconds: 20 } });
    await rpc(probe, "execute_query", { params, query: "SELECT 1 AS ready" });
  }
  await writeFile(path.join(profile, "connections.json"), JSON.stringify({ connections, groups: [], tags: [] }), { mode: 0o600 });
  await writeFile(path.join(profile, "config.json"), JSON.stringify({ activeExternalDrivers: ["sqlserver-plus", "postgresql"], mcpApprovalMode: "off", mcpReadonlyDefault: true, aiAuditEnabled: true, pluginCallTimeoutSeconds: 0 }), { mode: 0o600 });
  hostEnv = { TABULARIS_EXTENDED_DATA_DIR: profile, TABULARIS_EXTENDED_PLUGIN_DIR: plugins };
  host = await startHost();
}, { timeout: 360_000 });

after(async () => {
  try {
    await Promise.allSettled([host?.stop(), ...[...probes.values()].map(probe => probe.stop())]);
  } finally {
    await Promise.allSettled(containers.map(container => container.stop()));
    if (root) await rm(root, { recursive: true, force: true });
  }
});

async function waitFor(predicate, message, timeout = 15_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await predicate();
    if (value) return value;
    await delay(40);
  }
  throw new Error(message);
}

function terminal(queryId, process = host) {
  return waitFor(async () => {
    const status = await tool("get_query_status", { query_id: queryId }, process);
    return ["succeeded", "failed", "cancelled"].includes(status.state) ? status : null;
  }, "Managed query did not reach a terminal state");
}

async function activeCount(driver, tag) {
  const params = connections.find(connection => connection.id === driver).params;
  const sql = driver === "sqlserver-plus"
    ? `SELECT COUNT(*) FROM sys.dm_exec_requests r CROSS APPLY sys.dm_exec_sql_text(r.sql_handle) t WHERE r.session_id <> @@SPID AND t.text LIKE '%${tag}%'`
    : `SELECT COUNT(*) FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND state = 'active' AND query LIKE '%${tag}%'`;
  const result = await rpc(probes.get(driver), "execute_query", { params, query: sql });
  return Number(result.rows[0][0]);
}

function longSelect(driver, tag) {
  return driver === "sqlserver-plus"
    ? `SELECT /* ${tag} */ SUM(CONVERT(bigint,a.object_id)+b.object_id+c.object_id+d.object_id) FROM sys.all_objects a CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c CROSS JOIN sys.all_objects d`
    : `SELECT /* ${tag} */ pg_sleep(60)`;
}

test("호스트가 기존 다섯 도구와 관리형 여섯 도구를 함께 제공한다", async () => {
  // given
  const expected = ["list_connections", "list_databases", "list_tables", "describe_table", "run_query", "start_query", "get_query_status", "fetch_query_result", "cancel_query", "list_query_history", "release_query_result"];
  // when
  const available = await rpc(host, "tools/list", {});
  // then
  assert.deepEqual(available.tools.map(tool => tool.name).sort(), expected.sort());
  assert.equal(JSON.stringify(await tool("list_connections")).includes("sqlserver-plus"), true);
});

for (const driver of ["sqlserver-plus", "postgresql"]) {
  test(`${driver}: 저장한 페이지는 재실행 없이 중복 열과 임의 값을 보존한다`, async () => {
    // given
    const query = driver === "sqlserver-plus"
      ? "SELECT n AS same, CONVERT(varchar(36),NEWID()) AS same FROM (VALUES(1),(2),(3),(4),(5),(6),(7)) AS v(n) ORDER BY n"
      : "SELECT n AS same, random() AS same FROM generate_series(1,7) n ORDER BY n";
    // when
    const started = await tool("start_query", { connection_id: driver, query, max_rows: 5 });
    const status = await terminal(started.query_id);
    const first = await tool("fetch_query_result", { query_id: started.query_id, limit: 2 });
    const again = await tool("fetch_query_result", { query_id: started.query_id, limit: 2 });
    const middle = await tool("fetch_query_result", { query_id: started.query_id, offset: 2, limit: 2 });
    const last = await tool("fetch_query_result", { query_id: started.query_id, offset: 4, limit: 2 });
    // then
    assert.equal(started.state, "queued");
    assert.equal(status.state, "succeeded");
    assert.deepEqual(first.columns, ["same", "same"]);
    assert.deepEqual(again, first);
    assert.deepEqual([...first.rows, ...middle.rows, ...last.rows].map(row => Number(row[0])), [1, 2, 3, 4, 5]);
    assert.equal(first.stored_row_count, 5);
    assert.equal(first.truncated, true);
    assert.equal(last.has_more, false);
    await tool("release_query_result", { query_id: started.query_id });
    const released = await host.request("tools/call", { name: "fetch_query_result", arguments: { query_id: started.query_id } }).result;
    assert.equal(released.error.data.kind, "RESULT_EXPIRED");
    await tool("release_query_result", { query_id: started.query_id });
  });

  test(`${driver}: 빈 결과 열을 보존하고 기존 JSON과 TOON 실행을 유지한다`, async () => {
    // given
    const query = "SELECT 1 AS empty_column WHERE 1 = 0";
    // when
    const started = await tool("start_query", { connection_id: driver, query });
    assert.equal((await terminal(started.query_id)).state, "succeeded");
    const page = await tool("fetch_query_result", { query_id: started.query_id });
    const legacy = await tool("run_query", { connection_id: driver, query: "SELECT 42 AS healthy" });
    const toon = await rpc(host, "tools/call", { name: "run_query", arguments: { connection_id: driver, query: "SELECT 42 AS healthy", output_format: "toon" } });
    // then
    assert.deepEqual(page.columns, ["empty_column"]);
    assert.deepEqual(page.rows, []);
    assert.equal(JSON.stringify(legacy).includes("42"), true);
    assert.equal(toon.content[0].text.includes("healthy"), true);
    await tool("release_query_result", { query_id: started.query_id });
  });

  test(`${driver}: 포화 상태의 대기 취소와 실제 실행 취소가 이웃 쿼리를 보존한다`, { timeout: 60_000 }, async () => {
    // given
    const tag = `owned_managed_${randomUUID().replaceAll("-", "")}`;
    const ids = [];
    try {
      for (let index = 0; index < 2; index++) ids.push((await tool("start_query", { connection_id: driver, query: longSelect(driver, tag) })).query_id);
      await waitFor(async () => (await activeCount(driver, tag)) === 2, "Owned server queries did not start");
      ids.push((await tool("start_query", { connection_id: driver, query: "SELECT 7 AS queued" })).query_id);
      // when
      const responseStart = Date.now();
      await tool("cancel_query", { query_id: ids[2] });
      assert.equal((await terminal(ids[2])).state, "cancelled");
      await tool("cancel_query", { query_id: ids[0] });
      const cancelled = await terminal(ids[0]);
      const neighbor = await tool("get_query_status", { query_id: ids[1] });
      // then
      assert.equal(cancelled.state, "cancelled");
      assert.equal(neighbor.state, "running");
      assert.equal(await activeCount(driver, tag), 1);
      assert.ok(Date.now() - responseStart < 5000, "Control requests were blocked by long-running queries");
    } finally {
      await Promise.allSettled(ids.map(query_id => tool("cancel_query", { query_id })));
      await Promise.allSettled(ids.map(query_id => terminal(query_id)));
    }
    assert.equal(await activeCount(driver, tag), 0);
  });

  test(`${driver}: 실행 기한 만료는 실제 서버 실행을 종료한다`, { timeout: 30_000 }, async () => {
    // given
    const tag = `owned_deadline_${randomUUID().replaceAll("-", "")}`;
    // when
    const started = await tool("start_query", { connection_id: driver, query: longSelect(driver, tag), timeout_ms: 1000 });
    const status = await terminal(started.query_id);
    // then
    assert.equal(status.state, "cancelled");
    assert.equal(await activeCount(driver, tag), 0);
  });
}

test("PostgreSQL의 기존 실행 두 개와 관리형 실행 한 개가 취소 작업자를 막지 않는다", { timeout: 30_000 }, async () => {
  // given
  const tag = `owned_shared_${randomUUID().replaceAll("-", "")}`;
  const legacy = Array.from({ length: 2 }, () => host.request("tools/call", {
    name: "run_query", arguments: { connection_id: "postgresql", query: `SELECT /* ${tag} */ pg_sleep(8)` },
  }).result);
  let queryId;
  try {
    queryId = (await tool("start_query", { connection_id: "postgresql", query: longSelect("postgresql", tag) })).query_id;
    await waitFor(async () => (await activeCount("postgresql", tag)) === 3, "Shared ordinary calls did not reach the server");
    // when
    const beforeCancel = Date.now();
    await tool("cancel_query", { query_id: queryId });
    const status = await terminal(queryId);
    // then
    assert.equal(status.state, "cancelled");
    assert.ok(Date.now() - beforeCancel < 5000);
    assert.equal(await activeCount("postgresql", tag), 2);
    for (const response of await Promise.all(legacy)) assert.equal(response.error, undefined);
  } finally {
    if (queryId) await tool("cancel_query", { query_id: queryId }).catch(() => {});
    await Promise.allSettled(legacy);
  }
  assert.equal(await activeCount("postgresql", tag), 0);
});

test("관리형 실행은 쓰기와 여러 문장을 거부한다", async () => {
  // given
  const queries = ["DELETE FROM forbidden_fixture", "SELECT 1; SELECT 2", "SELECT 1 AS n INTO forbidden_fixture"];
  // when
  const responses = await Promise.all(queries.map(query => host.request("tools/call", { name: "start_query", arguments: { connection_id: "sqlserver-plus", query } }).result));
  // then
  for (const response of responses) assert.equal(response.error.data.kind, "INVALID_ARGUMENT");
});

test("호스트 재시작은 이력을 유지하되 이전 결과와 제어 권한을 되살리지 않는다", async () => {
  // given
  const started = await tool("start_query", { connection_id: "postgresql", query: "SELECT 81 AS retained" });
  assert.equal((await terminal(started.query_id)).state, "succeeded");
  // when
  await host.stop();
  host = await startHost();
  const history = await tool("list_query_history", { limit: 200 });
  const retained = history.items.find(item => item.query_id === started.query_id);
  // then
  assert.ok(retained);
  assert.equal(retained.result_available, false);
  assert.equal(retained.cancellable, false);
  const audit = await readFile(path.join(profile, "ai_activity.jsonl"), "utf8");
  for (const secret of secrets) {
    assert.equal(audit.includes(secret), false);
    assert.equal(host.stderr.includes(secret), false);
  }
  assert.deepEqual(host.unexpected, []);
});

test("드라이버 접근 설정이나 연결 대상이 바뀌면 이전 결과를 제공하지 않는다", async () => {
  // given
  const configPath = path.join(profile, "config.json");
  const connectionPath = path.join(profile, "connections.json");
  const originalConfig = await readFile(configPath);
  const originalConnections = await readFile(connectionPath);
  const started = await tool("start_query", { connection_id: "postgresql", query: "SELECT 82 AS retained" });
  assert.equal((await terminal(started.query_id)).state, "succeeded");
  try {
    // when
    await writeFile(configPath, JSON.stringify({ ...JSON.parse(originalConfig), activeExternalDrivers: ["sqlserver-plus"] }));
    const disabled = await host.request("tools/call", { name: "fetch_query_result", arguments: { query_id: started.query_id } }).result;
    await writeFile(configPath, originalConfig);
    const edited = JSON.parse(originalConnections);
    edited.connections.find(connection => connection.id === "postgresql").params.database = "changed_fixture_target";
    await writeFile(connectionPath, JSON.stringify(edited));
    const changed = await host.request("tools/call", { name: "get_query_status", arguments: { query_id: started.query_id } }).result;
    // then
    assert.equal(disabled.error.data.kind, "CONNECTION_ACCESS_CHANGED");
    assert.equal(changed.error.data.kind, "CONNECTION_ACCESS_CHANGED");
  } finally {
    await writeFile(configPath, originalConfig);
    await writeFile(connectionPath, originalConnections);
  }
  await tool("release_query_result", { query_id: started.query_id });
});

test("감사 기록을 끄면 초기화와 쿼리가 이력이나 세션 파일을 쓰지 않는다", async () => {
  // given
  const disabledProfile = path.join(root, "audit-disabled-profile");
  await mkdir(disabledProfile, { mode: 0o700 });
  await writeFile(path.join(disabledProfile, "connections.json"), JSON.stringify({ connections, groups: [], tags: [] }), { mode: 0o600 });
  await writeFile(path.join(disabledProfile, "config.json"), JSON.stringify({ activeExternalDrivers: ["postgresql"], mcpApprovalMode: "off", aiAuditEnabled: false }), { mode: 0o600 });
  const process = await startHost({ ...hostEnv, TABULARIS_EXTENDED_DATA_DIR: disabledProfile });
  try {
    // when
    await tool("run_query", { connection_id: "postgresql", query: "SELECT 83 AS legacy" }, process);
    const started = await tool("start_query", { connection_id: "postgresql", query: "SELECT 84 AS managed" }, process);
    assert.equal((await terminal(started.query_id, process)).state, "succeeded");
    const history = await tool("list_query_history", {}, process);
    const files = await readdir(disabledProfile);
    // then
    assert.ok(history.items.some(item => item.query_id === started.query_id));
    assert.equal(files.some(name => name.includes("ai_activity") || name.includes("mcp_session")), false);
    for (const secret of secrets) assert.equal(process.stderr.includes(secret), false);
  } finally {
    await process.stop();
  }
});

async function withApprovalPolicy(run) {
  const configPath = path.join(profile, "config.json");
  const original = await readFile(configPath);
  const heartbeat = path.join(profile, "tabularis.alive");
  const config = { ...JSON.parse(original), mcpApprovalMode: "all", mcpPreflightExplain: false };
  try {
    await writeFile(heartbeat, JSON.stringify({ pid: process.pid, updatedAt: new Date().toISOString() }), { mode: 0o600 });
    await writeFile(configPath, JSON.stringify(config));
    await run();
  } finally {
    await writeFile(configPath, original);
    await rm(heartbeat, { force: true });
  }
}

async function pendingApproval(query) {
  const directory = path.join(profile, "pending_approvals");
  return waitFor(async () => {
    const files = await readdir(directory).catch(error => {
      if (error.code === "ENOENT") return [];
      throw error;
    });
    for (const file of files.filter(file => file.endsWith(".pending.json"))) {
      const pending = JSON.parse(await readFile(path.join(directory, file), "utf8"));
      if (pending.query === query) return pending;
    }
    return null;
  }, "Owned approval request was not created");
}

async function approve(pending, editedQuery) {
  const file = path.join(profile, "pending_approvals", `${pending.id}.decision.json`);
  const temporary = `${file}.tmp`;
  await writeFile(temporary, JSON.stringify({ approvalId: pending.id, decidedAt: new Date().toISOString(), decision: "approve", reason: null, editedQuery }), { mode: 0o600 });
  await rename(temporary, file);
}

async function assertApprovalCleaned(pending) {
  const files = await readdir(path.join(profile, "pending_approvals"));
  assert.equal(files.some(file => file.startsWith(`${pending.id}.`)), false);
}

test("승인 대기에도 실행 기한을 적용하고 요청 파일을 정리한다", async () => {
  // given
  await withApprovalPolicy(async () => {
    const query = "SELECT 85 AS approval_deadline";
    // when
    const started = await tool("start_query", { connection_id: "postgresql", query, timeout_ms: 1000 });
    const pending = await pendingApproval(query);
    const status = await terminal(started.query_id);
    // then
    assert.equal(status.state, "failed");
    assert.equal(status.error.code, "QUERY_TIMEOUT");
    await assertApprovalCleaned(pending);
  });
});

test("승인된 수정 SQL도 SELECT 제한을 다시 검사한다", async () => {
  // given
  await withApprovalPolicy(async () => {
    const query = "SELECT 86 AS approval_edit";
    const started = await tool("start_query", { connection_id: "postgresql", query });
    const pending = await pendingApproval(query);
    // when
    await approve(pending, "SELECT 1; SELECT 2");
    const status = await terminal(started.query_id);
    // then
    assert.equal(status.state, "failed");
    assert.equal(status.error.code, "INVALID_QUERY");
    await assertApprovalCleaned(pending);
  });
});

test("대기열에서 승인 정책이 바뀌면 실행 전에 새 승인을 받는다", { timeout: 30_000 }, async () => {
  // given
  const tag = `owned_policy_${randomUUID().replaceAll("-", "")}`;
  const ids = [];
  try {
    for (let index = 0; index < 2; index++) ids.push((await tool("start_query", { connection_id: "postgresql", query: longSelect("postgresql", tag) })).query_id);
    await waitFor(async () => (await activeCount("postgresql", tag)) === 2, "Owned policy fixtures did not start");
    const query = "SELECT 87 AS policy_changed";
    const queued = await tool("start_query", { connection_id: "postgresql", query });
    ids.push(queued.query_id);
    assert.equal((await tool("get_query_status", { query_id: queued.query_id })).state, "queued");
    // when
    await withApprovalPolicy(async () => {
      await tool("cancel_query", { query_id: ids[0] });
      assert.equal((await terminal(ids[0])).state, "cancelled");
      const pending = await pendingApproval(query);
      assert.equal((await tool("get_query_status", { query_id: queued.query_id })).state, "queued");
      await approve(pending, "SELECT 88 AS approved_value");
      const status = await terminal(queued.query_id);
      const page = await tool("fetch_query_result", { query_id: queued.query_id });
      // then
      assert.equal(status.state, "succeeded");
      assert.deepEqual(page.columns, ["approved_value"]);
      assert.equal(Number(page.rows[0][0]), 88);
      await assertApprovalCleaned(pending);
    });
  } finally {
    await Promise.allSettled(ids.map(query_id => tool("cancel_query", { query_id })));
    await Promise.allSettled(ids.map(query_id => terminal(query_id)));
  }
  assert.equal(await activeCount("postgresql", tag), 0);
});
