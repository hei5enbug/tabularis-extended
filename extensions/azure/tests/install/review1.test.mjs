import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { packageBundle } from '../../scripts/package/index.mjs';
import { boundedLimits } from '../../scripts/package/errors.mjs';
import { readJSON, Stage } from '../../scripts/package/files.mjs';
import { materializeProductionGraph } from '../../scripts/package/graph.mjs';
import { ACCEPTED_MANIFEST, capture, fixture, write } from './fixtures.mjs';

for (const [title, change] of [
  ['UI 슬롯 목록이 비어 있으면 준비되지 않은 manifest로 거부한다', m => { m.ui_extensions = []; }],
  ['알 수 없는 UI 슬롯을 거부한다', m => { m.ui_extensions[0].slot = 'query-results.toolbar.actions'; }],
  ['중복 UI 슬롯을 거부한다', m => { m.ui_extensions[0] = m.ui_extensions[1]; }],
  ['UI 슬롯의 driver가 없으면 거부한다', m => { delete m.ui_extensions[0].driver; }],
  ['다른 driver의 UI 슬롯을 거부한다', m => { m.ui_extensions[0].driver = 'postgresql'; }],
  ['UI 슬롯에 추가 필드가 있으면 거부한다', m => { m.ui_extensions[0].extra = true; }],
  ['UI 슬롯이 하나 더 있으면 거부한다', m => { m.ui_extensions.push({ ...m.ui_extensions[0] }); }],
  ['document paradigm이 없으면 거부한다', m => { m.paradigms = []; }],
  ['관계형 DDL capability가 켜져 있으면 거부한다', m => { m.capabilities.manage_tables = true; }],
  ['필수 false capability가 없으면 거부한다', m => { delete m.capabilities.file_based; }],
  ['다른 manifest 이름을 거부한다', m => { m.name = 'other'; }],
  ['비어 있는 version을 거부한다', m => { m.version = ' '; }],
  ['필수 settings 배열이 없으면 거부한다', m => { delete m.settings; }],
  ['알 수 없는 최상위 manifest 필드를 거부한다', m => { m.bootstrap = false; }],
  ['connection metadata 준비 상태가 false이면 거부한다', m => { m.connection_metadata = false; }],
]) {
  test(title, t => {
    // given
    const value = fixture(t);
    const manifest = structuredClone(ACCEPTED_MANIFEST);
    change(manifest);
    write(path.join(value.source, 'manifest.json'), manifest);
    // when
    const result = capture(() => packageBundle(value.options));
    // then
    assert.equal(result.error, 'MANIFEST_NOT_READY');
    assert.equal(fs.existsSync(value.options.output), false);
  });
}

test('1MiB를 넘는 유효 JSON을 읽기 전에 거부한다', t => {
  // given
  const value = fixture(t);
  const file = path.join(value.root, 'oversize.json');
  write(file, `${' '.repeat(1024 * 1024)}{"x":1}`);
  // when
  const result = capture(() => readJSON(file));
  // then
  assert.equal(result.error, 'INVALID_JSON');
});

test('정확히 1MiB인 유효 JSON은 허용한다', t => {
  // given
  const value = fixture(t);
  const file = path.join(value.root, 'boundary.json');
  write(file, `${' '.repeat(1024 * 1024 - 7)}{"x":1}`);
  // when
  const result = capture(() => readJSON(file));
  // then
  assert.equal(result.error, null);
  assert.deepEqual(result.value, { x: 1 });
});

test('JSON 파일의 심볼릭 링크를 거부한다', t => {
  // given
  const value = fixture(t);
  const file = path.join(value.root, 'linked.json');
  fs.symlinkSync(path.join(value.source, 'package.json'), file);
  // when
  const result = capture(() => readJSON(file));
  // then
  assert.equal(result.error, 'INVALID_JSON');
});

function growAfterStat(file) {
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
      write(file, `${' '.repeat(2 * 1024 * 1024)}{"x":1}`);
    }
    return result;
  };
  fs.readSync = (descriptor, ...args) => {
    const count = read(descriptor, ...args);
    if (descriptor === target) bytes += count;
    return count;
  };
  try { return { result: capture(() => readJSON(file)), bytes }; }
  finally { fs.openSync = open; fs.fstatSync = stat; fs.readSync = read; }
}

