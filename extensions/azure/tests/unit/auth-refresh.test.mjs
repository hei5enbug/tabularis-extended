import test from "node:test";
import assert from "node:assert/strict";
import { createClientProvider, COSMOS_SCOPE } from "../../dist/connection/index.js";
import { createCosmosRuntime } from "../../dist/runtime/index.js";
import { MAX_JOB_DEADLINE_MS } from "../../dist/runtime/contracts.js";
import { fixtureEndpoint, fixtureSettings, rpcRequest } from "../support/fixture.mjs";
import { queryFixture } from "../support/query-fixture.mjs";
import { runtimeFixture, workRequest, deferred } from "../support/x0b-fixture.mjs";

const NOW = 1000;
const MARGIN = 300_000;
const entraSettings = { ...fixtureSettings, auth_mode: "entra_user", tenant_id: "tenant", client_id: "client" };
const entraParams = { driver: "cosmos-nosql", extra: { endpoint: fixtureEndpoint, auth_mode: "entra_user", tenant_id: "tenant", client_id: "client" } };
const entra = { kind: "entra_token", identity: "principal", tenant_id: "tenant", client_id: "client", principal_id: "oid", access_token: "token-a", expires_at_ms: 2_000_000, scope: COSMOS_SCOPE };

function providerFixture(t, clock = { now: NOW }) {
  const options = [];
  const disposed = [];
  const clients = createClientProvider({ now: () => clock.now, createClient: (value) => { const client = { dispose: () => disposed.push(client) }; options.push(value); return client; } });
  t.after(() => clients.dispose());
  return { clients, options, disposed, clock };
}
function expiryOf(promise) { return promise.then(() => null, (error) => error.code); }
function refreshRequest(overrides = {}) {
  return rpcRequest("service_refresh_auth", {}, { id: "refresh", params: entraParams, auth: entra, ...overrides });
}

test("작업 deadline은 MAX_JOB_DEADLINE_MS까지 허용하고 초과하면 거부한다", async (t) => {
  // given
  const f = runtimeFixture(t);
  const send = (deadline_ms, id) => {
    const request = workRequest("a", id);
    request.params.driver_context.deadline_ms = deadline_ms;
    return f.runtime.dispatch(request);
  };

  // when
  const actual = [await send(120_001, "over-old-cap"), await send(MAX_JOB_DEADLINE_MS, "at-cap"), await send(MAX_JOB_DEADLINE_MS + 1, "over-cap")];

  // then
  assert.equal(MAX_JOB_DEADLINE_MS, 3_600_000);
  assert.equal(actual[0].result.status, "succeeded");
  assert.equal(actual[1].result.status, "succeeded");
  assert.equal(actual[2].error.data.code, "INVALID_ARGUMENT");
});

test("query 핸들러의 deadline도 MAX_JOB_DEADLINE_MS까지 허용하고 초과하면 거부한다", async (t) => {
  // given
  const f = queryFixture(t, { pages: [{ resources: [1], hasMore: false }, { resources: [1], hasMore: false }] });

  // when
  const accepted = await Promise.all([120_001, MAX_JOB_DEADLINE_MS].map((deadline_ms) => f.execute({}, { deadline_ms })));
  const rejected = await f.execute({}, { deadline_ms: MAX_JOB_DEADLINE_MS + 1 }).catch((error) => error);

  // then
  assert.deepEqual(accepted.map((result) => result.data.values), [[1], [1]]);
  assert.equal(rejected.code, "INVALID_ARGUMENT");
});

for (const [label, deadline_ms, window] of [["기본 deadline", undefined, 30_000], ["짧은 deadline", 5_000, 5_000], ["120초 deadline", 120_000, 120_000], ["1시간 deadline", MAX_JOB_DEADLINE_MS, 120_000], ["getToken 확인", 0, 0]]) {
  test(`${label}의 Entra 만료 경계는 호스트의 now + min(deadline, 120초) + 5분 규칙과 같다`, async (t) => {
    // given
    const f = providerFixture(t);
    const boundary = NOW + window + MARGIN;
    const context = (expires_at_ms, connection_id) => ({ connection_id, settings: entraSettings, auth: { ...entra, expires_at_ms }, ...(deadline_ms === undefined ? {} : { deadline_ms }) });

    // when
    const below = await expiryOf(f.clients.get(context(boundary - 1, "below")));
    const exact = await expiryOf(f.clients.get(context(boundary, "exact")));

    // then
    assert.equal(below, "AUTH_EXPIRED");
    assert.equal(exact, null);
    assert.equal(f.options.length, 1);
  });
}

