import test from 'node:test';
import assert from 'node:assert/strict';
import { resolveLegacyContext, resolveCliAuth } from '../../dist/runtime/compat.js';
import { resolveWireContext } from '../../dist/runtime/wire.js';
import { createRpcRuntime } from '../../dist/runtime/engine.js';
import { queryFixture } from '../support/query-fixture.mjs';

const connection = { driver: 'cosmos-nosql', host: null, password: 'synthetic-account-key', database: 'fixture', extra: { endpoint: 'https://fixture.documents.azure.com', auth_mode: 'account_key', database: 'fixture', container: 'items' } };
const request = (method, id = 1, fields = {}) => ({ jsonrpc: '2.0', id, method, params: { params: structuredClone(connection), ...fields } });

test('0.26 연결은 비밀번호를 메모리 인증으로 받고 읽기 전용 정책을 적용한다', () => {
  // given
  const input = request('execute_query');
  // when
  const context = resolveLegacyContext(input);
  // then
  assert.equal(context.read_only, true);
  assert.equal(context.auth.account_key, connection.password);
  assert.equal(context.connection.container, 'items');
  assert.ok(!context.connection_id.includes(connection.password));
});

test('공통 서비스 문서 쓰기는 내부 인증 문맥 없이 실행하지 않는다', () => {
  // given
  const input = request('create_document', 1, { input: {} });
  // when
  const action = () => resolveWireContext(input);
  // then
  assert.throws(action, { code: 'AUTH_REQUIRED' });
});

test('공식 호스트의 query 요청은 설정된 컨테이너와 상한 초과 표시를 보존한다', async t => {
  // given
  const fixture = queryFixture(t, { pages: [{ resources: [1, 2], hasMore: true, token: 'synthetic-page' }] });
  const input = request('execute_query', 3, { query: 'SELECT VALUE c.value FROM c', schema: null, page: 1, limit: 2 });
  // when
  const response = await fixture.runtime.dispatch(input);
  // then
  assert.deepEqual(response.result.rows, [[1], [2]]);
  assert.equal(response.result.truncated, true);
  assert.equal(response.result.pagination.has_more, false);
  assert.equal(fixture.calls.find(call => call.method === 'container').container, 'items');
  assert.equal(fixture.calls.find(call => call.method === 'query').spec.query, input.params.query);
});

test('0.26 메타데이터 조회는 인증값 없이 읽기 전용 기능을 반환한다', async t => {
  // given
  const fixture = queryFixture(t);
  const input = request('get_connection_metadata');
  delete input.params.params.password;
  // when
  const response = await fixture.runtime.dispatch(input);
  // then
  assert.equal(response.result.capabilities.readonly, true);
  assert.equal(fixture.calls.length, 0);
});

test('Azure CLI 토큰은 Cosmos 범위를 요청하고 Cosmos GUID audience도 허용한다', async () => {
  // given
  const tenant = '00000000-0000-0000-0000-000000000001';
  const client = '04b07795-8ddb-461a-bbee-02f9e1bf7b46';
  const now = () => 1000000;
  const claims = { ver: '2.0', tid: tenant, appid: client, oid: 'synthetic-principal', aud: 'a232010e-820c-4083-83bb-3ace5fc29d0b', exp: 2000 };
  const token = `synthetic.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.signature`;
  const context = resolveLegacyContext({ ...request('test_connection'), params: { params: { ...connection, extra: { ...connection.extra, auth_mode: 'entra_user', auth_source: 'azure_cli', tenant_id: tenant, client_id: client } } } });
  let args;
  // when
  const auth = await resolveCliAuth(context, async value => { args = value; return JSON.stringify({ accessToken: token }); }, now);
  // then
  assert.equal(auth.principal_id, 'synthetic-principal');
  assert.equal(auth.scope, 'https://cosmos.azure.com/.default');
  assert.equal(args[args.indexOf('--tenant') + 1], tenant);
  assert.ok(!args.includes(token));
});

test('Azure CLI가 다른 서비스의 토큰을 반환하면 Cosmos 인증을 거부한다', async () => {
  // given
  const tenant = '00000000-0000-0000-0000-000000000001';
  const claims = { tid: tenant, appid: '04b07795-8ddb-461a-bbee-02f9e1bf7b46', oid: 'synthetic-principal', aud: 'https://database.windows.net', exp: 2000 };
  const token = `synthetic.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.signature`;
  const context = resolveLegacyContext({ ...request('test_connection'), params: { params: { ...connection, extra: { ...connection.extra, auth_mode: 'entra_user', auth_source: 'azure_cli', tenant_id: tenant, client_id: claims.appid } } } });
  // when
  const action = resolveCliAuth(context, async () => JSON.stringify({ accessToken: token }), () => 1000000);
  // then
  await assert.rejects(action, { code: 'AUTH_REQUIRED' });
});

test('호스트 cancel 알림은 요청을 중단하고 별도 응답을 보내지 않는다', async () => {
  // given
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const runtime = createRpcRuntime({ handlers: { execute_query: async (_value, context) => { started(); await new Promise(resolve => context.signal.addEventListener('abort', resolve, { once: true })); throw context.signal.reason; } } });
  const pending = runtime.dispatch(request('execute_query', 10, { query: 'SELECT * FROM c' }));
  await ready;
  // when
  const acknowledgement = await runtime.dispatch({ jsonrpc: '2.0', method: 'cancel', params: { id: 10 } });
  const response = await pending;
  // then
  assert.equal(acknowledgement, undefined);
  assert.equal(response.error.data.code, 'CANCELLED');
});
