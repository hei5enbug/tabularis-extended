import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { fixture, addPackage, packageAndAudit, attempt, json, write, directoryLinkType } from './fixtures.mjs';

test('dependency 순환을 패키징하면 canonical root를 한 번만 처리한다', t => {
  // given
  const f = fixture(t, { dependencies: { child: '1.0.0' } });
  const child = addPackage(f.maplibre, 'child', '1.0.0', { dependencies: { 'maplibre-gl': '6.11.2' } });
  fs.mkdirSync(path.join(child, 'node_modules'), { recursive: true });
  fs.symlinkSync(f.maplibre, path.join(child, 'node_modules/maplibre-gl'), directoryLinkType());
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.result.dependency_roots, 2);
  assert.deepEqual(actual.notices.packages.map(item => item.name), ['child', 'maplibre-gl']);
});

test('같은 dependency의 서로 다른 설치 버전은 각각 license를 보관한다', t => {
  // given
  const f = fixture(t, { dependencies: { parent: '1.0.0', shared: '1.0.0' } });
  const parent = addPackage(f.maplibre, 'parent', '1.0.0', { dependencies: { shared: '2.0.0' } });
  addPackage(f.maplibre, 'shared', '1.0.0');
  addPackage(parent, 'shared', '2.0.0');
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.result.dependency_roots, 4);
  assert.deepEqual(actual.notices.packages.filter(item => item.name === 'shared').map(item => item.version), ['1.0.0', '2.0.0']);
});

test('같은 name과 version의 서로 다른 license는 합치지 않는다', t => {
  // given
  const f = fixture(t, { dependencies: { parent: '1.0.0', shared: '1.0.0' } });
  const parent = addPackage(f.maplibre, 'parent', '1.0.0', { dependencies: { shared: '1.0.0' } });
  addPackage(f.maplibre, 'shared', '1.0.0', {}, 'license A');
  addPackage(parent, 'shared', '1.0.0', {}, 'license B');
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.notices.packages.filter(item => item.name === 'shared').length, 2);
  assert.notEqual(actual.notices.packages[2].identity_sha256, actual.notices.packages[3].identity_sha256);
});

test('같은 name과 version과 license의 중복 root는 notice를 재사용한다', t => {
  // given
  const f = fixture(t, { dependencies: { parent: '1.0.0', shared: '1.0.0' } });
  const parent = addPackage(f.maplibre, 'parent', '1.0.0', { dependencies: { shared: '1.0.0' } });
  addPackage(f.maplibre, 'shared', '1.0.0');
  addPackage(parent, 'shared', '1.0.0');
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.result.dependency_roots, 4);
  assert.equal(actual.notices.packages.filter(item => item.name === 'shared').length, 1);
});

test('required peer와 설치된 optional만 closure에 포함한다', t => {
  // given
  const f = fixture(t, { dependencies: { normal: '1' }, optionalDependencies: { present: '1', absent: '1' }, peerDependencies: { required: '1', optionalPeer: '1' }, peerDependenciesMeta: { optionalPeer: { optional: true } }, devDependencies: { devOnly: '1' } });
  for (const name of ['normal', 'present', 'required', 'devOnly']) addPackage(f.maplibre, name);
  // when
  const actual = packageAndAudit(f);
  // then
  assert.deepEqual(actual.notices.packages.map(item => item.name), ['maplibre-gl', 'normal', 'present', 'required']);
});

test('설치된 package directory link는 외부 link 없이 license만 복사한다', t => {
  // given
  const f = fixture(t, { dependencies: { linked: '1' } });
  const linked = addPackage(path.join(f.root, 'public-packages'), 'linked');
  fs.mkdirSync(path.join(f.maplibre, 'node_modules'));
  fs.symlinkSync(linked, path.join(f.maplibre, 'node_modules/linked'), directoryLinkType());
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(actual.result.dependency_roots, 2);
  assert.ok(Object.values(actual.files).every(entry => entry.mode === 0o100644));
  assert.equal(JSON.stringify(actual.notices).includes('public-packages'), false);
});

for (const [label, setup, code] of [
  ['필수 dependency 누락', f => json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '6.11.2', dependencies: { absent: '1' } }), 'DEPENDENCY_MISSING'],
  ['필수 peer 누락', f => json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '6.11.2', peerDependencies: { absent: '1' } }), 'DEPENDENCY_MISSING'],
  ['다른 installed name', f => { const pkg = addPackage(f.maplibre, 'target'); json(pkg, 'package.json', { name: 'wrong', version: '1.0.0' }); json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '6.11.2', dependencies: { target: '1' } }); }, 'DEPENDENCY_MISMATCH'],
  ['불법 dependency 경로', f => json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '6.11.2', dependencies: { '../outside': '1' } }), 'INVALID_PACKAGE'],
  ['불법 package version', f => json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '../escape' }), 'INVALID_PACKAGE'],
  ['다른 MapLibre pin', f => json(f.source, 'ui/package.json', { devDependencies: { 'maplibre-gl': '^6.11.2' } }), 'MAPLIBRE_VERSION_MISMATCH'],
  ['다른 installed MapLibre version', f => json(f.maplibre, 'package.json', { name: 'maplibre-gl', version: '6.11.3' }), 'MAPLIBRE_VERSION_MISMATCH'],
  ['symlink package JSON', f => { fs.renameSync(path.join(f.maplibre, 'package.json'), path.join(f.maplibre, 'manifest-copy')); fs.symlinkSync('manifest-copy', path.join(f.maplibre, 'package.json')); }, 'INVALID_FILE'],
  ['symlink license', f => { fs.renameSync(path.join(f.maplibre, 'LICENSE'), path.join(f.maplibre, 'copy')); fs.symlinkSync('copy', path.join(f.maplibre, 'LICENSE')); }, 'INVALID_FILE'],
  ['빈 license', f => write(f.maplibre, 'LICENSE', ''), 'PACKAGE_TOO_LARGE'],
]) {
  test(`${label} closure를 패키징하면 게시 전에 거부된다`, t => {
    // given
    const f = fixture(t);
    setup(f);
    // when
    const actual = attempt(f);
    // then
    assert.equal(actual.error, code);
    assert.equal(fs.existsSync(f.output), false);
    assert.deepEqual(actual.remaining, []);
  });
}

for (const version of ['1.0.0', '1.0.1']) {
  test(`murmurhash-js ${version} README 증거 hash가 다르면 license를 추정하지 않는다`, t => {
    // given
    const f = fixture(t, { dependencies: { 'murmurhash-js': version } });
    const pkg = addPackage(f.maplibre, 'murmurhash-js', version, {}, null);
    write(pkg, 'README.md', 'MIT라고 적힌 가짜 증거');
    // when
    const actual = attempt(f);
    // then
    assert.equal(actual.error, 'LICENSE_MISSING');
    assert.equal(fs.existsSync(f.output), false);
  });
}
