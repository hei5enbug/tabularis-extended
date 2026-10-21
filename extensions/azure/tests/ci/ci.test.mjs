import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import yamlParser from 'js-yaml';
import { archiveName, ensureArchive } from '../../scripts/ci/runtime.mjs';
import { prepareCI, prepareInputs } from '../../scripts/ci/prepare.mjs';
import { collectArtifacts } from '../../scripts/ci/collect.mjs';
import { NODE_PINS } from '../../scripts/package/pins.mjs';

const source = fileURLToPath(new URL('../../', import.meta.url));
function fixture(t) { const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-x1-c-ci-')); t.after(() => fs.rmSync(root, { recursive: true, force: true })); return root; }
async function observe(action) { try { return { value: await action() }; } catch (error) { return { code: error.code ?? error.message }; } }

test('CI는 수동 실행으로만 고정된 세 native x64 runner를 사용한다', () => {
  // given
  const yaml = fs.readFileSync(path.join(source, '.github/workflows/bootstrap.yml'), 'utf8');
  // when
  const actual = yamlParser.load(yaml);
  // then
  assert.deepEqual(Object.keys(actual.on), ['workflow_dispatch']);
  assert.deepEqual(actual.permissions, { contents: 'read' });
  assert.deepEqual(actual.jobs.native.strategy.matrix.os, ['ubuntu-24.04', 'windows-2022', 'macos-15-intel']);
  assert.equal(actual.jobs.native.steps.filter(step => step.uses?.startsWith('actions/checkout@')).length, 1);
  assert.equal(actual.jobs.native.steps[0].with.path, 'tabularis-azure');
  assert.ok(actual.jobs.native.steps.filter(step => step.uses).every(step => /@[a-f0-9]{40}$/.test(step.uses)));
  assert.equal(yaml.includes('host_repository'), false);
  assert.equal(yaml.includes('test:live'), false);
  assert.ok(yaml.includes('pnpm test:install'));
});
test('package scripts는 SDK workspace 빌드와 검사를 연결하고 live 진입점을 유지한다', () => {
  // given
  const metadata = JSON.parse(fs.readFileSync(path.join(source, 'package.json'), 'utf8'));
  const ui = JSON.parse(fs.readFileSync(path.join(source, 'ui/package.json'), 'utf8'));
  // when
  const actual = {
    bootstrap: metadata.scripts.bootstrap,
    verifySdk: metadata.scripts['verify:sdk'],
    buildSdk: metadata.scripts['build:sdk'],
    buildDriver: metadata.scripts['build:driver'],
    testSdk: metadata.scripts['test:sdk'],
    testBootstrap: metadata.scripts['test:bootstrap'],
    testLive: metadata.scripts['test:live'],
    serviceContracts: metadata.dependencies['@tabularis/service-contracts'],
    uiPluginApi: ui.devDependencies['@tabularis/plugin-api'],
    uiServiceContracts: ui.devDependencies['@tabularis/service-contracts'],
    yamlPin: metadata.devDependencies['js-yaml'],
  };
  // then
  assert.equal(actual.bootstrap, 'node scripts/bootstrap/cli.mjs');
  assert.equal(actual.verifySdk, 'node scripts/sdk.mjs');
  assert.match(actual.buildSdk, /verify:sdk/);
  assert.match(actual.buildSdk, /@tabularis\/service-contracts build/);
  assert.match(actual.buildSdk, /@tabularis\/plugin-api build/);
  assert.equal(actual.buildDriver, 'tsc');
  assert.equal(actual.testSdk, 'corepack pnpm build:sdk && node --test tests/sdk.test.mjs');
  assert.equal(actual.testBootstrap, 'node --test tests/bootstrap/*.test.mjs tests/ci/*.test.mjs');
  assert.equal(actual.testLive, 'node scripts/live/cli.mjs');
  assert.equal(actual.serviceContracts, 'workspace:*');
  assert.equal(actual.uiPluginApi, 'workspace:*');
  assert.equal(actual.uiServiceContracts, 'workspace:*');
  assert.equal(actual.yamlPin, '4.1.1');
  assert.equal(metadata.dependencies['@azure/cosmos'], '4.10.1');
});
test('다섯 archive 이름은 기존 플랫폼별 pin과 같은 Node 버전을 가리킨다', () => {
  // given
  const keys = Object.keys(NODE_PINS).sort();
  // when
  const actual = keys.map(archiveName);
  // then
  assert.deepEqual(actual, ['node-v24.21.0-darwin-arm64.tar.gz', 'node-v24.21.0-darwin-x64.tar.gz', 'node-v24.21.0-linux-arm64.tar.gz', 'node-v24.21.0-linux-x64.tar.gz', 'node-v24.21.0-win-x64.zip']);
});
test('손상된 cache는 온라인 교체 없이 SHA 불일치로 거부한다', async t => {
  // given
  const root = fixture(t);
  fs.writeFileSync(path.join(root, archiveName('linux-x64')), 'tampered');
  let fetches = 0;
  // when
  const actual = await observe(() => ensureArchive(root, 'linux-x64', { fetcher: async () => { fetches += 1; } }));
  // then
  assert.equal(actual.code, 'ARCHIVE_HASH_MISMATCH');
  assert.equal(fetches, 0);
  assert.equal(fs.readFileSync(path.join(root, archiveName('linux-x64')), 'utf8'), 'tampered');
});
test('다운로드 hash가 다르면 발행하지 않고 자신의 임시 파일을 정리한다', async t => {
  // given
  const root = fixture(t);
  const urls = [];
  const fetcher = async url => { urls.push(url); return new Response('synthetic-invalid-archive'); };
  // when
  const actual = await observe(() => ensureArchive(root, 'win32-x64', { fetcher }));
  // then
  assert.equal(actual.code, 'ARCHIVE_HASH_MISMATCH');
  assert.deepEqual(fs.readdirSync(root), []);
  assert.deepEqual(urls, ['https://nodejs.org/dist/v24.21.0/node-v24.21.0-win-x64.zip']);
});
test('합성 CI 입력은 실제 host 파일 경로와 x64 archive 경로를 install 환경에 전달한다', async t => {
    // given
    const root = fixture(t);
    const launcher = path.join(root, 'launcher'); const python = path.join(root, 'python'); const envFile = path.join(root, 'environment');
    fs.writeFileSync(launcher, 'synthetic'); fs.writeFileSync(python, 'synthetic');
    const expectedPython = fs.realpathSync(python);
    const values = { platform: process.platform, arch: 'x64', launcher, python, cache: root, envFile, systemRoot: process.platform === 'win32' ? process.env.SystemRoot : undefined };
    const calls = [];
    const archives = async cache => { calls.push(cache); return Object.fromEntries(Object.keys(NODE_PINS).map(key => [key, path.join(cache, archiveName(key))])); };
    // when
    const actual = await prepareCI(values, { archives, execute: () => ({ status: 0, stdout: `${python}\n` }) });
    // then
    assert.equal(actual.TABULARIS_C3B_TEST_LAUNCHER, launcher);
    assert.equal(actual.TABULARIS_C3B_TEST_NODE_ARCHIVE, path.join(root, archiveName(`${process.platform}-x64`)));
    assert.equal(actual.TABULARIS_C3B_TEST_PYTHON, expectedPython);
    assert.equal(actual.TABULARIS_C3B_TEST_NODE_CACHE, root);
    assert.deepEqual(calls, [root]);
    assert.equal(fs.readFileSync(envFile, 'utf8').split('\n').filter(Boolean).length, 4);
});
for (const platform of ['darwin', 'linux', 'win32']) {
  test(`${platform} CI 입력은 native 절대 경로 형태를 검증한다`, () => {
    // given
    const base = platform === 'win32' ? 'C:\\owned' : '/owned';
    const join = platform === 'win32' ? path.win32.join : path.posix.join;
    const values = { platform, arch: 'x64', launcher: join(base, 'launcher'), python: join(base, 'python'), cache: join(base, 'cache'), envFile: join(base, 'env'), systemRoot: 'C:\\Windows' };
    // when
    const actual = prepareInputs(values);
    // then
    assert.equal(actual.platform, platform);
    assert.equal(actual.arch, 'x64');
    assert.equal(actual.launcher, values.launcher);
  });
}
for (const changed of [{ arch: 'arm64' }, { launcher: 'relative' }, { platform: 'unsupported' }]) {
  test(`지원하지 않는 CI 입력 ${JSON.stringify(changed)}은 실행 전에 실패한다`, async () => {
    // given
    const values = { platform: 'linux', arch: 'x64', launcher: '/owned/launcher', python: '/owned/python', cache: '/owned/cache', envFile: '/owned/env', ...changed };
    // when
    const actual = await observe(() => prepareInputs(values));
    // then
    assert.ok(['CAPABILITY_UNAVAILABLE', 'INVALID_PATH'].includes(actual.code));
  });
}
test('native artifact가 관찰되지 않으면 업로드 보고서를 성공으로 만들지 않는다', async t => {
  // given
  const root = fixture(t);
  const log = path.join(root, 'install.log'); const output = path.join(root, 'artifacts');
  fs.writeFileSync(log, 'synthetic tests without a native artifact');
  // when
  const actual = await observe(() => collectArtifacts(log, output));
  // then
  assert.equal(actual.code, 'MISSING_NATIVE_ARTIFACT');
  assert.equal(fs.existsSync(output), false);
});
