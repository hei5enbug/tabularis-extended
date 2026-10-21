import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from 'node:test';
import { sha256 } from '../../scripts/package/files.mjs';
import { validatedManifest } from '../../scripts/package/manifest.mjs';
import { fixture, json, MANIFEST, packageAndAudit, decode, attempt, addPackage } from './fixtures.mjs';

test('원본 공간 asset을 패키징하면 고정 항목과 전체 SHA가 보존된다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = packageAndAudit(f);
  // then
  assert.deepEqual(Object.keys(actual.files), ['.tabularium', 'LICENSE', 'licenses/packages/maplibre-gl/6.11.2/' + actual.notices.packages[0].identity_sha256 + '/LICENSE', 'licenses/third-party-notices.json', 'release.json', 'ui/dist/index.js', 'ui/dist/maplibre-worker.js', 'ui/dist/style.css']);
  assert.equal(actual.result.files, 8);
  assert.equal(actual.result.dependency_roots, 1);
  assert.equal(actual.release.platform, 'any');
  assert.equal(actual.release.arch, 'any');
  assert.equal(actual.release.maplibre_version, '6.11.2');
  assert.equal(actual.release.min_runtime_version, '0.26.0');
  assert.equal(actual.release.service_protocol, 1);
  assert.equal(actual.release.manifest_version, '0.1.0');
  assert.equal(JSON.parse(decode(actual.files['.tabularium'])).kind, 'driver');
  assert.deepEqual(JSON.parse(decode(actual.files['.tabularium'])).ui_assets, [{ path: 'ui/dist/style.css', mime: 'text/css' }, { path: 'ui/dist/maplibre-worker.js', mime: 'text/javascript' }]);
  assert.equal(Object.hasOwn(JSON.parse(decode(actual.files['.tabularium'])), 'capabilities'), false);
  assert.deepEqual(decode(actual.files['.tabularium']), fs.readFileSync(path.join(f.source, 'manifest.json')));
  assert.deepEqual(decode(actual.files.LICENSE), fs.readFileSync(path.join(f.source, 'LICENSE')));
  assert.ok(actual.release.files.every(entry => actual.files[entry.path].bytes === entry.bytes && actual.files[entry.path].sha256 === entry.sha256));
  assert.equal(actual.release.files.some(entry => entry.path === 'release.json'), false);
  assert.ok(Object.entries(actual.files).filter(([name]) => name.startsWith('ui/')).every(([name, value]) => decode(value).equals(fs.readFileSync(path.join(f.source, name)))));
  assert.ok(Object.values(actual.files).every(value => value.mode === 0o100644 && value.method === 8 && value.timestamp.join(',') === '1980,1,1,0,0,0'));
  assert.equal(fs.readFileSync(`${f.output}.sha256`, 'utf8'), `${actual.result.sha256}\n`);
  assert.equal(sha256(fs.readFileSync(f.output)), actual.result.sha256);
  assert.equal(fs.lstatSync(f.output).isFile(), true);
  assert.equal(fs.lstatSync(f.output).isSymbolicLink(), false);
  if (process.platform !== 'win32') assert.equal(fs.statSync(f.output).mode & 0o777, 0o600);
  assert.equal(fs.readdirSync(f.root).some(name => name.startsWith('.tabularis-spatial-package-')), false);
});

function deterministic(f) {
  const first = attempt(f).result;
  const second = attempt(f, { output: path.join(f.root, 'second.zip') }).result;
  return { first, second };
}

test('같은 원본으로 두 번 패키징하면 ZIP bytes와 SHA가 동일하다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = deterministic(f);
  // then
  assert.equal(actual.first.sha256, actual.second.sha256);
  assert.deepEqual(fs.readFileSync(f.output), fs.readFileSync(path.join(f.root, 'second.zip')));
});

