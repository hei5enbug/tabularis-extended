import test from "node:test";
import assert from "node:assert/strict";
import { createClientProvider, normalizeConnectionSettings, COSMOS_SCOPE } from "../../dist/connection/index.js";
import { createConnectionHandlers, legacyConnectionMetadata } from "../../dist/connection/handlers.js";
import { getContainerMetadata } from "../../dist/connection/metadata.js";
import { createFixture, fixtureEndpoint, fixtureAuth, fixtureSettings, rpcRequest } from "../support/fixture.mjs";

function providerFixture(t) {
  const options = [];
  const disposed = [];
  const clients = createClientProvider({ now: () => 1000, createClient: (value) => { const client = { dispose: () => disposed.push(client) }; options.push(value); return client; } });
  t.after(() => clients.dispose());
  return { clients, options, disposed };
}
const entraSettings = { ...fixtureSettings, auth_mode: "entra_user", tenant_id: "tenant", client_id: "client" };
const entra = { kind: "entra_token", identity: "principal", tenant_id: "tenant", client_id: "client", principal_id: "oid", access_token: "synthetic-token-a", expires_at_ms: 900000, scope: COSMOS_SCOPE };

test("연결 설정은 기존 host extra에서 공개 필드만 정규화한다", () => {
  // given
  const params = { driver: "cosmos-nosql", database: ["fixture", "other"], extra: { endpoint: fixtureEndpoint + "/", auth_mode: "entra_user", tenant_id: "tenant", client_id: "client", credential_ref: "credential-id" } };

  // when
  const actual = normalizeConnectionSettings(params);

  // then
  assert.deepEqual(actual, { endpoint: fixtureEndpoint, database: "fixture", auth_mode: "entra_user", tenant_id: "tenant", client_id: "client", credential_ref: "credential-id" });
});

for (const [label, endpoint, code] of [["HTTP", "http://fixture.documents.azure.com", "INVALID_ARGUMENT"], ["외부 HTTPS", "https://example.com", "INVALID_ARGUMENT"], ["사용자 정보", "https://user:secret@fixture.documents.azure.com", "INVALID_ARGUMENT"], ["MongoDB", "mongodb://fixture.mongo.cosmos.azure.com", "UNSUPPORTED_OPERATION"], ["사설 cloud", "https://fixture.documents.azure.cn", "INVALID_ARGUMENT"]]) {
  test(`${label} endpoint는 공개 NoSQL 연결로 허용하지 않는다`, () => {
    // given
    const settings = { endpoint, auth_mode: "account_key" };

    // when
    const actual = (() => { try { normalizeConnectionSettings(settings); return null; } catch (error) { return error; } })();

    // then
    assert.equal(actual.code, code);
    assert.equal(actual.message.includes("secret"), false);
  });
}

test("클라이언트 캐시는 연결과 endpoint 및 인증 주체마다 분리한다", async (t) => {
  // given
  const f = providerFixture(t);
  const a = { connection_id: "a", settings: fixtureSettings, auth: fixtureAuth };
  const initial = await f.clients.get(a);
  const other = await f.clients.get({ ...a, connection_id: "b" });

  // when
  const actual = await f.clients.get({ ...a, auth: { ...fixtureAuth, identity: "different-principal" } });

  // then
  assert.notEqual(actual, initial);
  assert.notEqual(actual, other);
  assert.deepEqual(f.disposed, [initial]);
  assert.equal(f.options.length, 3);
  assert.equal(f.options[0].connectionPolicy.enableEndpointDiscovery, false);
  assert.equal(f.options[0].connectionPolicy.enableBackgroundEndpointRefreshing, false);
  assert.equal(f.options[0].connectionPolicy.enablePartitionLevelFailover, false);
  assert.equal(f.options[0].connectionPolicy.enablePartitionLevelCircuitBreaker, false);
  assert.equal(Object.hasOwn(f.options[0], "agent"), false);
});

test("동일한 연결의 유효 인증 갱신은 기존 클라이언트를 재사용한다", async (t) => {
  // given
  const f = providerFixture(t);
  const context = { connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 };
  const initial = await f.clients.get(context);
  await f.clients.get({ ...context, auth: { ...entra, access_token: "synthetic-token-b", expires_at_ms: 1000000 } });

  // when
  const actual = await f.options[0].aadCredentials.getToken(COSMOS_SCOPE);

  // then
  assert.deepEqual(actual, { token: "synthetic-token-b", expiresOnTimestamp: 1000000 });
  assert.equal(f.options[0].aadScope, COSMOS_SCOPE);
  assert.equal(f.options.length, 1);
  assert.equal(f.disposed.includes(initial), false);
});

