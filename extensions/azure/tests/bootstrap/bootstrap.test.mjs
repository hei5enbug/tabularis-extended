import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { bootstrap, commandFor, NODE_VERSION, PNPM_VERSION } from '../../scripts/sdk-bootstrap.mjs';
import { SDK_REVISION, readProvenance, verifySdk } from '../../scripts/sdk.mjs';

const repository = fileURLToPath(new URL('../../', import.meta.url));

function fixture(t, name = '코스모스 checkout with spaces') {
  const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-sdk 테스트 '));
  t.after(() => fs.rmSync(parent, { recursive: true, force: true }));
  const source = path.join(parent, name);
  fs.mkdirSync(path.join(source, 'build-support'), { recursive: true });
  fs.cpSync(path.join(repository, 'build-support/sdk'), path.join(source, 'build-support/sdk'), { recursive: true });
  fs.writeFileSync(path.join(source, 'package.json'), JSON.stringify({ name: '@tabularis/azure' }));
  return { parent, source, lock: path.join(source, '.sdk-bootstrap.lock') };
}

function observe(action) {
  try { return { value: action() }; } catch (error) { return { code: error.code ?? error.message }; }
}

function pnpmRun(calls) {
  return (args, cwd) => {
    calls.push({ args: [...args], cwd });
    return args[0] === '--version' ? PNPM_VERSION : undefined;
  };
}

test('SDK 출처 원장은 고정 revision의 22개 파일을 검증한다', () => {
  // given
  const provenance = readProvenance(repository);
  // when
  const actual = verifySdk(repository);
  // then
  assert.deepEqual(actual.provenance, provenance);
  assert.equal(provenance.canonical_host_commit, SDK_REVISION);
  assert.equal(provenance.files.length, 22);
  assert.equal(path.basename(actual.directory), 'sdk');
});

test('한글과 공백이 포함된 checkout 안에서 root만 설치하고 빌드한다', t => {
  // given
  const f = fixture(t);
  const calls = [];
  // when
  const actual = bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: pnpmRun(calls), env: {} });
  // then
  assert.deepEqual(actual, { sdk_revision: SDK_REVISION, sdk: path.join(f.source, 'build-support/sdk'), node: NODE_VERSION, pnpm: PNPM_VERSION });
  assert.deepEqual(calls.map(call => call.args), [
    ['--version'],
    ['install', '--frozen-lockfile', '--ignore-scripts'],
    ['--filter', '@tabularis/service-contracts', 'build'],
    ['--filter', '@tabularis/plugin-api', 'build'],
    ['build:driver'],
    ['--dir', 'ui', 'build'],
  ]);
  assert.ok(calls.every(call => call.cwd === f.source));
  assert.equal(fs.existsSync(f.lock), false);
  assert.equal(fs.existsSync(path.join(f.parent, 'tabularis-host')), false);
});

test('원장 파일이 손상되면 설치를 시작하지 않는다', t => {
  // given
  const f = fixture(t);
  const provenance = readProvenance(f.source);
  const target = path.join(f.source, 'build-support/sdk', provenance.files[0].path);
  fs.appendFileSync(target, 'tampered');
  const calls = [];
  // when
  const actual = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: pnpmRun(calls), env: {} }));
  // then
  assert.equal(actual.code, 'SOURCE_MISMATCH');
  assert.deepEqual(calls.map(call => call.args), [['--version']]);
  assert.equal(fs.existsSync(f.lock), false);
});

test('원장에 없는 SDK 입력을 거부한다', t => {
  // given
  const f = fixture(t);
  fs.writeFileSync(path.join(f.source, 'build-support/sdk/unrecorded.txt'), 'extra');
  const calls = [];
  // when
  const actual = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: pnpmRun(calls), env: {} }));
  // then
  assert.equal(actual.code, 'SOURCE_MISMATCH');
  assert.deepEqual(calls.map(call => call.args), [['--version']]);
});

test('SDK 원장 파일을 가리키는 symlink를 거부한다', t => {
  // given
  const f = fixture(t);
  const provenance = readProvenance(f.source);
  const target = path.join(f.source, 'build-support/sdk', provenance.files[0].path);
  const outside = path.join(f.parent, 'outside.txt');
  fs.writeFileSync(outside, 'outside');
  fs.unlinkSync(target);
  try { fs.symlinkSync(outside, target); } catch (error) {
    if (['EPERM', 'EACCES', 'ENOTSUP'].includes(error.code)) return t.skip('이 환경에서는 symlink를 만들 수 없습니다');
    throw error;
  }
  const calls = [];
  // when
  const actual = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: pnpmRun(calls), env: {} }));
  // then
  assert.equal(actual.code, 'SOURCE_MISMATCH');
  assert.deepEqual(calls.map(call => call.args), [['--version']]);
});

