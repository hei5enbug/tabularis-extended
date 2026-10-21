import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { Stage, sha256 } from '../../scripts/package/files.mjs';
import { boundedLimits } from '../../scripts/package/errors.mjs';
import { materializeProductionGraph } from '../../scripts/package/graph.mjs';
import { privateStageDirectory } from '../../scripts/package/embedded-licenses.mjs';

const packageSource = fileURLToPath(new URL('../../scripts/package/', import.meta.url));
const packages = [['priorityqueuejs', '2.0.0'], ['semaphore', '1.1.0']];

async function fixture(t, name = 'priorityqueuejs', version = '2.0.0', limits = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-embedded-license-'));
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const module = path.join(root, 'module');
  const installed = path.join(root, 'installed');
  const stageRoot = path.join(root, 'stage');
  for (const folder of [module, installed, stageRoot]) fs.mkdirSync(folder, { mode: 0o700 });
  for (const file of ['embedded-licenses.mjs', 'errors.mjs', 'files.mjs']) fs.copyFileSync(path.join(packageSource, file), path.join(module, file));
  fs.cpSync(path.join(packageSource, 'licenses'), path.join(module, 'licenses'), { recursive: true });
  const vendor = path.join(module, 'licenses', `${name}-${version}`);
  fs.copyFileSync(path.join(vendor, 'UPSTREAM-PACKAGE.json'), path.join(installed, 'package.json'));
  fs.copyFileSync(path.join(vendor, 'UPSTREAM-README.md'), path.join(installed, 'README.md'));
  const manifest = JSON.parse(fs.readFileSync(path.join(installed, 'package.json'), 'utf8'));
  const { copyEmbeddedLicense } = await import(pathToFileURL(path.join(module, 'embedded-licenses.mjs')).href);
  return { root, installed, vendor, module, manifest, stage: new Stage(stageRoot, boundedLimits(limits)), notices: new Map(), copyEmbeddedLicense };
}

function run(value) {
  try {
    value.copyEmbeddedLicense(value.stage, value.installed, value.manifest, value.notices);
    return { error: null, files: [...value.stage.entries], notices: [...value.notices.values()] };
  } catch (error) { return { error: error.code, files: [...value.stage.entries], notices: [...value.notices.values()] }; }
}

function changedByte(file) { const data = fs.readFileSync(file); data[0] ^= 1; fs.writeFileSync(file, data); }

for (const [name, version] of packages) {
  test(`${name} 고정 버전은 원문 두 파일과 별도 MIT 템플릿 및 출처를 보존한다`, async t => {
    // given
    const value = await fixture(t, name, version);
    // when
    const actual = run(value);
    // then
    assert.equal(actual.error, null);
    assert.equal(actual.files.length, 4);
    assert.equal(actual.notices.length, 1);
    assert.deepEqual(Object.keys(actual.notices[0]).sort(), ['name', 'version', 'sha256', 'license', 'license_source', 'template_source'].sort());
    assert.equal(actual.notices[0].name, name);
    assert.equal(actual.notices[0].version, version);
    assert.equal(actual.notices[0].license, 'MIT');
    assert.equal(actual.notices[0].license_source, 'embedded_readme');
    assert.equal(actual.notices[0].template_source, 'https://raw.githubusercontent.com/spdx/license-list-data/main/text/MIT.txt');
    assert.ok(actual.files.every(([relative]) => relative.startsWith(`licenses/packages/${name}/${version}/${actual.notices[0].sha256}/`)));
    assert.ok(actual.files.every(([relative, file]) => fs.readFileSync(file.path).equals(fs.readFileSync(path.join(value.vendor, path.basename(relative))))));
    assert.ok(actual.files.every(([, file]) => process.platform === 'win32' ? fs.lstatSync(file.path).isFile() : (fs.statSync(file.path).mode & 0o777) === 0o600));
    assert.ok(actual.files.every(([, file]) => !fs.lstatSync(file.path).isSymbolicLink()));
    assert.match(fs.readFileSync(actual.files.find(([relative]) => relative.endsWith('/MIT-TEMPLATE.txt'))[1].path, 'utf8'), /Copyright \(c\) <year> <copyright holders>/);
  });
}