test("SDK getToken은 같은 만료 규칙으로 토큰을 다시 확인한다", async (t) => {
  // given
  const f = providerFixture(t);
  await f.clients.get({ connection_id: "a", settings: entraSettings, auth: { ...entra, expires_at_ms: NOW + 30_000 + MARGIN }, deadline_ms: 30_000 });
  const getToken = () => f.options[0].aadCredentials.getToken(COSMOS_SCOPE);
  f.clock.now = NOW + 30_000;
  const atBoundary = await getToken();
  f.clock.now = NOW + 30_001;

  // when
  const pastBoundary = await expiryOf(getToken());

  // then
  assert.equal(atBoundary.token, "token-a");
  assert.equal(pastBoundary, "AUTH_EXPIRED");
});

test("refresh는 캐시된 클라이언트의 다음 getToken에 새 토큰을 제공하고 클라이언트를 재사용한다", async (t) => {
  // given
  const f = providerFixture(t);
  const client = await f.clients.get({ connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 });
  const before = await f.options[0].aadCredentials.getToken(COSMOS_SCOPE);

  // when
  const refreshed = await f.clients.refresh({ connection_id: "a", settings: entraSettings, auth: { ...entra, access_token: "token-b", expires_at_ms: 3_000_000 }, deadline_ms: 1000 });

  // then
  const after = await f.options[0].aadCredentials.getToken(COSMOS_SCOPE);
  assert.equal(refreshed, true);
  assert.deepEqual(before, { token: "token-a", expiresOnTimestamp: 2_000_000 });
  assert.deepEqual(after, { token: "token-b", expiresOnTimestamp: 3_000_000 });
  assert.equal(f.options.length, 1);
  assert.deepEqual(f.disposed, []);
  assert.equal(await f.clients.get({ connection_id: "a", settings: entraSettings, auth: { ...entra, access_token: "token-b", expires_at_ms: 3_000_000 }, deadline_ms: 1000 }), client);
});

test("refresh는 더 일찍 만료되는 토큰으로 저장된 토큰을 바꾸지 않는다", async (t) => {
  // given
  const f = providerFixture(t);
  await f.clients.get({ connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 });

  // when
  const refreshed = await f.clients.refresh({ connection_id: "a", settings: entraSettings, auth: { ...entra, access_token: "older", expires_at_ms: 1_500_000 }, deadline_ms: 1000 });

  // then
  assert.equal(refreshed, false);
  assert.equal((await f.options[0].aadCredentials.getToken(COSMOS_SCOPE)).token, "token-a");
});

test("refresh는 클라이언트가 없거나 인증 주체가 다르면 만들거나 교체하지 않고 false를 반환한다", async (t) => {
  // given
  const f = providerFixture(t);
  const refresh = (auth, connection_id = "a") => f.clients.refresh({ connection_id, settings: entraSettings, auth, deadline_ms: 1000 });
  const absent = await refresh({ ...entra, access_token: "token-b", expires_at_ms: 3_000_000 });
  await f.clients.get({ connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 });

  // when
  const otherIdentity = await refresh({ ...entra, identity: "other-principal", access_token: "token-b", expires_at_ms: 3_000_000 });
  const otherPrincipal = await refresh({ ...entra, principal_id: "other-oid", access_token: "token-c", expires_at_ms: 3_000_000 });
  const otherConnection = await refresh({ ...entra, access_token: "token-d", expires_at_ms: 3_000_000 }, "b");

  // then
  assert.deepEqual([absent, otherIdentity, otherPrincipal, otherConnection], [false, false, false, false]);
  assert.equal(f.options.length, 1);
  assert.deepEqual(f.disposed, []);
  assert.equal((await f.options[0].aadCredentials.getToken(COSMOS_SCOPE)).token, "token-a");
});

test("refresh는 get과 같은 인증 검증 오류로 잘못된 컨텍스트를 거부한다", async (t) => {
  // given
  const f = providerFixture(t);
  await f.clients.get({ connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 });
  const refresh = (auth, settings = entraSettings, deadline_ms = 1000) => expiryOf(f.clients.refresh({ connection_id: "a", settings, auth, deadline_ms }));

  // when
  const actual = [
    await refresh({ ...entra, access_token: "token-b", tenant_id: "other-tenant" }),
    await refresh({ ...entra, access_token: "token-b", scope: "https://other/.default" }),
    await refresh({ ...entra, access_token: "token-b", expires_at_ms: NOW + 1000 + MARGIN - 1 }),
    await refresh({ kind: "account_key", account_key: "key", identity: "principal" }),
    await refresh({ ...entra, access_token: "token-b" }, { ...entraSettings, endpoint: "http://fixture-account.documents.azure.com" }),
  ];

  // then
  assert.deepEqual(actual, ["AUTH_REQUIRED", "AUTH_REQUIRED", "AUTH_EXPIRED", "AUTH_REQUIRED", "INVALID_ARGUMENT"]);
  assert.equal((await f.options[0].aadCredentials.getToken(COSMOS_SCOPE)).token, "token-a");
});

