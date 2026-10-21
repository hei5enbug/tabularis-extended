import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { fixture, runCLI, audit } from './fixtures.mjs';

test('공개 CLI를 실행하면 stdin을 읽지 않고 한 JSON 성공 응답을 반환한다', t => {
  // given
  const f = fixture(t);
  const args = ['--source', f.source, '--output', f.output];
  // when
  const actual = runCLI(args);
  // then
  assert.equal(actual.status, 0);
  assert.equal(actual.stderr, '');
  assert.equal(actual.stdout.trim().split('\n').length, 1);
  assert.equal(JSON.parse(actual.stdout).ok, true);
  assert.equal(actual.stdout.includes(f.root), false);
  assert.equal(actual.stdout.includes('stdin-secret-canary'), false);
  assert.equal(Object.keys(audit(f.output)).length, 8);
});

for (const args of [[], ['--source', '/tmp'], ['--source', '/tmp', '--source', '/tmp'], ['--source', '/tmp', '--unknown', '/tmp'], ['--source', 'relative', '--output', '/tmp/example.zip'], ['--source', '/tmp', '--output', 'relative.zip'], ['--source', '/tmp', '--output', '/tmp/example.zip', '--replace', 'true']]) {
  test('잘못된 공개 CLI 인수는 코드만 반환하고 파일을 만들지 않는다: ' + JSON.stringify(args), () => {
    // given
    const input = args;
    // when
    const actual = runCLI(input);
    // then
    assert.equal(actual.status, 1);
    assert.deepEqual(JSON.parse(actual.stdout), { ok: false, code: 'INVALID_ARGUMENT' });
    assert.equal(actual.stderr, '');
  });
}

test('비밀처럼 보이는 누락 경로도 CLI 오류나 stderr에 노출되지 않는다', t => {
  // given
  const f = fixture(t);
  const source = path.join(f.root, 'password-secret-canary');
  // when
  const actual = runCLI(['--source', source, '--output', f.output]);
  // then
  assert.equal(actual.status, 1);
  assert.deepEqual(JSON.parse(actual.stdout), { ok: false, code: 'PACKAGE_FAILED' });
  assert.equal(actual.stderr, '');
  assert.equal(actual.stdout.includes('secret-canary'), false);
  assert.equal(fs.existsSync(f.output), false);
});

test('공개 CLI의 출력 순서를 바꾸어도 같은 계약을 사용한다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = runCLI(['--output', f.output, '--source', f.source]);
  // then
  assert.equal(actual.status, 0);
  assert.equal(JSON.parse(actual.stdout).ok, true);
});