for (const [title, mutate] of [
  ['알 수 없는 이름', value => { value.manifest.name = 'other'; }],
  ['다른 버전', value => { value.manifest.version = '2.0.1'; }],
  ['설치 package 원문 변조', value => changedByte(path.join(value.installed, 'package.json'))],
  ['설치 README 원문 변조', value => changedByte(path.join(value.installed, 'README.md'))],
  ['보관 package 원문 변조', value => changedByte(path.join(value.vendor, 'UPSTREAM-PACKAGE.json'))],
  ['보관 README 원문 변조', value => changedByte(path.join(value.vendor, 'UPSTREAM-README.md'))],
  ['MIT 템플릿 변조', value => changedByte(path.join(value.vendor, 'MIT-TEMPLATE.txt'))],
  ['출처 원문 변조', value => changedByte(path.join(value.vendor, 'provenance.json'))],
  ['출처에 알 수 없는 필드 추가', value => { const file = path.join(value.vendor, 'provenance.json'); const data = JSON.parse(fs.readFileSync(file)); data.extra = true; fs.writeFileSync(file, JSON.stringify(data)); }],
  ['설치 README 누락', value => fs.unlinkSync(path.join(value.installed, 'README.md'))],
  ['보관 README 누락', value => fs.unlinkSync(path.join(value.vendor, 'UPSTREAM-README.md'))],
  ['출처 파일 누락', value => fs.unlinkSync(path.join(value.vendor, 'provenance.json'))],
  ['MIT 템플릿 누락', value => fs.unlinkSync(path.join(value.vendor, 'MIT-TEMPLATE.txt'))],
  ['상한을 넘는 설치 README', value => fs.truncateSync(path.join(value.installed, 'README.md'), 64 * 1024 * 1024)],
  ['상한을 넘는 보관 README', value => fs.truncateSync(path.join(value.vendor, 'UPSTREAM-README.md'), 64 * 1024 * 1024)],
  ['설치 README 심볼릭 링크', value => { const file = path.join(value.installed, 'README.md'); fs.unlinkSync(file); fs.symlinkSync(path.join(value.vendor, 'UPSTREAM-README.md'), file); }],
  ['보관 템플릿 심볼릭 링크', value => { const file = path.join(value.vendor, 'MIT-TEMPLATE.txt'); fs.renameSync(file, `${file}.original`); fs.symlinkSync(`${file}.original`, file); }],
  ['보관 디렉터리 심볼릭 링크', value => { fs.renameSync(value.vendor, `${value.vendor}.original`); fs.symlinkSync(`${value.vendor}.original`, value.vendor, process.platform === 'win32' ? 'junction' : 'dir'); }],
  ...(process.platform === 'win32' ? [] : [['공개 staging 디렉터리', value => fs.chmodSync(value.stage.root, 0o755)]]),
  ['staging 디렉터리 심볼릭 링크', value => { const folder = value.stage.root; fs.renameSync(folder, `${folder}.original`); fs.symlinkSync(`${folder}.original`, folder, process.platform === 'win32' ? 'junction' : 'dir'); }],
]) {
  test(`${title} 조건에서는 보관 라이선스를 허용하지 않는다`, async t => {
    // given
    const value = await fixture(t);
    mutate(value);
    // when
    const actual = run(value);
    // then
    assert.equal(actual.error, 'LICENSE_MISSING');
    assert.equal(actual.files.length, 0);
    assert.equal(actual.notices.length, 0);
  });
}

for (const [title, limits] of [['파일 수 예산', { files: 3 }], ['단일 파일 예산', { fileBytes: 1000 }], ['전체 바이트 예산', { bytes: 4000 }]]) {
  test(`${title}이 부족하면 원문 파일을 staging에 추가하지 않는다`, async t => {
    // given
    const value = await fixture(t, 'priorityqueuejs', '2.0.0', limits);
    // when
    const actual = run(value);
    // then
    assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
    assert.equal(actual.files.length, 0);
    assert.equal(actual.notices.length, 0);
  });
}

