import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { directoryLinkType, fixtureEnvironment, fixturePaths, nativeNames, runtimeCache, selectTargets } from './platform-options.mjs';
import { productionOptions } from './production-harness.mjs';

function capture(action) { try { return { value: action(), code: null }; } catch (error) { return { value: null, code: error.code ?? error.message }; } }
const windows = { TABULARIS_C3B_TEST_LAUNCHER: 'C:\\fixture\\launcher.exe', TABULARIS_C3B_TEST_NODE_ARCHIVE: 'C:\\fixture\\node.zip', TABULARIS_C3B_TEST_PYTHON: 'C:\\fixture\\python.exe', TABULARIS_C3B_TEST_NODE_CACHE: 'C:\\fixture\\cache', SystemRoot: 'C:\\Windows' };
const linux = { TABULARIS_C3B_TEST_LAUNCHER: '/fixture/launcher', TABULARIS_C3B_TEST_NODE_ARCHIVE: '/fixture/node.tar.gz', TABULARIS_C3B_TEST_NODE_CACHE: '/fixture/cache' };

for (const [platform, hostArch, arches] of [['darwin', 'arm64', ['arm64', 'x64']], ['darwin', 'x64', ['x64']], ['linux', 'arm64', ['arm64']], ['linux', 'x64', ['x64']], ['win32', 'x64', ['x64']]]) {
  test(`${platform} ${hostArch}는 승인된 native 실행 대상만 선택한다`, () => {
    // given
    const input = { platform, hostArch };
    // when
    const actual = selectTargets(input.platform, input.hostArch);
    // then
    assert.deepEqual(actual, arches.map(arch => ({ platform, arch })));
    assert.ok(actual.length > 0);
  });
}

for (const [platform, arch] of [['win32', 'arm64'], ['linux', 'ia32'], ['darwin', 'ia32'], ['freebsd', 'x64'], ['', 'x64']]) {
  test(`${platform || '빈 OS'} ${arch}는 빈 대상이나 자동 skip 대신 명시적으로 실패한다`, () => {
    // given
    const input = { platform, arch };
    // when
    const actual = capture(() => selectTargets(input.platform, input.arch));
    // then
    assert.equal(actual.code, 'CAPABILITY_UNAVAILABLE');
    assert.equal(actual.value, null);
  });
}

for (const [platform, arch, archive, executable, runtime] of [
  ['darwin', 'arm64', 'node-v24.21.0-darwin-arm64.tar.gz', 'cosmos-nosql', 'runtime/bin/node'],
  ['darwin', 'x64', 'node-v24.21.0-darwin-x64.tar.gz', 'cosmos-nosql', 'runtime/bin/node'],
  ['linux', 'arm64', 'node-v24.21.0-linux-arm64.tar.gz', 'cosmos-nosql', 'runtime/bin/node'],
  ['linux', 'x64', 'node-v24.21.0-linux-x64.tar.gz', 'cosmos-nosql', 'runtime/bin/node'],
  ['win32', 'x64', 'node-v24.21.0-win-x64.zip', 'cosmos-nosql.exe', 'runtime/node.exe'],
]) {
  test(`${platform} ${arch} archive와 manifest의 native 파일명은 고정 규칙을 따른다`, () => {
    // given
    const input = { platform, arch };
    // when
    const actual = nativeNames(input.platform, input.arch);
    // then
    assert.equal(actual.archive, archive);
    assert.equal(actual.executable, executable);
    assert.equal(actual.runtime, runtime);
    assert.match(actual.pin, /^[a-f0-9]{64}$/);
  });
}

test('macOS debug와 production 기본 launcher 경로 및 cache를 보존한다', () => {
  // given
  const inputs = [{ arch: 'arm64', production: false }, { arch: 'x64', production: false }, { arch: 'arm64', production: true }, { arch: 'x64', production: true }];
  // when
  const actual = inputs.map(input => fixturePaths({ platform: 'darwin', env: {}, ...input }));
  // then
  assert.deepEqual(actual.map(value => value.launcher), ['/tmp/tabularis-c3b-launcher-target/debug/tabularis-azure-launcher', '/tmp/tabularis-c3b-launcher-target/x86_64-apple-darwin/debug/tabularis-azure-launcher', '/tmp/tabularis-c3b2b1-launcher-target/aarch64-apple-darwin/release/tabularis-azure-launcher', '/tmp/tabularis-c3b2b1-launcher-target/x86_64-apple-darwin/release/tabularis-azure-launcher']);
  assert.ok(actual.every(value => value.runtimeArchive.startsWith('/tmp/tabularis-runtime-cache/node-v24.21.0/')));
  assert.ok(actual.every(value => value.python === '/usr/bin/python3' && value.tar === '/usr/bin/tar'));
});

for (const [platform, arch, env] of [['linux', 'arm64', linux], ['linux', 'x64', linux], ['win32', 'x64', windows]]) {
  test(`${platform} ${arch}는 지정한 절대 launcher와 archive만 사용한다`, () => {
    // given
    const input = { platform, arch, env };
    // when
    const actual = fixturePaths(input);
    // then
    assert.equal(actual.launcher, env.TABULARIS_C3B_TEST_LAUNCHER);
    assert.equal(actual.runtimeArchive, env.TABULARIS_C3B_TEST_NODE_ARCHIVE);
    assert.equal(actual.python, platform === 'win32' ? 'C:\\fixture\\python.exe' : '/usr/bin/python3');
    assert.equal(actual.tar, platform === 'win32' ? 'C:\\Windows\\System32\\tar.exe' : '/usr/bin/bsdtar');
  });
}