test("service_refresh_auth는 진행 중인 요청을 중단하지 않고 다음 getToken의 토큰을 교체한다", async (t) => {
  // given
  const started = deferred();
  const release = deferred();
  const options = [];
  const signals = [];
  let finished = false;
  const clients = createClientProvider({ now: () => NOW, createClient: (value) => {
    options.push(value);
    return { dispose() {}, databases: { readAll: (feed) => { signals.push(feed.abortSignal); return { hasMoreResults: () => !finished, async fetchNext() { started.resolve(); await release.promise; finished = true; return { resources: [], requestCharge: 1 }; } }; } } };
  } });
  t.after(() => clients.dispose());
  const runtime = createCosmosRuntime({ clients });
  t.after(() => runtime.close());
  const running = runtime.dispatch(rpcRequest("test_connection", {}, { id: "running", params: entraParams, auth: entra, deadline_ms: 30_000 }));
  await started.promise;

  // when
  const refreshed = await runtime.dispatch(refreshRequest({ auth: { ...entra, access_token: "token-b", expires_at_ms: 3_000_000 } }));
  const token = await options[0].aadCredentials.getToken(COSMOS_SCOPE);
  release.resolve();
  const completed = await running;

  // then
  assert.equal(refreshed.result.status, "succeeded");
  assert.deepEqual(refreshed.result.data, { refreshed: true });
  assert.equal(token.token, "token-b");
  assert.equal(signals[0].aborted, false);
  assert.equal(completed.result.status, "succeeded");
});

test("service_refresh_auth는 클라이언트가 없으면 만들지 않고 refreshed false를 반환한다", async (t) => {
  // given
  const f = providerFixture(t);
  const runtime = createCosmosRuntime({ clients: f.clients });
  t.after(() => runtime.close());

  // when
  const actual = await runtime.dispatch(refreshRequest());

  // then
  assert.deepEqual(actual.result.data, { refreshed: false });
  assert.equal(f.options.length, 0);
});

test("service_refresh_auth는 비어 있지 않은 input이나 세션 핸들을 거부한다", async (t) => {
  // given
  const f = providerFixture(t);
  const runtime = createCosmosRuntime({ clients: f.clients });
  t.after(() => runtime.close());
  const withInput = refreshRequest();
  withInput.params.input = { token: "extra" };
  const withSession = refreshRequest({ context: { session_handle: "handle" } });

  // when
  const actual = [await runtime.dispatch(withInput), await runtime.dispatch(withSession)];

  // then
  assert.deepEqual(actual.map((response) => response.error.data.code), ["INVALID_ARGUMENT", "INVALID_ARGUMENT"]);
});

test("service_refresh_auth는 만료되었거나 맞지 않는 인증을 get과 같은 오류로 거부한다", async (t) => {
  // given
  const f = providerFixture(t);
  const runtime = createCosmosRuntime({ clients: f.clients });
  t.after(() => runtime.close());

  // when
  const actual = [
    await runtime.dispatch(refreshRequest({ auth: { ...entra, expires_at_ms: NOW + 1000 + MARGIN - 1 } })),
    await runtime.dispatch(refreshRequest({ auth: { ...entra, tenant_id: "other-tenant" } })),
  ];

  // then
  assert.deepEqual(actual.map((response) => response.result.error.code), ["AUTH_EXPIRED", "AUTH_REQUIRED"]);
});

test("refresh는 계정 키 인증의 클라이언트에는 토큰을 교체하지 않고 false를 반환한다", async (t) => {
  // given
  const f = providerFixture(t);
  const key = { kind: "account_key", account_key: "key", identity: "principal" };
  await f.clients.get({ connection_id: "a", settings: fixtureSettings, auth: key, deadline_ms: 1000 });

  // when
  const actual = await f.clients.refresh({ connection_id: "a", settings: fixtureSettings, auth: key, deadline_ms: 1000 });

  // then
  assert.equal(actual, false);
});

test("refresh는 작업이 다른 데이터베이스를 쓰더라도 같은 엔드포인트와 인증 주체의 클라이언트에 도달한다", async (t) => {
  // given
  const f = providerFixture(t);
  await f.clients.get({ connection_id: "a", settings: { ...entraSettings, database: "job-database" }, auth: entra, deadline_ms: 1000 });

  // when
  const actual = await f.clients.refresh({ connection_id: "a", settings: entraSettings, auth: { ...entra, access_token: "token-b", expires_at_ms: 3_000_000 }, deadline_ms: 1000 });

  // then
  assert.equal(actual, true);
  assert.equal((await f.options[0].aadCredentials.getToken(COSMOS_SCOPE)).token, "token-b");
});
