import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { NativeRpc, wire, safeFailure } from '../../scripts/live/rpc.mjs';
import { CONFIG, SECRET, auth, childFixture } from './synthetic-support.mjs';

async function requestAndClose(client, method, params, options) {
  try { return { result: await client.request(method, params, options), error: null }; }
  catch (error) { return { result: null, error: { code: error.code, outcome: error.outcome } }; }
  finally { await client.close(); }
}
function rpc(handler, options = {}) {
  const child = childFixture(handler); let launch;
  const client = new NativeRpc({ closeTimeout: 10, ...options, spawnChild: (executable, args, configuration) => { launch = { executable, args, configuration }; return child; } });
  return { child, client, launch };
}

test('자식 실행은 고정 Node와 private 환경만 전달하고 실제 네트워크를 쓰지 않는다', async () => {
  // given
  const value = rpc(() => ({ result: null })); const profile = value.client.profile;
  // when
  const result = await requestAndClose(value.client, 'initialize', { settings: {}, service_protocol: 1 });
  // then
  assert.equal(result.error, null);
  assert.equal(value.launch.executable, process.execPath);
  assert.equal(value.launch.configuration.shell, false);
  assert.deepEqual(Object.keys(value.launch.configuration.env).sort(), ['HOME', 'PATH', 'TMPDIR', 'USERPROFILE']);
  assert.equal(value.launch.configuration.env.PATH, '');
  assert.equal(value.launch.configuration.env.HOME, profile);
  assert.equal(fs.existsSync(profile), false);
});

test('v1 wire는 connection과 transient context와 입력을 명시적으로 바인딩한다', () => {
  // given
  const credential = auth().owner; const input = { database: CONFIG.database, container: CONFIG.container };
  // when
  const value = wire(CONFIG, credential, 'explicit-connection', input, { readOnly: false });
  // then
  assert.deepEqual(Object.keys(value).sort(), ['driver_context', 'input', 'params']);
  assert.equal(value.params.driver, 'cosmos-nosql');
  assert.equal(value.driver_context.connection_id, 'explicit-connection');
  assert.equal(value.driver_context.auth, credential);
  assert.equal(value.driver_context.read_only, false);
  assert.equal(value.driver_context.deadline_ms, 120000);
});

test('invalidate wire는 secret과 저장된 설정 없이 최소 연결 ID만 전달한다', () => {
  // given
  const credential = auth().owner;
  // when
  const value = wire(CONFIG, credential, 'explicit-connection', {}, { invalidate: true });
  // then
  assert.deepEqual(value.params, { driver: 'cosmos-nosql', connection_id: 'explicit-connection' });
  assert.equal(Object.hasOwn(value.driver_context, 'auth'), false);
  assert.ok(!JSON.stringify(value).includes(SECRET));
});

test('16MiB 초과 요청은 자식 stdin에 쓰기 전에 거부한다', async () => {
  // given
  const value = rpc(() => ({ result: null })); const params = { value: 'x'.repeat(16 * 1024 * 1024) };
  // when
  const result = await requestAndClose(value.client, 'initialize', params);
  // then
  assert.equal(result.error.code, 'RESOURCE_LIMIT');
  assert.equal(value.child.frames.filter(frame => frame.method === 'initialize').length, 0);
});

async function flood(value) {
  const pending = Array.from({ length: 64 }, () => value.client.request('initialize', {}).catch(error => error.code));
  let code;
  try { await value.client.request('initialize', {}); } catch (error) { code = error.code; }
  await value.client.close(); const result = await Promise.all(pending);
  return { code, result };
}
test('64개 in-flight 상한 이후에는 실행 전에 거부하고 종료 시 모두 해제한다', async () => {
  // given
  const value = rpc(frame => frame.method === 'shutdown' ? { result: null } : undefined);
  // when
  const result = await flood(value);
  // then
  assert.equal(result.code, 'RESOURCE_LIMIT');
  assert.equal(result.result.length, 64);
  assert.equal(value.child.frames.filter(frame => frame.method === 'initialize').length, 64);
  assert.equal(value.client.pending.size, 0);
});

test('쓰기 timeout은 cancel 신호를 보내고 unknown 결과를 재전송하지 않는다', async () => {
  // given
  const value = rpc(frame => frame.method === 'shutdown' ? { result: null } : undefined, { requestTimeout: 5 });
  const params = wire(CONFIG, auth().owner, 'connection', { document: {} }, { readOnly: false });
  // when
  const result = await requestAndClose(value.client, 'create_document', params, { write: true });
  // then
  assert.equal(result.error.code, 'DEADLINE_EXCEEDED');
  assert.equal(result.error.outcome, 'unknown');
  assert.equal(value.child.frames.filter(frame => frame.method === 'create_document').length, 1);
  assert.equal(value.child.frames.filter(frame => frame.method === 'cancel_request').length, 1);
});