for (const [platform, base, key] of [
  ['linux', linux, 'TABULARIS_C3B_TEST_LAUNCHER'], ['linux', linux, 'TABULARIS_C3B_TEST_NODE_ARCHIVE'],
  ['win32', windows, 'TABULARIS_C3B_TEST_LAUNCHER'], ['win32', windows, 'TABULARIS_C3B_TEST_NODE_ARCHIVE'], ['win32', windows, 'TABULARIS_C3B_TEST_PYTHON'], ['win32', windows, 'SystemRoot'],
]) {
  for (const replacement of [undefined, 'relative/file']) {
    test(`${platform} ${key}가 ${replacement === undefined ? '누락되면' : '상대 경로이면'} native 입력을 추측하지 않는다`, () => {
      // given
      const env = { ...base, [key]: replacement };
      // when
      const actual = capture(() => fixturePaths({ platform, arch: 'x64', env }));
      // then
      assert.equal(actual.code, 'CAPABILITY_UNAVAILABLE');
      assert.equal(actual.value, null);
    });
  }
}

for (const key of ['TABULARIS_C3B_TEST_NODE_CACHE', 'TABULARIS_C3B_TEST_PYTHON']) {
  test(`선택 입력 ${key}도 상대 경로를 거부한다`, () => {
    // given
    const env = { ...linux, [key]: 'relative/input' };
    // when
    const actual = capture(() => fixturePaths({ platform: 'linux', arch: 'x64', env }));
    // then
    assert.equal(actual.code, 'CAPABILITY_UNAVAILABLE');
  });
}

test('비macOS cache는 system temp 하위 전용 경로를 기본으로 사용한다', () => {
  // given
  const platform = process.platform === 'win32' ? 'win32' : 'linux';
  const expected = path.join(os.tmpdir(), 'tabularis-runtime-cache', 'node-v24.21.0');
  // when
  const actual = runtimeCache(platform, {});
  // then
  assert.equal(actual, expected);
});

test('전용 cache와 Python 절대 경로 입력은 POSIX 기본값보다 우선한다', () => {
  // given
  const env = { TABULARIS_C3B_TEST_NODE_CACHE: '/fixture/owned-cache', TABULARIS_C3B_TEST_PYTHON: '/fixture/python' };
  // when
  const actual = fixturePaths({ platform: 'darwin', arch: 'arm64', env });
  // then
  assert.equal(path.dirname(actual.runtimeArchive), '/fixture/owned-cache');
  assert.equal(actual.python, '/fixture/python');
});

test('경로 resolver는 whitelist 밖의 환경 입력을 수용하지 않는다', () => {
  // given
  const env = { ...linux, AZURE_SYNTHETIC_CANARY: 'never-propagated' };
  // when
  const actual = capture(() => fixturePaths({ platform: 'linux', arch: 'x64', env }));
  // then
  assert.equal(actual.code, 'CAPABILITY_UNAVAILABLE');
  assert.equal(JSON.stringify(actual).includes('never-propagated'), false);
});

for (const platform of ['darwin', 'linux', 'win32']) {
  test(`${platform} child 환경은 빈 PATH와 전용 프로필 및 Windows SystemRoot만 전달한다`, () => {
    // given
    const env = platform === 'win32' ? windows : {};
    const profile = platform === 'win32' ? 'C:\\fixture\\profile' : '/fixture/profile';
    // when
    const actual = fixtureEnvironment(profile, platform, env);
    // then
    assert.deepEqual(actual, { PATH: '', HOME: profile, USERPROFILE: profile, ...(platform === 'win32' ? { SystemRoot: 'C:\\Windows' } : {}) });
  });
}

test('directory fixture link는 Windows junction과 POSIX dir로 구분한다', () => {
  // given
  const platforms = ['win32', 'darwin', 'linux'];
  // when
  const actual = platforms.map(platform => directoryLinkType(platform));
  // then
  assert.deepEqual(actual, ['junction', 'dir', 'dir']);
});

test('production 경로 준비는 injected Windows 입력을 보존하고 system temp 안에만 작업한다', t => {
  // given
  const input = { platform: 'win32', hostArch: 'x64', env: windows };
  let options;
  t.after(() => { if (options) fs.rmSync(options.root, { recursive: true, force: true }); });
  // when
  options = productionOptions('x64', input);
  // then
  assert.equal(options.platform, 'win32');
  assert.equal(options.arch, 'x64');
  assert.equal(options.launcher, windows.TABULARIS_C3B_TEST_LAUNCHER);
  assert.equal(options.python, windows.TABULARIS_C3B_TEST_PYTHON);
  assert.equal(path.dirname(options.root), os.tmpdir());
  assert.equal(fs.lstatSync(options.root).isDirectory(), true);
  assert.equal(fs.lstatSync(options.root).isSymbolicLink(), false);
  assert.equal(path.basename(options.output), 'cosmos-nosql-0.1.0-win32-x64.zip');
});

test('x64 host에서는 arm64 production 실행을 준비하지 않는다', () => {
  // given
  const input = { platform: 'darwin', hostArch: 'x64', env: {} };
  // when
  const actual = capture(() => productionOptions('arm64', input));
  // then
  assert.equal(actual.code, 'CAPABILITY_UNAVAILABLE');
});