test('이미 사용한 staging 바이트를 포함해 남은 예산을 검사한다', async t => {
  // given
  const value = await fixture(t, 'priorityqueuejs', '2.0.0', { bytes: 5500 });
  value.stage.add('existing', Buffer.alloc(1000));
  // when
  const actual = run(value);
  // then
  assert.equal(actual.error, 'PACKAGE_TOO_LARGE');
  assert.equal(actual.files.length, 1);
  assert.equal(actual.notices.length, 0);
});

test('같은 원문 증거를 다시 검사해도 공지와 파일을 중복 추가하지 않는다', async t => {
  // given
  const value = await fixture(t);
  run(value);
  // when
  const actual = run(value);
  // then
  assert.equal(actual.error, null);
  assert.equal(actual.files.length, 4);
  assert.equal(actual.notices.length, 1);
});

test('기존 라이선스 파일이 있으면 고정 버전 예외 없이 일반 경로를 사용한다', async t => {
  // given
  const value = await fixture(t);
  const source = path.join(value.root, 'source');
  const installed = path.join(source, 'node_modules', 'priorityqueuejs');
  fs.mkdirSync(installed, { recursive: true });
  fs.writeFileSync(path.join(source, 'package.json'), JSON.stringify({ name: 'fixture', version: '1', dependencies: { priorityqueuejs: '9' } }));
  fs.writeFileSync(path.join(installed, 'package.json'), JSON.stringify({ name: 'priorityqueuejs', version: '9.0.0', main: 'index.js' }));
  fs.writeFileSync(path.join(installed, 'index.js'), 'module.exports = {};');
  fs.writeFileSync(path.join(installed, 'LICENSE'), 'Synthetic original license.\n');
  // when
  const actual = materializeProductionGraph(value.stage, source);
  // then
  assert.deepEqual(actual.notices.map(item => [item.name, item.version]), [['priorityqueuejs', '9.0.0']]);
  assert.deepEqual(Object.keys(actual.notices[0]).sort(), ['name', 'version', 'sha256'].sort());
  assert.ok([...value.stage.entries.keys()].some(relative => relative.endsWith('/LICENSE')));
  assert.ok(![...value.stage.entries.keys()].some(relative => relative.endsWith('/MIT-TEMPLATE.txt')));
});

test('보관한 원문은 고정 package와 README 해시를 유지한다', () => {
  // given
  const file = path.join(packageSource, 'licenses', 'priorityqueuejs-2.0.0', 'UPSTREAM-README.md');
  // when
  const hash = sha256(fs.readFileSync(file));
  // then
  assert.equal(hash, 'd8b642d2eec713715981b38b7bf0fe7686f7e182464747f959d06352d47a05da');
});

for (const [title, platform, mode, directory, symbolic, expected] of [
  ['Windows 디렉터리는 Unix 권한 비트로 거부하지 않는다', 'win32', 0o777, true, false, true],
  ['macOS 공개 디렉터리는 거부한다', 'darwin', 0o755, true, false, false],
  ['Linux 공개 디렉터리는 거부한다', 'linux', 0o755, true, false, false],
  ['macOS 비공개 디렉터리는 허용한다', 'darwin', 0o700, true, false, true],
  ['Windows 심볼릭 링크는 거부한다', 'win32', 0o777, true, true, false],
  ['macOS 심볼릭 링크는 거부한다', 'darwin', 0o700, true, true, false],
  ['Windows 파일은 staging 디렉터리로 허용하지 않는다', 'win32', 0o777, false, false, false],
]) {
  test(title, () => {
    // given
    const stat = { mode, isDirectory: () => directory, isSymbolicLink: () => symbolic };
    // when
    const actual = privateStageDirectory(stat, platform);
    // then
    assert.equal(actual, expected);
  });
}