test('pre-abort 요청은 자식에 dispatch하지 않는다', async () => {
  // given
  const value = rpc(() => ({ result: null })); const controller = new AbortController(); controller.abort();
  // when
  const result = await requestAndClose(value.client, 'create_document', {}, { signal: controller.signal, write: true });
  // then
  assert.equal(result.error.code, 'CANCELLED');
  assert.equal(result.error.outcome, 'not_applied');
  assert.equal(value.child.frames.filter(frame => frame.method === 'create_document').length, 0);
});

for (const [title, emit, expected] of [
  ['잘못된 UTF-8', child => child.stdout.write(Buffer.from([0xff, 10])), 'INVALID_RPC_FRAME'],
  ['비 JSON 로그', child => child.stdout.write(`${SECRET}\n`), 'INVALID_RPC_FRAME'],
  ['과대 stdout frame', child => child.stdout.write(Buffer.alloc(16 * 1024 * 1024 + 1)), 'RESOURCE_LIMIT'],
  ['과대 stderr 로그', child => child.stderr.write(Buffer.alloc(8 * 1024 * 1024 + 1)), 'RESOURCE_LIMIT'],
  ['자식 종료', child => child.emit('close', 1, null), 'DRIVER_EXITED'],
]) {
  test(`${title}는 raw 값을 반환하지 않고 pending 쓰기를 unknown으로 끝낸다`, async () => {
    // given
    const value = rpc((frame, child) => { if (frame.method === 'create_document') queueMicrotask(() => emit(child)); });
    // when
    const result = await requestAndClose(value.client, 'create_document', wire(CONFIG, auth().owner, 'connection', {}), { write: true });
    // then
    assert.equal(result.error.code, expected);
    assert.equal(result.error.outcome, 'unknown');
    assert.ok(!JSON.stringify(result).includes(SECRET));
    assert.equal(value.child.frames.filter(frame => frame.method === 'create_document').length, 1);
    assert.equal(fs.existsSync(value.client.profile), false);
  });
}

test('임의 RPC method는 driver에 전달하지 않는다', async () => {
  // given
  const value = rpc(() => ({ result: null }));
  // when
  const result = await requestAndClose(value.client, 'ping', {});
  // then
  assert.equal(result.error.code, 'UNSUPPORTED_OPERATION');
  assert.equal(value.child.frames.filter(frame => frame.method === 'ping').length, 0);
});

test('서버 오류의 임의 문자열과 details는 고정 코드로 축소한다', () => {
  // given
  const server = { code: SECRET, message: SECRET, details: { token: SECRET }, outcome: 'unknown' };
  // when
  const error = safeFailure(server, true);
  // then
  assert.equal(error.code, 'RPC_FAILURE');
  assert.equal(error.outcome, 'unknown');
  assert.ok(!JSON.stringify(error).includes(SECRET));
});

async function terminationObservation(value) {
  let requestError; let closeError;
  try { await value.client.request('create_document', wire(CONFIG, auth().owner, 'connection', {}), { write: true }); }
  catch (error) { requestError = { code: error.code, outcome: error.outcome }; }
  const before = { observed: value.client.child_exit_observed, profile: fs.existsSync(value.client.profile) };
  try { await value.client.close(); } catch (error) { closeError = { code: error.code, outcome: error.outcome }; }
  return { requestError, closeError, before, after: { observed: value.client.child_exit_observed, profile: fs.existsSync(value.client.profile) } };
}

test('kill 이후 실제 close가 늦게 오면 관찰한 다음 private 프로필을 정리한다', async () => {
  // given
  const value = rpc((frame, child) => { if (frame.method === 'create_document') queueMicrotask(() => child.stdout.write('invalid-json\n')); }, { closeTimeout: 50 });
  value.child.kill = signal => { value.child.kills.push(signal); setTimeout(() => value.child.emit('close', null, signal), 15); return true; };
  // when
  const result = await terminationObservation(value);
  // then
  assert.deepEqual(result.requestError, { code: 'INVALID_RPC_FRAME', outcome: 'unknown' });
  assert.deepEqual(result.before, { observed: false, profile: true });
  assert.deepEqual(result.after, { observed: true, profile: false });
  assert.equal(result.closeError, undefined);
  assert.equal(value.child.frames.filter(frame => frame.method === 'create_document').length, 1);
});

test('kill 이후 close를 확인하지 못하면 정리 성공 대신 unknown 오류를 반환한다', async t => {
  // given
  const value = rpc((frame, child) => { if (frame.method === 'create_document') queueMicrotask(() => child.stdout.write('invalid-json\n')); }, { closeTimeout: 5 });
  value.child.kill = signal => { value.child.kills.push(signal); return true; };
  t.after(() => fs.rmSync(value.client.profile, { recursive: true, force: true }));
  // when
  const result = await terminationObservation(value);
  // then
  assert.deepEqual(result.closeError, { code: 'DRIVER_EXITED', outcome: 'unknown' });
  assert.deepEqual(result.after, { observed: false, profile: true });
  assert.equal(value.client.pending.size, 0);
  assert.equal(value.child.frames.filter(frame => frame.method === 'create_document').length, 1);
});
