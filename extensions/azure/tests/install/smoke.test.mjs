import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { packageBundle } from '../../scripts/package/index.mjs';
import { prepareInstallSmoke, runInstallSmoke } from './harness.mjs';
import { actualOptions, fixture, python } from './fixtures.mjs';
import { spawnSync } from 'node:child_process';
import { selectTargets } from './platform-options.mjs';

function runPackageCLI(options) {
  const args = ['scripts/package/cli.mjs', '--platform', options.platform, '--arch', options.arch, '--source', options.source, '--launcher', options.launcher, '--runtime-archive', options.runtimeArchive, '--output', options.output];
  if (options.tar) args.push('--tar', options.tar);
  const result = spawnSync(process.execPath, args, { cwd: path.resolve(import.meta.dirname, '../..'), shell: false, encoding: 'utf8', timeout: 60000 });
  if (result.status !== 0 || result.stderr) throw new Error('synthetic CLI packaging failed');
  return JSON.parse(result.stdout);
}

async function installedSmoke(options, input, arch) {
  const packaged = arch === 'arm64' ? runPackageCLI(options) : packageBundle(options);
  const prepared = prepareInstallSmoke({ bundle: options.output, platform: options.platform, arch, python: options.python });
  try {
    const extracted = python(prepared.extraction.args);
    if (extracted.status !== 0) return { extracted: extracted.status };
    const config = prepared.launch();
    const runtimeCopy = fs.statSync(config.runtime).isFile();
    const result = await runInstallSmoke(prepared, { input, timeoutMs: 60000 });
    const firstLine = result.stdout.indexOf('\n');
    return { packaged, extracted: 0, runtimeCopy, path: config.env.PATH, profile: config.env.HOME, root: prepared.root, result, metadata: result.code === 0 ? JSON.parse(result.stdout.slice(0, firstLine)) : null, echoed: result.stdout.slice(firstLine + 1), symlink: fs.lstatSync(path.join(prepared.installed, 'node_modules/@tabularis/service-contracts')).isSymbolicLink() };
  } finally { prepared.dispose(); }
}

for (const { arch } of selectTargets(process.platform, process.arch)) {
  test(`${arch} 번들 런처가 Node 없는 PATH와 새 프로필에서 합성 드라이버를 실행한다`, { timeout: 120000 }, async t => {
    // given
    const value = fixture(t);
    const options = actualOptions(value, arch);
    const input = `${' {"원문":"한글 공백", "value":null}\n'.repeat(25000)}EOF\n`;
    // when
    const result = await installedSmoke(options, input, arch);
    // then
    assert.equal(result.extracted, 0);
    assert.equal(result.runtimeCopy, true);
    assert.equal(result.path, '');
    assert.ok(result.profile.startsWith(result.root));
    assert.equal(result.result.code, 0);
    assert.equal(result.result.signal, null);
    assert.equal(result.result.stderr, '');
    assert.deepEqual(result.metadata, { cosmos: '4.10.1', schema: 'materialized contract', first: '1.0.0:1.0.0', second: '2.0.0', cycle: 'bounded cycle', helper: 'relative import works', argv: [], node: 'v24.21.0', envAbsent: true });
    assert.equal(result.echoed, input);
    assert.equal(result.symlink, false);
    assert.equal(fs.existsSync(result.root), false);
    assert.ok(result.packaged.bytes > 1000000);
  });
}

test('설치 harness는 호출자가 명시한 bundle과 정리 가능한 새 프로필을 반환한다', t => {
  // given
  const value = fixture(t);
  // when
  const prepared = prepareScoped(t, value.options.output);
  // then
  assert.ok(path.isAbsolute(prepared.extraction.executable));
  assert.equal(prepared.extraction.args.at(-2), value.options.output);
  assert.equal(prepared.extraction.args.at(-1), prepared.installed);
  assert.ok(prepared.profile.startsWith(prepared.root));
  assert.equal(fs.lstatSync(prepared.root).isDirectory(), true);
  assert.equal(fs.lstatSync(prepared.root).isSymbolicLink(), false);
  if (process.platform !== 'win32') assert.equal(fs.statSync(prepared.root).mode & 0o777, 0o700);
  assert.equal(typeof prepared.dispose, 'function');
});

function prepareScoped(t, bundle) {
  const prepared = prepareInstallSmoke({ bundle });
  t.after(() => prepared.dispose());
  return prepared;
}
