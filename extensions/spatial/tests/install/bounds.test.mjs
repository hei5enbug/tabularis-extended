import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { DEFAULT_LIMITS, limitsFor, readBounded, relative, Stage } from '../../scripts/package/files.mjs';
import { writeZip } from '../../scripts/package/zip.mjs';
import { fixture, attempt, addPackage, write } from './fixtures.mjs';

for (const [label, limits] of [
  ['전체 bytes', { bytes: 1 }], ['개별 asset bytes', { fileBytes: 20 }], ['파일 개수', { files: 7 }],
  ['manifest JSON bytes', { jsonBytes: 32 }], ['license bytes', { licenseBytes: 5 }], ['ZIP bytes', { zipBytes: 1 }],
]) {
  test(`${label} 예산을 낮추면 상한을 넘는 ZIP은 게시되지 않는다`, t => {
    // given
    const f = fixture(t);
    // when
    const actual = attempt(f, { limits });
    // then
    assert.ok(['PACKAGE_TOO_LARGE', 'INVALID_JSON', 'ZIP_TOO_LARGE'].includes(actual.error));
    assert.equal(fs.existsSync(f.output), false);
    assert.equal(fs.existsSync(`${f.output}.sha256`), false);
    assert.deepEqual(actual.remaining, []);
  });
}

for (const [name, value] of [['bytes', DEFAULT_LIMITS.bytes + 1], ['fileBytes', 0], ['files', 1.5], ['unknown', 1], ['toString', 1], ['depth', Number.MAX_SAFE_INTEGER]]) {
  test(`${name} 상한 증가나 잘못된 값을 지정하면 API가 거부한다`, () => {
    // given
    const input = { [name]: value };
    // when
    const actual = (() => { try { limitsFor(input); return null; } catch (error) { return error.code; } })();
    // then
    assert.equal(actual, 'INVALID_LIMITS');
  });
}

for (const [label, limits] of [['roots', { dependencyRoots: 1 }], ['depth', { depth: 1 }]]) {
  test(`${label} closure 예산을 넘으면 license 탐색을 중단한다`, t => {
    // given
    const f = fixture(t, { dependencies: { child: '1' } });
    addPackage(f.maplibre, 'child');
    // when
    const actual = attempt(f, { limits });
    // then
    assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
    assert.equal(fs.existsSync(f.output), false);
  });
}

for (const name of ['ui/dist/index.js', 'ui/dist/style.css', 'ui/dist/maplibre-worker.js', 'manifest.json']) {
  test(`${name} symlink 입력은 bundle 경계를 넘지 못한다`, t => {
    // given
    const f = fixture(t);
    const file = path.join(f.source, name);
    fs.renameSync(file, `${file}.copy`);
    fs.symlinkSync(`${file}.copy`, file);
    // when
    const actual = attempt(f);
    // then
    assert.equal(actual.error, 'INVALID_FILE');
    assert.equal(fs.existsSync(f.output), false);
  });
}

test('asset 부모 directory가 symlink이면 정규파일이어도 거부한다', t => {
  // given
  const f = fixture(t);
  fs.renameSync(path.join(f.source, 'ui/dist'), path.join(f.root, 'linked-assets'));
  fs.symlinkSync(path.join(f.root, 'linked-assets'), path.join(f.source, 'ui/dist'), process.platform === 'win32' ? 'junction' : 'dir');
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, 'INVALID_FILE');
  assert.equal(fs.existsSync(f.output), false);
});

test('필수 asset이 없으면 자동 build 없이 실패한다', t => {
  // given
  const f = fixture(t);
  fs.unlinkSync(path.join(f.source, 'ui/dist/maplibre-worker.js'));
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, 'PACKAGE_FAILED');
  assert.equal(fs.existsSync(f.output), false);
  assert.deepEqual(actual.remaining, []);
});

function oversizedRead(t, f, file, size, options = {}) {
  const fd = fs.openSync(file, 'w');
  fs.ftruncateSync(fd, size);
  fs.closeSync(fd);
  let largeReads = 0;
  const original = fs.readSync;
  t.mock.method(fs, 'readSync', (readFD, ...args) => {
    if (fs.fstatSync(readFD).size === size) largeReads += 1;
    return original(readFD, ...args);
  });
  const actual = attempt(f, options);
  return { ...actual, largeReads };
}