test("무효화된 이전 인증 주체는 다른 주체의 새 토큰을 읽지 못한다", async (t) => {
  // given
  const f = providerFixture(t);
  const context = { connection_id: "a", settings: entraSettings, auth: entra, deadline_ms: 1000 };
  await f.clients.get(context);
  const previousCredential = f.options[0].aadCredentials;
  await f.clients.get({ ...context, auth: { ...entra, principal_id: "new-oid", identity: "new-principal", access_token: "new-principal-secret" } });

  // when
  const actual = await previousCredential.getToken(COSMOS_SCOPE).catch((error) => error);

  // then
  assert.equal(actual.code, "AUTH_REQUIRED");
  assert.equal(actual.message.includes("new-principal-secret"), false);
});

test("deadline과 갱신 여유를 충족하지 못하는 Entra 토큰은 SDK 생성 전에 거부한다", async (t) => {
  // given
  const f = providerFixture(t);

  // when
  const actual = await f.clients.get({ connection_id: "a", settings: entraSettings, auth: { ...entra, expires_at_ms: 331000 }, deadline_ms: 30000 }).catch((error) => error);

  // then
  assert.equal(actual.code, "AUTH_EXPIRED");
  assert.equal(f.options.length, 0);
});

test("데이터베이스와 계층형 container RID 및 partition path를 읽는다", async (t) => {
  // given
  const f = await createFixture(t, { partitionPaths: ["/tenant", "/region"] });

  // when
  const actual = await getContainerMetadata(f.clients, f.context(), "fixture", "items");

  // then
  assert.deepEqual(actual.metadata, { database: "fixture", container: "items", database_rid: "db-rid", container_rid: "container-rid", partition_key_paths: ["/tenant", "/region"], partition_key_kind: "MultiHash", partition_key_version: 2, system_key: false });
  assert.equal(f.sdk.calls.filter((call) => ["database", "container"].includes(call.operation)).every((call) => call.options.abortSignal instanceof AbortSignal), true);
});

test("기존 metadata 응답은 호스트가 허용한 필드만 제공한다", async (t) => {
  // given
  const f = await createFixture(t);
  const handlers = createConnectionHandlers(f.clients);

  // when
  const actual = await handlers.get_connection_metadata({ params: {} }, f.context());

  // then
  assert.deepEqual(Object.keys(actual).sort(), ["capabilities", "data_types", "type_mappings"]);
  assert.deepEqual(actual, legacyConnectionMetadata);
  assert.equal(Object.hasOwn(actual.capabilities, "documents_v1"), false);
});

test("초기화는 구현된 서비스 기능만 협상한다", async (t) => {
  // given
  const f = await createFixture(t);

  // when
  const actual = await f.runtime.dispatch({ jsonrpc: "2.0", id: "init", method: "initialize", params: { settings: {} } });

  // then
  assert.deepEqual(actual.result.service_capabilities, { protocol_version: 1, documents_v1: true, query_page_v1: false, cancel_v1: true, sessions_v1: false });
});

test("공개 operation input의 권한 주장은 엄격한 스키마로 거부한다", async (t) => {
  // given
  const f = await createFixture(t);
  const request = rpcRequest("read_document", { database: "fixture", container: "items", identity: { id: "1", partition_key: [{ type: "string", value: "a" }] }, permissions: ["write"] });

  // when
  const actual = await f.runtime.dispatch(request);

  // then
  assert.equal(actual.result.error.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.length, 0);
});

test("initialize의 잘못된 endpoint는 연결 기본값으로 승인하지 않는다", async (t) => {
  // given
  const f = await createFixture(t);

  // when
  const actual = await f.runtime.dispatch({ jsonrpc: "2.0", id: "init", method: "initialize", params: { settings: { endpoint: "http://fixture.documents.azure.com", auth_mode: "account_key" } } });

  // then
  assert.equal(actual.error.data.code, "INVALID_ARGUMENT");
  assert.equal(f.sdk.calls.length, 0);
});


test("Azure CLI 소스는 공식 사용자 토큰을 기존 Cosmos SDK에 전달한다", async (t) => {
  // given
  const cli = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";
  const settings = normalizeConnectionSettings({ ...entraSettings, client_id: cli, auth_source: "azure_cli" });
  const f = providerFixture(t);
  const context = { connection_id: "cli", settings, auth: { ...entra, client_id: cli } };
  // when
  await f.clients.get(context);
  const token = await f.options[0].aadCredentials.getToken(COSMOS_SCOPE);
  // then
  assert.equal(settings.auth_source, "azure_cli");
  assert.equal(token.token, "synthetic-token-a");
  assert.equal(Object.hasOwn(f.options[0], "key"), false);
});

for (const [name, settings] of [["다른 클라이언트", { ...entraSettings, auth_source: "azure_cli" }], ["계정 키", { ...fixtureSettings, auth_source: "azure_cli" }], ["알 수 없는 소스", { ...entraSettings, auth_source: "unknown" }]]) {
  test(`${name} 설정으로 Azure CLI 인증을 선택할 수 없다`, () => {
    // given
    const input = settings;
    // when
    const error = (() => { try { normalizeConnectionSettings(input); } catch (error) { return error; } })();
    // then
    assert.equal(error.code, "INVALID_ARGUMENT");
  });
}