test('fd stat 뒤 커진 JSON도 최대 1MiB와 한 바이트까지만 읽는다', t => {
  // given
  const value = fixture(t);
  const file = path.join(value.root, 'growing.json');
  write(file, { x: 1 });
  // when
  const result = growAfterStat(file);
  // then
  assert.equal(result.result.error, 'INVALID_JSON');
  assert.ok(result.bytes <= 1024 * 1024 + 1);
});

test('의존성 package JSON이 상한을 넘으면 게시하지 않는다', t => {
  // given
  const value = fixture(t);
  write(path.join(value.source, 'node_modules/@azure/cosmos/package.json'), `${' '.repeat(1024 * 1024)}{"name":"@azure/cosmos"}`);
  // when
  const result = capture(() => packageBundle(value.options));
  // then
  assert.equal(result.error, 'INVALID_JSON');
  assert.equal(fs.existsSync(value.options.output), false);
});

function graphWithReadCanary(stage, source, licenseRoot) {
  const readFile = fs.readFileSync;
  const open = fs.openSync;
  const read = fs.readSync;
  const descriptors = new Set();
  let reads = 0;
  fs.readFileSync = (file, ...args) => {
    if (typeof file === 'string' && file.startsWith(`${licenseRoot}${path.sep}`) && /LICENSE/.test(path.basename(file))) {
      reads += 1;
      throw new Error('BULK_LICENSE_READ');
    }
    return readFile(file, ...args);
  };
  fs.openSync = (file, ...args) => {
    const descriptor = open(file, ...args);
    if (typeof file === 'string' && file.startsWith(`${licenseRoot}${path.sep}`) && /LICENSE/.test(path.basename(file))) descriptors.add(descriptor);
    else descriptors.delete(descriptor);
    return descriptor;
  };
  fs.readSync = (descriptor, ...args) => {
    if (descriptors.has(descriptor)) reads += 1;
    return read(descriptor, ...args);
  };
  try { return { result: capture(() => materializeProductionGraph(stage, source)), reads }; }
  finally { fs.readFileSync = readFile; fs.openSync = open; fs.readSync = read; }
}

test('상한을 넘는 sparse 라이선스는 내용을 읽기 전에 거부한다', t => {
  // given
  const value = fixture(t);
  const licenseRoot = path.join(value.source, 'node_modules/@azure/cosmos');
  fs.truncateSync(path.join(licenseRoot, 'LICENSE'), 256 * 1024 * 1024 + 1);
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits());
  // when
  const result = graphWithReadCanary(stage, value.source, licenseRoot);
  // then
  assert.equal(result.result.error, 'PACKAGE_TOO_LARGE');
  assert.equal(result.reads, 0);
  assert.equal(stage.entries.size, 0);
});

test('라이선스 합계가 남은 예산을 넘으면 해시 읽기 전에 거부한다', t => {
  // given
  const value = fixture(t);
  const licenseRoot = path.join(value.source, 'node_modules/@azure/cosmos');
  write(path.join(licenseRoot, 'LICENSE'), 'x'.repeat(40));
  write(path.join(licenseRoot, 'LICENSE-SECOND'), 'y'.repeat(40));
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits({ bytes: 64 }));
  // when
  const result = graphWithReadCanary(stage, value.source, licenseRoot);
  // then
  assert.equal(result.result.error, 'PACKAGE_TOO_LARGE');
  assert.equal(result.reads, 0);
  assert.equal(stage.entries.size, 0);
});

test('패키지 root 항목이 파일 상한을 넘으면 라이선스 내용을 읽지 않는다', t => {
  // given
  const value = fixture(t);
  const licenseRoot = path.join(value.source, 'node_modules/@azure/cosmos');
  for (let index = 0; index < 5; index += 1) write(path.join(licenseRoot, `extra-${index}`), 'x');
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits({ files: 4 }));
  // when
  const result = graphWithReadCanary(stage, value.source, licenseRoot);
  // then
  assert.equal(result.result.error, 'PACKAGE_TOO_LARGE');
  assert.equal(result.reads, 0);
});