test('기존 다른 실행의 lock을 보존하고 bootstrap을 거부한다', t => {
  // given
  const f = fixture(t);
  fs.writeFileSync(f.lock, 'foreign-owner');
  const calls = [];
  // when
  const actual = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: pnpmRun(calls), env: {} }));
  // then
  assert.equal(actual.code, 'BOOTSTRAP_LOCKED');
  assert.equal(fs.readFileSync(f.lock, 'utf8'), 'foreign-owner');
  assert.deepEqual(calls.map(call => call.args), [['--version']]);
});

test('동시 bootstrap은 활성 lock을 사용하고 소유한 lock만 정리한다', t => {
  // given
  const f = fixture(t);
  let concurrent;
  const run = (args, cwd) => {
    if (args[0] === '--version') return PNPM_VERSION;
    if (!concurrent) {
      concurrent = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run: () => PNPM_VERSION, env: {} }));
    }
    return undefined;
  };
  // when
  const actual = bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run, env: {} });
  // then
  assert.equal(actual.sdk_revision, SDK_REVISION);
  assert.equal(concurrent.code, 'BOOTSTRAP_LOCKED');
  assert.equal(fs.existsSync(f.lock), false);
});

test('명령 실패 뒤 자신이 만든 lock만 제거한다', t => {
  // given
  const f = fixture(t);
  const unrelated = path.join(f.source, 'keep.txt');
  fs.writeFileSync(unrelated, 'preserve');
  const run = args => {
    if (args[0] === '--version') return PNPM_VERSION;
    const error = new Error('COMMAND_FAILED');
    error.code = 'COMMAND_FAILED';
    throw error;
  };
  // when
  const actual = observe(() => bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run, env: {} }));
  // then
  assert.equal(actual.code, 'COMMAND_FAILED');
  assert.equal(fs.existsSync(f.lock), false);
  assert.equal(fs.readFileSync(unrelated, 'utf8'), 'preserve');
});

test('lock 경로가 실행 중 교체되면 다른 소유자의 lock을 보존한다', t => {
  // given
  const f = fixture(t);
  let replaced = false;
  const run = args => {
    if (args[0] === '--version') return PNPM_VERSION;
    if (!replaced) {
      fs.renameSync(f.lock, `${f.lock}.owned`);
      fs.writeFileSync(f.lock, 'replacement-owner');
      replaced = true;
    }
    return undefined;
  };
  // when
  const actual = bootstrap({ source: f.source, nodeVersion: NODE_VERSION, run, env: {} });
  // then
  assert.equal(actual.sdk_revision, SDK_REVISION);
  assert.equal(fs.readFileSync(f.lock, 'utf8'), 'replacement-owner');
  assert.equal(fs.existsSync(`${f.lock}.owned`), true);
});

for (const [nodeVersion, pnpmVersion, code] of [
  ['24.20.0', PNPM_VERSION, 'NODE_VERSION_MISMATCH'],
  [NODE_VERSION, '10.30.2', 'PNPM_VERSION_MISMATCH'],
]) {
  test(`고정 도구 버전이 다르면 ${code}로 중단한다`, t => {
    // given
    const f = fixture(t);
    const calls = [];
    const run = pnpmRun(calls);
    // when
    const actual = observe(() => bootstrap({ source: f.source, nodeVersion, run: (args, cwd) => args[0] === '--version' ? pnpmVersion : run(args, cwd), env: {} }));
    // then
    assert.equal(actual.code, code);
    assert.equal(fs.existsSync(f.lock), false);
  });
}

test('Windows 명령은 고정 실행 파일을 쓰고 셸 제어 문자를 거부한다', () => {
  // given
  const args = ['--filter', '@tabularis/plugin-api', 'build'];
  // when
  const actual = commandFor(args, 'win32', 'C:\\Windows');
  // then
  assert.equal(actual.executable, 'C:\\Windows\\System32\\cmd.exe');
  assert.deepEqual(actual.args, ['/d', '/s', '/c', 'corepack pnpm --filter @tabularis/plugin-api build']);
  assert.equal(observe(() => commandFor(['build&whoami'], 'win32', 'C:\\Windows')).code, 'INVALID_COMMAND');
  assert.equal(observe(() => commandFor(['--dir', '한글 경로'], 'win32', 'C:\\Windows')).code, 'INVALID_COMMAND');
});
