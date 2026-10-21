import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { attempt, auditPython, directoryLinkType, fixture } from './fixtures.mjs';

function capture(action) { try { return { value: action(), error: null }; } catch (error) { return { value: null, error: error.message }; } }

for (const [platform, env, expected] of [
  ['darwin', {}, '/usr/bin/python3'], ['linux', {}, '/usr/bin/python3'],
  ['darwin', { TABULARIS_S3_TEST_PYTHON: '/fixture/python' }, '/fixture/python'],
  ['linux', { TABULARIS_S3_TEST_PYTHON: '/fixture/python' }, '/fixture/python'],
  ['win32', { TABULARIS_S3_TEST_PYTHON: 'C:\\fixture\\python.exe' }, 'C:\\fixture\\python.exe'],
]) {
  test(`${platform} ZIP audit은 승인된 Python 절대 경로를 사용한다`, () => {
    // given
    const input = { platform, env };
    // when
    const actual = auditPython(input.platform, input.env);
    // then
    assert.equal(actual, expected);
  });
}

for (const [platform, env] of [['win32', {}], ['win32', { TABULARIS_S3_TEST_PYTHON: 'relative/python.exe' }], ['linux', { TABULARIS_S3_TEST_PYTHON: 'relative/python' }], ['darwin', { TABULARIS_S3_TEST_PYTHON: '' }]]) {
  test(`${platform} Python 입력이 없거나 상대 경로이면 PATH로 탐색하지 않는다`, () => {
    // given
    const input = { platform, env };
    // when
    const actual = capture(() => auditPython(input.platform, input.env));
    // then
    assert.equal(actual.error, 'CAPABILITY_UNAVAILABLE');
    assert.equal(actual.value, null);
  });
}

test('directory fixture link는 Windows에서만 junction을 사용한다', () => {
  // given
  const platforms = ['win32', 'darwin', 'linux'];
  // when
  const actual = platforms.map(platform => directoryLinkType(platform));
  // then
  assert.deepEqual(actual, ['junction', 'dir', 'dir']);
});

function publishWithPlatform(t, f, platform, parentSyncFailure) {
  const descriptor = Object.getOwnPropertyDescriptor(process, 'platform');
  t.after(() => Object.defineProperty(process, 'platform', descriptor));
  Object.defineProperty(process, 'platform', { ...descriptor, value: platform });
  const open = fs.openSync, sync = fs.fsyncSync, close = fs.closeSync;
  const fileNames = new Map();
  const synced = [];
  let parentOpens = 0;
  const parentFD = -777;
  t.mock.method(fs, 'openSync', (file, ...args) => {
    if (file === path.dirname(f.output)) { parentOpens++; return parentFD; }
    const fd = open(file, ...args);
    fileNames.set(fd, path.basename(file));
    return fd;
  });
  t.mock.method(fs, 'fsyncSync', fd => {
    if (fd === parentFD) {
      synced.push('parent');
      if (parentSyncFailure) throw new Error('synthetic directory sync failure');
      return;
    }
    synced.push(fileNames.get(fd));
    return sync(fd);
  });
  t.mock.method(fs, 'closeSync', fd => { if (fd !== parentFD) return close(fd); });
  try {
    const actual = attempt(f);
    return { ...actual, parentOpens, synced, zipExists: fs.existsSync(f.output), checksumExists: fs.existsSync(`${f.output}.sha256`) };
  } finally { Object.defineProperty(process, 'platform', descriptor); }
}

test('Windows 발행은 parent 디렉터리를 열지 않고 ZIP과 checksum 파일을 동기화한다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = publishWithPlatform(t, f, 'win32', true);
  // then
  assert.equal(actual.error, null);
  assert.equal(actual.parentOpens, 0);
  assert.ok(actual.synced.includes('bundle.zip'));
  assert.ok(actual.synced.includes('bundle.sha256'));
  assert.equal(actual.synced.includes('parent'), false);
  assert.equal(actual.zipExists, true);
  assert.equal(actual.checksumExists, true);
});

for (const platform of ['darwin', 'linux']) {
  test(`${platform} parent fsync 실패는 이미 적용된 두 게시를 부분 완료로 보존한다`, t => {
    // given
    const f = fixture(t);
    // when
    const actual = publishWithPlatform(t, f, platform, true);
    // then
    assert.equal(actual.error, 'PARTIAL_PUBLICATION_APPLIED');
    assert.equal(actual.parentOpens, 1);
    assert.ok(actual.synced.includes('bundle.zip'));
    assert.ok(actual.synced.includes('bundle.sha256'));
    assert.ok(actual.synced.includes('parent'));
    assert.equal(actual.zipExists, true);
    assert.equal(actual.checksumExists, true);
    assert.deepEqual(actual.remaining, []);
  });
}
