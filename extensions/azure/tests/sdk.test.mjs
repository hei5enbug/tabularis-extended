import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { verifySdk } from '../scripts/sdk.mjs';

const source = fileURLToPath(new URL('../', import.meta.url));
function fixture(t) {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-sdk-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  fs.cpSync(path.join(source, 'build-support/sdk'), path.join(temporary, 'build-support/sdk'), {
    recursive: true, filter: file => !['dist', 'node_modules'].includes(path.basename(file)),
  });
  return temporary;
}

test('저장소 안의 SDK 파일과 라이선스가 고정된 출처 원장과 일치한다', () => {
  // given
  const root = source;
  // when
  const actual = verifySdk(root);
  // then
  assert.equal(actual.provenance.files.length, 22);
  assert.equal(actual.provenance.api_version, '0.2.0');
  assert.equal(actual.provenance.service_protocol, 1);
});

test('SDK 원본이 변조되면 설치나 빌드 전에 거부한다', t => {
  // given
  const root = fixture(t);
  fs.appendFileSync(path.join(root, 'build-support/sdk/packages/plugin-api/src/index.ts'), '\nchanged');
  // when
  let error;
  try { verifySdk(root); } catch (value) { error = value; }
  // then
  assert.equal(error?.code, 'SOURCE_MISMATCH');
});

test('출처 원장에 없는 SDK 소스가 추가되면 거부한다', t => {
  // given
  const root = fixture(t);
  fs.writeFileSync(path.join(root, 'build-support/sdk/packages/plugin-api/src/untracked.ts'), 'export const injected = true;');
  // when
  let error;
  try { verifySdk(root); } catch (value) { error = value; }
  // then
  assert.equal(error?.code, 'SOURCE_MISMATCH');
});

test('소스 아래의 dist 폴더도 생성물로 오인하지 않고 출처 검사를 적용한다', t => {
  // given
  const root = fixture(t);
  const extra = path.join(root, 'build-support/sdk/packages/plugin-api/src/dist/injected.ts');
  fs.mkdirSync(path.dirname(extra));
  fs.writeFileSync(extra, 'export const injected = true;');
  // when
  let error;
  try { verifySdk(root); } catch (value) { error = value; }
  // then
  assert.equal(error?.code, 'SOURCE_MISMATCH');
});

test('새 workspace에서 빌드한 계약 SDK는 모든 고정 요청과 응답 fixture를 동일하게 판정한다', async () => {
  // given
  const sdk = await import('../build-support/sdk/packages/service-contracts/dist/index.js');
  const fixtures = JSON.parse(fs.readFileSync(sdk.fixtureFile, 'utf8'));
  // when
  const actual = fixtures.map(item => (item.schema === 'request' ? sdk.validateRequest : sdk.validateResponse)(item.value).valid);
  // then
  assert.deepEqual(actual, fixtures.map(item => item.valid));
});
