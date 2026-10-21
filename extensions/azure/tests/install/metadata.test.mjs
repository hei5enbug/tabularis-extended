import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { validatedManifest, requiredAsset, uiStyle } from '../../scripts/package/manifest.mjs';
import { ACCEPTED_MANIFEST, fixture, write } from './fixtures.mjs';

const source = fileURLToPath(new URL('../../', import.meta.url));
const readJSON = relative => JSON.parse(fs.readFileSync(path.join(source, relative), 'utf8'));

test('실제 Cosmos manifest는 완료된 기능과 고정 호스트 버전으로 패키징 검증을 통과한다', () => {
  // given
  const platform = 'darwin';
  // when
  const manifest = validatedManifest(source, platform);
  // then
  assert.equal(manifest.version, '0.1.0');
  assert.equal(manifest.min_runtime_version, '0.26.0');
  assert.equal(manifest.service_protocol, 1);
  assert.equal(manifest.connection_metadata, true);
  assert.equal(manifest.capabilities.documents_v1, true);
  assert.equal(manifest.capabilities.query_page_v1, true);
  assert.equal(manifest.capabilities.cancel_v1, true);
  assert.equal(manifest.capabilities.metadata_discovery, true);
  assert.deepEqual(manifest.paradigms, ['document']);
  assert.deepEqual(manifest.data_types, [{ name: 'JSON', category: 'json', requires_length: false, requires_precision: false, default_length: null }]);
  assert.doesNotMatch(manifest.description, /bootstrap|unavailable/i);
});

test('다섯 UI slot은 같은 Cosmos 드라이버와 번들 엔트리를 사용한다', () => {
  // given
  const platform = 'darwin';
  // when
  const manifest = validatedManifest(source, platform);
  // then
  assert.deepEqual(manifest.ui_extensions.map(extension => extension.slot).sort(), [
    'connection-modal.extra_fields', 'data-grid.toolbar.actions', 'row-edit-modal.footer.before', 'row-editor-sidebar.header.actions', 'settings.plugin.actions',
  ].sort());
  assert.ok(manifest.ui_extensions.every(extension => (extension.slot === 'settings.plugin.actions' ? !Object.hasOwn(extension, 'driver') : extension.driver === 'cosmos-nosql') && extension.module === 'ui/dist/index.js'));
  assert.deepEqual(manifest.ui_assets, [{ path: 'ui/dist/style.css', mime: 'text/css' }]);
});

for (const [name, mutate] of [
  ['누락 CSS 선언', value => { delete value.ui_assets; }],
  ['추가 CSS 선언', value => { value.ui_assets.push(value.ui_assets[0]); }],
  ['다른 CSS 경로', value => { value.ui_assets[0].path = 'other.css'; }],
  ['다른 CSS MIME', value => { value.ui_assets[0].mime = 'text/javascript'; }],
  ['추가 CSS 필드', value => { value.ui_assets[0].extra = true; }],
]) {
  test(`${name}은 패키징 manifest 검증에서 거부된다`, t => {
    // given
    const value = fixture(t);
    const manifest = structuredClone(ACCEPTED_MANIFEST);
    mutate(manifest);
    write(path.join(value.source, 'manifest.json'), manifest);
    // when
    const actual = (() => { try { validatedManifest(value.source, 'darwin'); return null; } catch (error) { return error.code; } })();
    // then
    assert.equal(actual, 'MANIFEST_NOT_READY');
  });
}

test('공개 연결 설정은 자격 증명 참조와 비밀 값을 입력받지 않는다', () => {
  // given
  const platform = 'darwin';
  // when
  const manifest = validatedManifest(source, platform);
  // then
  assert.deepEqual(manifest.settings, []);
  assert.ok(manifest.settings.every(setting => !/credential|password|secret|token|account_key/.test(setting.key)));

});

test('패키지와 live 명령은 각각의 실제 CLI 진입점에 연결한다', () => {
  // given
  const relative = 'package.json';
  // when
  const metadata = readJSON(relative);
  // then
  assert.equal(metadata.version, '0.1.0');
  assert.equal(metadata.license, 'Apache-2.0');
  assert.equal(metadata.packageManager, 'pnpm@10.30.3');
  assert.equal(metadata.scripts['package:plugin'], 'node scripts/package/cli.mjs');
  assert.equal(metadata.scripts['test:install'], 'node --test tests/install/*.test.mjs');
  assert.equal(metadata.scripts['test:live'], 'node scripts/live/cli.mjs');
  assert.ok(fs.statSync(path.join(source, 'scripts/package/cli.mjs')).isFile());
});

test('배포 라이선스는 원문 Apache 2.0 텍스트를 그대로 보존한다', () => {
  // given
  const relative = 'LICENSE';
  // when
  const license = fs.readFileSync(path.join(source, relative));
  // then
  assert.equal(createHash('sha256').update(license).digest('hex'), 'cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30');
  assert.match(license.toString('utf8'), /Apache License\s+Version 2\.0, January 2004/);
});

function assets() {
  return [requiredAsset(source, 'dist/index.js'), requiredAsset(source, 'ui/dist/index.js'), uiStyle(source)]
    .map(file => ({ relative: path.relative(source, file), bytes: fs.statSync(file).size }));
}

test('현재 빌드 산출물에 패키징용 엔트리와 UI 및 CSS가 있다', () => {
  // given
  const expected = [path.join('dist', 'index.js'), path.join('ui', 'dist', 'index.js'), path.join('ui', 'dist', 'style.css')];
  // when
  const available = assets();
  // then
  assert.deepEqual(available.map(asset => asset.relative), expected);
  assert.ok(available.every(asset => asset.bytes > 0));
});