test('여러 원문 라이선스의 순차 해시는 파일명과 구분자를 보존한다', t => {
  // given
  const value = fixture(t);
  const licenseRoot = path.join(value.source, 'node_modules/@azure/cosmos');
  const licenses = [['LICENSE', '첫 원문\n'], ['LICENSE-SECOND', 'Second original\n']];
  const expected = createHash('sha256');
  for (const [name, content] of licenses) {
    write(path.join(licenseRoot, name), content);
    expected.update(name).update(Buffer.from([0])).update(content).update(Buffer.from([0]));
  }
  const digest = expected.digest('hex');
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits());
  // when
  const graph = materializeProductionGraph(stage, value.source);
  // then
  assert.equal(graph.notices.find(item => item.name === '@azure/cosmos').sha256, digest);
  assert.ok(stage.entries.has(`licenses/packages/@azure/cosmos/4.10.1/${digest}/LICENSE`));
  assert.ok(stage.entries.has(`licenses/packages/@azure/cosmos/4.10.1/${digest}/LICENSE-SECOND`));
});

function publicationWithCleanupFault(options, mode) {
  const unlink = fs.unlinkSync;
  const remove = fs.rmSync;
  const rename = fs.renameSync;
  const owned = [];
  let nextUnlinks = 0;
  fs.unlinkSync = file => {
    if (file.includes('.tabularis-package-next-')) {
      nextUnlinks += 1;
      throw new Error('synthetic temp unlink failure');
    }
    return unlink(file);
  };
  fs.rmSync = (file, ...args) => {
    if (mode !== 'unlink' && (path.basename(file).startsWith('tabularis-azure-package-') || path.basename(file).startsWith('.tabularis-package-next-'))) {
      owned.push(file);
      throw new Error('synthetic cleanup failure');
    }
    return remove(file, ...args);
  };
  fs.renameSync = (source, destination) => {
    if (mode === 'partial' && destination === `${options.output}.sha256`) throw new Error('synthetic checksum failure');
    return rename(source, destination);
  };
  try { return { result: capture(() => packageBundle({ ...options, replace: mode === 'partial' })), nextUnlinks }; }
  finally {
    fs.unlinkSync = unlink;
    fs.rmSync = remove;
    fs.renameSync = rename;
    for (const file of owned) remove(file, { recursive: true, force: true });
  }
}

test('기본 게시에서는 원본 임시 링크 unlink 실패 경로를 호출하지 않는다', t => {
  // given
  const value = fixture(t);
  // when
  const result = publicationWithCleanupFault(value.options, 'unlink');
  // then
  assert.equal(result.result.error, null);
  assert.equal(result.nextUnlinks, 0);
  assert.equal(fs.existsSync(value.options.output), true);
  assert.equal(fs.existsSync(`${value.options.output}.sha256`), true);
});

test('두 파일 게시 뒤 cleanup 실패는 적용된 게시 상태로 알린다', t => {
  // given
  const value = fixture(t);
  // when
  const result = publicationWithCleanupFault(value.options, 'cleanup');
  // then
  assert.equal(result.result.error, 'PACKAGE_CLEANUP_FAILED_APPLIED');
  assert.equal(fs.existsSync(value.options.output), true);
  assert.equal(fs.existsSync(`${value.options.output}.sha256`), true);
});

test('부분 게시 오류는 finally cleanup 실패로 덮어쓰지 않는다', t => {
  // given
  const value = fixture(t);
  write(value.options.output, 'old zip');
  write(`${value.options.output}.sha256`, 'old checksum');
  // when
  const result = publicationWithCleanupFault(value.options, 'partial');
  // then
  assert.equal(result.result.error, 'PARTIAL_PUBLICATION_APPLIED');
  assert.equal(fs.existsSync(value.options.output), true);
  assert.equal(fs.readFileSync(`${value.options.output}.sha256`, 'utf8'), 'old checksum');
});
