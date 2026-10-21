import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { uiStyle } from '../../scripts/package/manifest.mjs';
import { capture, fixture, write } from './fixtures.mjs';

function compareWithoutBulkRead(source) {
  const read = fs.readFileSync;
  let bulkReads = 0;
  fs.readFileSync = (file, ...args) => {
    if (typeof file === 'string' && file.endsWith('.css')) bulkReads += 1;
    return read(file, ...args);
  };
  try { return { result: capture(() => uiStyle(source)), bulkReads }; }
  finally { fs.readFileSync = read; }
}

test('동일한 두 CSS를 전체 buffer 읽기 없이 비교한다', t => {
  // given
  const value = fixture(t);
  const canonical = path.join(value.source, 'ui/dist/style.css');
  write(canonical, fs.readFileSync(path.join(value.source, 'ui/dist/azure-ui.css')));
  // when
  const result = compareWithoutBulkRead(value.source);
  // then
  assert.equal(result.result.error, null);
  assert.equal(result.result.value, fs.realpathSync(canonical));
  assert.equal(result.bulkReads, 0);
});

for (const [title, name] of [['canonical CSS만 있어도 허용한다', 'style.css'], ['빌드 이름의 CSS만 있어도 허용한다', 'azure-ui.css']]) {
  test(title, t => {
    // given
    const value = fixture(t);
    if (name === 'style.css') fs.renameSync(path.join(value.source, 'ui/dist/azure-ui.css'), path.join(value.source, 'ui/dist/style.css'));
    // when
    const result = capture(() => uiStyle(value.source));
    // then
    assert.equal(result.error, null);
    assert.equal(result.value, fs.realpathSync(path.join(value.source, 'ui/dist', name)));
  });
}

function refuseBulkRead(source) {
  const read = fs.readFileSync;
  let bulkReads = 0;
  fs.readFileSync = (file, ...args) => {
    if (typeof file === 'string' && file.endsWith('.css')) {
      bulkReads += 1;
      throw new Error('BULK_STYLE_READ');
    }
    return read(file, ...args);
  };
  try { return { result: capture(() => uiStyle(source)), bulkReads }; }
  finally { fs.readFileSync = read; }
}

for (const [title, both] of [['두 CSS 중 큰 sparse 입력은 내용 읽기 전에 거부한다', true], ['CSS가 하나여도 256MiB 상한을 먼저 확인한다', false]]) {
  test(title, t => {
    // given
    const value = fixture(t);
    const built = path.join(value.source, 'ui/dist/azure-ui.css');
    fs.truncateSync(built, 256 * 1024 * 1024 + 1);
    if (both) write(path.join(value.source, 'ui/dist/style.css'), 'normal CSS');
    // when
    const result = refuseBulkRead(value.source);
    // then
    assert.equal(result.result.error, 'PACKAGE_TOO_LARGE');
    assert.equal(result.bulkReads, 0);
  });
}

test('CSS 파일의 심볼릭 링크를 거부한다', t => {
  // given
  const value = fixture(t);
  fs.symlinkSync(path.join(value.source, 'ui/dist/azure-ui.css'), path.join(value.source, 'ui/dist/style.css'));
  // when
  const result = capture(() => uiStyle(value.source));
  // then
  assert.equal(result.error, 'INVALID_FILE');
});

test('CSS 상위 디렉터리의 심볼릭 링크를 거부한다', t => {
  // given
  const value = fixture(t);
  const dist = path.join(value.source, 'ui/dist');
  const moved = path.join(value.root, 'moved-dist');
  fs.renameSync(dist, moved);
  fs.symlinkSync(moved, dist, process.platform === 'win32' ? 'junction' : 'dir');
  // when
  const result = capture(() => uiStyle(value.source));
  // then
  assert.equal(result.error, 'INVALID_FILE');
});

function growingCSS(source, file) {
  const open = fs.openSync;
  const stat = fs.fstatSync;
  const read = fs.readSync;
  let target;
  let grew = false;
  let bytes = 0;
  fs.openSync = (name, ...args) => {
    const descriptor = open(name, ...args);
    if (name === file) target = descriptor;
    return descriptor;
  };
  fs.fstatSync = (descriptor, ...args) => {
    const result = stat(descriptor, ...args);
    if (descriptor === target && !grew) {
      grew = true;
      write(file, 'x'.repeat(2 * 1024 * 1024));
    }
    return result;
  };
  fs.readSync = (descriptor, ...args) => {
    const count = read(descriptor, ...args);
    if (descriptor === target) bytes += count;
    return count;
  };
  try { return { result: capture(() => uiStyle(source)), bytes }; }
  finally { fs.openSync = open; fs.fstatSync = stat; fs.readSync = read; }
}

test('fd stat 뒤 커진 CSS도 선언된 크기와 한 바이트까지만 읽는다', t => {
  // given
  const value = fixture(t);
  const canonical = path.join(value.source, 'ui/dist/style.css');
  const data = fs.readFileSync(path.join(value.source, 'ui/dist/azure-ui.css'));
  write(canonical, data);
  const target = fs.realpathSync(canonical);
  // when
  const result = growingCSS(value.source, target);
  // then
  assert.equal(result.result.error, 'PACKAGE_TOO_LARGE');
  assert.ok(result.bytes <= data.length + 1);
});
