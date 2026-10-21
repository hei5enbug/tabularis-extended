import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { directory, limitsFor, readBounded, regularWithin, sha256 } from '../../scripts/package/files.mjs';
import { resolveInstalled } from '../../scripts/package/licenses.mjs';
import { fixture, addPackage, write, attempt, packageAndAudit, decode } from './fixtures.mjs';

function pinnedEvidence() {
  const source = directory(fileURLToPath(new URL('../../', import.meta.url)));
  const maplibre = resolveInstalled(path.join(source, 'ui'), 'maplibre-gl', limitsFor());
  const murmur = resolveInstalled(maplibre.root, 'murmurhash-js', limitsFor());
  return {
    package: readBounded(regularWithin(murmur.root, 'package.json'), 1024 * 1024),
    readme: readBounded(regularWithin(murmur.root, 'README.md'), 1024 * 1024),
  };
}

function addEvidence(f, evidence) {
  const pkg = addPackage(f.maplibre, 'murmurhash-js', '1.0.0', {}, null);
  write(pkg, 'package.json', evidence.package);
  write(pkg, 'README.md', evidence.readme);
  return pkg;
}

test('고정 upstream README의 전체 MIT 증거는 원본과 package JSON으로 보관된다', t => {
  // given
  const f = fixture(t, { dependencies: { 'murmurhash-js': '1.0.0' } });
  const evidence = pinnedEvidence();
  addEvidence(f, evidence);
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(evidence.package.length, 635);
  assert.equal(sha256(evidence.package), 'e4b3531abcc7da48f732a058112686b44af66004c38d98a8014619542e767cb4');
  assert.equal(evidence.readme.length, 1941);
  assert.equal(sha256(evidence.readme), 'e137ced8967fc334ec9b5fc5c8500992f9e49d2e9cc0f6e2439e46af2f2481a4');
  assert.equal(actual.notices.packages[1].license_source, 'embedded_readme');
  assert.equal(actual.notices.packages[1].source_url, 'https://github.com/mikolalysenko/murmurhash-js/tree/72aabce3f52cb8f16245692a69fd35951e165af0');
  assert.deepEqual(decode(actual.files[actual.notices.packages[1].files[0].path]), evidence.readme);
  assert.deepEqual(decode(actual.files[actual.notices.packages[1].files[1].path]), evidence.package);
  assert.ok(actual.notices.packages[1].files[0].path.endsWith('/UPSTREAM-README.md'));
  assert.ok(actual.notices.packages[1].files[1].path.endsWith('/UPSTREAM-package.json'));
});

for (const target of ['README.md', 'package.json']) {
  test(`정확한 pinned ${target} 한 byte가 달라지면 예외는 적용되지 않는다`, t => {
    // given
    const f = fixture(t, { dependencies: { 'murmurhash-js': '1.0.0' } });
    const evidence = pinnedEvidence();
    const pkg = addEvidence(f, evidence);
    const bytes = fs.readFileSync(path.join(pkg, target));
    if (target === 'README.md') bytes[0] ^= 1;
    else bytes[bytes.length - 1] = bytes[bytes.length - 1] === 10 ? 32 : 10;
    fs.writeFileSync(path.join(pkg, target), bytes);
    // when
    const actual = attempt(f);
    // then
    assert.equal(actual.error, 'LICENSE_MISSING');
    assert.equal(fs.existsSync(f.output), false);
  });
}

test('case가 다른 COPYING과 LICENSE 원문을 모두 보관한다', t => {
  // given
  const f = fixture(t);
  fs.renameSync(path.join(f.maplibre, 'LICENSE'), path.join(f.maplibre, 'licence.TXT'));
  write(f.maplibre, 'Copying.md', 'second original license');
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.notices.packages[0].files.length, 2);
  assert.ok(actual.notices.packages[0].files.some(item => item.path.endsWith('/licence.TXT')));
  assert.ok(actual.notices.packages[0].files.some(item => item.path.endsWith('/Copying.md')));
});