for (const [name, size, code] of [['ui/dist/index.js', DEFAULT_LIMITS.fileBytes + 1, 'PACKAGE_TOO_LARGE'], ['manifest.json', DEFAULT_LIMITS.jsonBytes + 1, 'INVALID_JSON'], ['ui/node_modules/maplibre-gl/package.json', DEFAULT_LIMITS.jsonBytes + 1, 'INVALID_JSON'], ['ui/node_modules/maplibre-gl/LICENSE', DEFAULT_LIMITS.licenseBytes + 1, 'PACKAGE_TOO_LARGE']]) {
  test(`${name} sparse oversized 입력은 bytes를 읽기 전에 거부된다`, t => {
    // given
    const f = fixture(t);
    const file = path.join(f.source, name);
    // when
    const actual = oversizedRead(t, f, file, size);
    // then
    assert.equal(actual.error, code);
    assert.equal(actual.largeReads, 0);
    assert.equal(fs.existsSync(f.output), false);
  });
}

test('license들의 합이 남은 예산을 넘으면 큰 원문을 읽지 않는다', t => {
  // given
  const f = fixture(t);
  write(f.maplibre, 'COPYING', Buffer.alloc(600));
  write(f.maplibre, 'LICENSE', Buffer.alloc(600));
  // when
  const actual = oversizedRead(t, f, path.join(f.maplibre, 'LICENSE'), 600, { limits: { bytes: 1600 } });
  // then
  assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
  assert.equal(actual.largeReads, 0);
  assert.equal(fs.existsSync(f.output), false);
});

function mutateDuringRead(t, file, mode) {
  const original = fs.readSync;
  let changed = false;
  if (mode === 'unchanged_metadata') {
    const initial = fs.statSync(file);
    for (const method of ['fstatSync', 'lstatSync']) {
      const stat = fs[method];
      t.mock.method(fs, method, (...args) => {
        const current = stat(...args);
        if (current.isFile()) {
          current.mtimeMs = initial.mtimeMs;
          current.ctimeMs = initial.ctimeMs;
        }
        return current;
      });
    }
  }
  t.mock.method(fs, 'readSync', (fd, ...args) => {
    const count = original(fd, ...args);
    if (!changed) {
      changed = true;
      if (mode === 'growth') fs.appendFileSync(file, 'growth');
      else fs.writeFileSync(file, 'different');
    }
    return count;
  });
  try { readBounded(file, 16); return null; } catch (error) { return error.code; }
}

for (const mode of ['growth', 'drift', 'unchanged_metadata']) {
  test(`bounded read 중 ${mode}이 발생하면 원본 변경을 거부한다`, t => {
    // given
    const f = fixture(t);
    const file = write(f.root, 'mutable', 'original!');
    // when
    const actual = mutateDuringRead(t, file, mode);
    // then
    assert.equal(actual, 'SOURCE_CHANGED');
  });
}

test('stage의 SHA 기록 뒤 bytes가 바뀌면 ZIP은 게시할 수 없다', t => {
  // given
  const f = fixture(t);
  const root = path.join(f.root, 'isolated-stage');
  fs.mkdirSync(root);
  const stage = new Stage(root, limitsFor());
  stage.add('fixed', Buffer.from('original'));
  fs.writeFileSync(path.join(root, 'fixed'), 'modified');
  // when
  const actual = (() => { try { writeZip(stage, path.join(f.root, 'next.zip')); return null; } catch (error) { return error.code; } })();
  // then
  assert.equal(actual, 'SOURCE_CHANGED');
});

for (const value of ['../escape', '/absolute', 'a\\b', 'a:b', 'a\0b', 'a//b', 'a/./b', '\ud800']) {
  test('안전하지 않은 ZIP 상대 경로를 검증하면 거부한다: ' + JSON.stringify(value), () => {
    // given
    const name = value;
    // when
    const actual = (() => { try { relative(name); return null; } catch (error) { return error.code; } })();
    // then
    assert.equal(actual, 'INVALID_PATH');
  });
}
