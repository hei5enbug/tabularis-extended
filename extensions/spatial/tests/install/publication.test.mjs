import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { fixture, attempt, write } from './fixtures.mjs';

for (const suffix of ['', '.sha256']) {
  test(`기존 ${suffix || 'ZIP'}을 지정하면 원본을 보호한다`, t => {
    // given
    const f = fixture(t);
    const original = Buffer.from('existing-private-canary');
    fs.writeFileSync(`${f.output}${suffix}`, original);
    // when
    const actual = attempt(f);
    // then
    assert.equal(actual.error, 'OUTPUT_EXISTS');
    assert.deepEqual(fs.readFileSync(`${f.output}${suffix}`), original);
    assert.equal(fs.existsSync(`${f.output}${suffix ? '' : '.sha256'}`), false);
  });
}

function publicationRace(t, f, kind) {
  const original = fs.linkSync;
  t.mock.method(fs, 'linkSync', (source, destination) => {
    if (kind === 'zip' && destination === f.output) {
      fs.writeFileSync(destination, 'competing ZIP', { flag: 'wx' });
    }
    if (destination === `${f.output}.sha256` && kind !== 'zip') {
      if (kind === 'mutation') fs.writeFileSync(f.output, 'external modified ZIP');
      if (kind === 'replacement') {
        fs.unlinkSync(f.output);
        fs.writeFileSync(f.output, 'external replaced ZIP');
      }
      fs.writeFileSync(destination, 'competing checksum', { flag: 'wx' });
    }
    return original(source, destination);
  });
  return attempt(f);
}

test('ZIP 게시 직전 destination 경쟁이 생기면 원자적으로 덮어쓰지 않는다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = publicationRace(t, f, 'zip');
  // then
  assert.equal(actual.error, 'OUTPUT_EXISTS');
  assert.equal(fs.readFileSync(f.output, 'utf8'), 'competing ZIP');
  assert.equal(fs.existsSync(`${f.output}.sha256`), false);
  assert.deepEqual(actual.remaining, []);
});

test('checksum 경쟁 후 자기 ZIP의 inode와 SHA가 같으면 자기 게시만 제거한다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = publicationRace(t, f, 'checksum');
  // then
  assert.equal(actual.error, 'OUTPUT_EXISTS');
  assert.equal(fs.existsSync(f.output), false);
  assert.equal(fs.readFileSync(`${f.output}.sha256`, 'utf8'), 'competing checksum');
  assert.deepEqual(actual.remaining, []);
});

for (const kind of ['mutation', 'replacement']) {
  test(`checksum 실패 뒤 외부 ${kind} ZIP은 제거하거나 성공으로 숨기지 않는다`, t => {
    // given
    const f = fixture(t);
    // when
    const actual = publicationRace(t, f, kind);
    // then
    assert.equal(actual.error, 'PARTIAL_PUBLICATION_APPLIED');
    assert.equal(fs.readFileSync(f.output, 'utf8'), kind === 'mutation' ? 'external modified ZIP' : 'external replaced ZIP');
    assert.equal(fs.readFileSync(`${f.output}.sha256`, 'utf8'), 'competing checksum');
    assert.deepEqual(actual.remaining, []);
  });
}

function cleanupFailure(t, f) {
  const original = fs.rmSync;
  t.mock.method(fs, 'rmSync', (file, options) => {
    if (path.basename(file).startsWith('.tabularis-spatial-package-')) throw Error('cleanup-secret-canary');
    return original(file, options);
  });
  return attempt(f);
}

test('완료된 두 게시의 cleanup이 실패해도 적용된 상태를 숨기지 않는다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = cleanupFailure(t, f);
  // then
  assert.equal(actual.error, 'PARTIAL_PUBLICATION_APPLIED');
  assert.equal(fs.existsSync(f.output), true);
  assert.match(fs.readFileSync(`${f.output}.sha256`, 'utf8'), /^[0-9a-f]{64}\n$/);
  assert.equal(actual.message.includes('secret-canary'), false);
});

function mutationAfterZipHash(t, f) {
  const open = fs.openSync;
  const sync = fs.fsyncSync;
  const zipFDs = new Set();
  t.mock.method(fs, 'openSync', (file, ...args) => {
    const fd = open(file, ...args);
    if (path.basename(file) === 'bundle.zip') zipFDs.add(fd);
    return fd;
  });
  t.mock.method(fs, 'fsyncSync', fd => {
    const result = sync(fd);
    if (zipFDs.has(fd)) write(f.source, 'ui/dist/index.js', 'changed-after-zip-SHA');
    return result;
  });
  return attempt(f);
}

test('ZIP SHA 계산 뒤 source asset이 변하면 게시 전에 거부한다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = mutationAfterZipHash(t, f);
  // then
  assert.equal(actual.error, 'SOURCE_CHANGED');
  assert.equal(fs.existsSync(f.output), false);
  assert.deepEqual(actual.remaining, []);
});

test('게시 전 실패는 다른 owned fixture와 외부 sentinel을 지우지 않는다', t => {
  // given
  const f = fixture(t);
  const sentinel = write(f.root, 'sentinel', 'preserve-me');
  fs.unlinkSync(path.join(f.source, 'ui/dist/index.js'));
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, 'PACKAGE_FAILED');
  assert.equal(fs.readFileSync(sentinel, 'utf8'), 'preserve-me');
  assert.equal(fs.existsSync(path.join(f.source, 'manifest.json')), true);
  assert.deepEqual(actual.remaining, []);
});
