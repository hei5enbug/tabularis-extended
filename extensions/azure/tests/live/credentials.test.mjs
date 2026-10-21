import test from 'node:test';
import assert from 'node:assert/strict';
import { readCredentialFD, releaseCredential } from '../../scripts/live/credentials.mjs';
import { CONFIG, SECRET, auth, capture, fdReader } from './synthetic-support.mjs';

test('명시된 FD reader의 account key JSON만 메모리로 수용한다', () => {
  // given
  const value = auth().owner; const read = fdReader(JSON.stringify(value));
  // when
  const result = readCredentialFD(3, CONFIG, { read });
  // then
  assert.deepEqual(result, value);
});

for (const [title, text] of [
  ['빈 입력', ''], ['과대 입력', 'x'.repeat(65537)], ['잘못된 UTF-8', Buffer.from([0xff])], ['raw 비밀', SECRET],
  ['추가 필드', JSON.stringify({ ...auth().owner, password: SECRET })], ['잘못된 shape', '[]'], ['identity 누락', JSON.stringify({ kind: 'account_key', account_key: SECRET })],
]) {
  test(`${title} credential은 비밀 없는 고정 오류로 거부한다`, () => {
    // given
    const read = fdReader(text);
    // when
    const result = capture(() => readCredentialFD(3, CONFIG, { read }));
    // then
    assert.equal(result.code, 'INVALID_CREDENTIAL');
    assert.ok(!JSON.stringify(result).includes(SECRET));
  });
}

const tenant = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'; const client = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb';
const entraConfig = { ...CONFIG, auth_mode: 'entra_user', tenant_id: tenant, client_id: client };
const token = { kind: 'entra_token', access_token: SECRET, expires_at_ms: 1_000_000, tenant_id: tenant, client_id: client, principal_id: 'cccccccc-cccc-4ccc-8ccc-cccccccccccc', scope: 'https://cosmos.azure.com/.default', identity: 'synthetic-principal' };
test('Entra context는 설정과 scope와 요청 여유 만료를 모두 확인한다', () => {
  // given
  const read = fdReader(JSON.stringify(token));
  // when
  const result = readCredentialFD(4, entraConfig, { read, now: () => 1 });
  // then
  assert.deepEqual(result, token);
});

for (const [title, patch] of [
  ['scope 불일치', { scope: 'https://management.azure.com/.default' }],
  ['만료 여유 부족', { expires_at_ms: 420001 }],
  ['tenant 불일치', { tenant_id: client }],
  ['client 불일치', { client_id: tenant }],
  ['principal 잘못됨', { principal_id: 'unknown' }],
]) {
  test(`${title} Entra context는 기존 토큰을 쓰지 않고 거부한다`, () => {
    // given
    const read = fdReader(JSON.stringify({ ...token, ...patch }));
    // when
    const result = capture(() => readCredentialFD(4, entraConfig, { read, now: () => 1 }));
    // then
    assert.equal(result.code, 'INVALID_CREDENTIAL');
  });
}

test('잘못된 FD 번호는 주입된 reader도 호출하지 않는다', () => {
  // given
  let calls = 0;
  // when
  const result = capture(() => readCredentialFD(0, CONFIG, { read: () => { calls += 1; return 0; } }));
  // then
  assert.equal(result.code, 'INVALID_CREDENTIAL');
  assert.equal(calls, 0);
});

test('credential 해제는 token 참조 값을 비우되 완전한 zeroization을 주장하지 않는다', () => {
  // given
  const value = { ...token };
  // when
  releaseCredential(value);
  // then
  assert.equal(value.access_token, '');
  assert.equal(value.identity, token.identity);
});
