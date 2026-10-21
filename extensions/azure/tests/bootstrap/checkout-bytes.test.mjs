import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { readProvenance, verifyFiles } from '../../scripts/sdk.mjs';

const source = fileURLToPath(new URL('../../', import.meta.url));

function checkoutFixture(root, provenance) {
  const env = {
    PATH: process.env.PATH,
    HOME: root,
    USERPROFILE: root,
    APPDATA: root,
    LOCALAPPDATA: root,
    TMPDIR: root,
    TEMP: root,
    TMP: root,
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_ATTR_NOSYSTEM: '1',
    GIT_CONFIG_GLOBAL: path.join(root, 'empty-gitconfig'),
    ...(process.platform === 'win32' ? { SystemRoot: process.env.SystemRoot } : {}),
  };
  fs.writeFileSync(env.GIT_CONFIG_GLOBAL, '');
  const hooks = path.join(root, 'empty-hooks');
  fs.mkdirSync(hooks);
  const run = args => {
    const result = spawnSync('git', ['-c', 'core.autocrlf=true', '-c', `core.hooksPath=${hooks}`, ...args], {
      cwd: root,
      env,
      shell: false,
      encoding: 'utf8',
      timeout: 30_000,
    });
    if (result.error || result.status !== 0) throw new Error('FIXTURE_GIT_FAILED');
  };
  run(['init', '--quiet', '--initial-branch=main']);
  run(['add', '.gitattributes', 'build-support/sdk']);
  run(['-c', 'user.name=checkout-fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'fixture']);
  fs.rmSync(path.join(root, 'build-support/sdk'), { recursive: true });
  run(['checkout', 'HEAD', '--', 'build-support/sdk']);
  verifyFiles(path.join(root, 'build-support/sdk'), provenance.files);
  return { checked: provenance.files.length };
}

test('autocrlf가 켜진 Git checkout도 SDK 22개 파일의 원본 바이트를 보존한다', t => {
  // given
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-sdk-checkout-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const provenance = readProvenance(source);
  fs.copyFileSync(path.join(source, '.gitattributes'), path.join(root, '.gitattributes'));
  for (const record of provenance.files) {
    const target = path.join(root, 'build-support/sdk', record.path);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.copyFileSync(path.join(source, 'build-support/sdk', record.path), target);
  }
  fs.copyFileSync(path.join(source, 'build-support/sdk/provenance.json'), path.join(root, 'build-support/sdk/provenance.json'));
  // when
  const actual = checkoutFixture(root, provenance);
  // then
  assert.equal(actual.checked, 22);
});