test('추가 비밀 파일과 runtime 소스가 있어도 고정 공개 asset만 포함된다', t => {
  // given
  const f = fixture(t);
  // when
  const actual = packageAndAudit(f);
  // then
  assert.equal(Object.keys(actual.files).some(name => /secret|node_modules|\.rs$|node\.exe|runtime\/|package\.json$/.test(name)), false);
  assert.equal(JSON.stringify(actual.release).includes(f.root), false);
  assert.equal(JSON.stringify(actual.notices).includes(f.root), false);
  assert.equal(Object.values(actual.files).some(entry => decode(entry).includes('source-secret-canary')), false);
});

for (const [name, mutate] of [
  ['빈 UI 등록', value => { value.ui_extensions = []; }],
  ['중복 toolbar', value => { value.ui_extensions[1] = value.ui_extensions[0]; }],
  ['임의 slot', value => { value.ui_extensions[1].slot = 'sidebar.footer'; }],
  ['잘못된 driver', value => { value.ui_extensions[0].driver = 'other'; }],
  ['전역 renderer driver', value => { value.ui_extensions[1].driver = 'postgresql'; }],
  ['지원하지 않는 패키지 유형', value => { value.kind = 'extension'; }],
  ['다른 module', value => { value.ui_extensions[0].module = 'other.js'; }],
  ['추가 extension field', value => { value.ui_extensions[1].extra = true; }],
  ['낮은 host floor', value => { value.min_runtime_version = '0.25.0'; }],
  ['driver capability', value => { value.capabilities = { spatial_v1: true }; }],
  ['DDL capability', value => { value.capabilities = { manage_tables: true }; }],
  ['누락 protocol', value => { delete value.service_protocol; }],
  ['다른 required protocol', value => { value.required_service_protocol = 2; }],
  ['누락 asset 선언', value => { delete value.ui_assets; }],
  ['중복 asset 선언', value => { value.ui_assets[1] = value.ui_assets[0]; }],
  ['임의 asset 경로', value => { value.ui_assets[0].path = 'other.css'; }],
  ['다른 asset MIME', value => { value.ui_assets[1].mime = 'application/json'; }],
  ['추가 asset field', value => { value.ui_assets[0].extra = true; }],
  ['driver executable', value => { value.executable = 'driver'; }],
  ['engine field', value => { value.engine = 'spatial'; }],
  ['잘못된 protocol', value => { value.service_protocol = 2; }],
  ['다른 id', value => { value.id = 'other'; }],
  ['다른 version', value => { value.version = '0.2.0'; }],
  ['bootstrap 설명', value => { value.description = 'bootstrap unavailable'; }],
]) {
  test(`${name} manifest를 검증하면 배포 준비 오류가 반환된다`, () => {
    // given
    const value = structuredClone(MANIFEST);
    mutate(value);
    // when
    const actual = (() => { try { validatedManifest(value); return null; } catch (error) { return error.code; } })();
    // then
    assert.equal(actual, 'MANIFEST_NOT_READY');
  });
}

test('누락 license가 있는 dependency를 패키징하면 추정 없이 거부된다', t => {
  // given
  const f = fixture(t, { dependencies: { missing: '1.0.0' } });
  addPackage(f.maplibre, 'missing', '1.0.0', {}, null);
  // when
  const actual = attempt(f);
  // then
  assert.equal(actual.error, 'LICENSE_MISSING');
  assert.equal(fs.existsSync(f.output), false);
  assert.deepEqual(actual.remaining, []);
});

test('license에 한글과 원문 저작권이 있으면 bytes를 변경하지 않는다', t => {
  // given
  const f = fixture(t);
  const original = Buffer.from('Copyright 원문\r\n전체 조건\0유지\n');
  fs.writeFileSync(path.join(f.maplibre, 'LICENSE'), original);
  // when
  const actual = packageAndAudit(f);
  // then
  assert.deepEqual(decode(actual.files[actual.notices.packages[0].files[0].path]), original);
  assert.equal(actual.notices.packages[0].files[0].sha256, sha256(original));
});
