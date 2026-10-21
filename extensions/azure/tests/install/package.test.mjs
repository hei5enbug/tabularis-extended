import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { packageBundle } from '../../scripts/package/index.mjs';
import { extractPinnedRuntime } from '../../scripts/package/runtime.mjs';
import { parseArguments } from '../../scripts/package/cli.mjs';
import { boundedLimits } from '../../scripts/package/errors.mjs';
import { Stage, safeRelative, sha256 } from '../../scripts/package/files.mjs';
import { prepareInstallSmoke } from './harness.mjs';
import { capture, fixture, inspectArchive, python, write } from './fixtures.mjs';

function packageAndInspect(options) {
  const result = packageBundle(options);
  return { result, archive: inspectArchive(options.output) };
}

test('패키지는 고정 레이아웃과 중첩 버전 및 원문 라이선스를 보존한다', t => {
  // given
  const value = fixture(t);
  // when
  const { result, archive } = packageAndInspect(value.options);
  // then
  assert.equal(archive.bad, null);
  assert.equal(archive.manifest.executable, 'cosmos-nosql');
  assert.equal(archive.release.node, '24.21.0');
  assert.equal(archive.release.sdk, '4.10.1');
  assert.equal(archive.release.service_protocol, 1);
  assert.equal(archive.release.min_runtime_version, '0.26.0');
  assert.equal(archive.entries.find(item => item.name === 'cosmos-nosql').mode & 0o777, 0o755);
  assert.equal(archive.entries.find(item => item.name === 'runtime/bin/node').mode & 0o777, 0o755);
  assert.ok(archive.entries.filter(item => !['cosmos-nosql', 'runtime/bin/node'].includes(item.name)).every(item => (item.mode & 0o777) === 0o644));
  assert.ok(archive.entries.every(item => item.compression === 8 && item.time.join(',') === '1980,1,1,0,0,0'));
  assert.ok(archive.entries.some(item => item.name === 'node_modules/first/node_modules/shared/package.json'));
  assert.ok(archive.entries.some(item => item.name === 'node_modules/second/node_modules/shared/package.json'));
  assert.ok(archive.entries.some(item => item.name === 'node_modules/peer/package.json'));
  assert.ok(archive.entries.some(item => item.name === 'node_modules/installedOptional/package.json'));
  assert.ok(archive.entries.some(item => item.name === 'node_modules/@tabularis/service-contracts/schemas/fixture.json'));
  assert.ok(!archive.entries.some(item => /development|\/tests\/|\.env|missingPeer|absent/.test(item.name)));
  assert.ok(archive.entries.length < 100);
  assert.ok(archive.notices.every(item => Object.keys(item).sort().join(',') === 'name,sha256,version'));
  assert.ok(archive.notices.some(item => item.name === 'shared' && item.version === '1.0.0'));
  assert.ok(archive.notices.some(item => item.name === 'shared' && item.version === '2.0.0'));
  assert.ok(!JSON.stringify(archive).includes(value.root));
  assert.equal(fs.readFileSync(`${value.options.output}.sha256`, 'utf8').trim(), result.sha256);
  assert.equal(fs.lstatSync(value.options.output).isFile(), true);
  assert.equal(fs.lstatSync(value.options.output).isSymbolicLink(), false);
  if (process.platform !== 'win32') assert.equal(fs.statSync(value.options.output).mode & 0o777, 0o600);
});

function packageTwice(options) {
  return [packageBundle(options), packageBundle({ ...options, output: `${options.output}.second.zip` })];
}

function packageLicense(options) {
  packageBundle(options);
  const result = python(['-c', `import json,sys,zipfile
with zipfile.ZipFile(sys.argv[1]) as archive:
 print(json.dumps({'license':archive.read('LICENSE').decode('utf-8'),'release':json.loads(archive.read('release.json'))}))`, options.output]);
  if (result.status !== 0) throw new Error('root license verification failed');
  return JSON.parse(result.stdout);
}

test('패키지는 플러그인 자체 라이선스 원문과 파일 해시를 포함한다', t => {
  // given
  const value = fixture(t);
  const expected = fs.readFileSync(path.join(value.source, 'LICENSE'));
  // when
  const archive = packageLicense(value.options);
  // then
  assert.equal(archive.license, expected.toString('utf8'));
  assert.deepEqual(archive.release.files.find(file => file.path === 'LICENSE'), { path: 'LICENSE', bytes: expected.length, sha256: sha256(expected) });
});

test('플러그인 자체 라이선스가 없으면 ZIP을 게시하지 않는다', t => {
  // given
  const value = fixture(t);
  fs.unlinkSync(path.join(value.source, 'LICENSE'));
  // when
  const result = capture(() => packageBundle(value.options));
  // then
  assert.equal(result.error, 'ASSET_MISSING');
  assert.equal(fs.existsSync(value.options.output), false);
  assert.equal(fs.existsSync(`${value.options.output}.sha256`), false);
});

test('같은 입력으로 만든 두 ZIP의 SHA256이 같다', t => {
  // given
  const value = fixture(t);
  // when
  const results = packageTwice(value.options);
  // then
  assert.equal(results[0].sha256, results[1].sha256);
  assert.equal(results[0].bytes, results[1].bytes);
});

function materializedImport(options) {
  packageBundle(options);
  const prepared = prepareInstallSmoke({ bundle: options.output });
  try {
    const extraction = python(prepared.extraction.args);
    if (extraction.status !== 0) return { extraction: extraction.status };
    const script = `import cosmos from '@azure/cosmos';import {schema} from '@tabularis/service-contracts';import first from 'first';import second from 'second';import cycle from 'cycle';console.log(JSON.stringify({cosmos,schema,first,second,cycle}));`;
    write(path.join(prepared.installed, 'verify.mjs'), script);
    const executed = spawnSync(process.execPath, [path.join(prepared.installed, 'verify.mjs')], { cwd: prepared.installed, shell: false, encoding: 'utf8' });
    return { extraction: 0, code: executed.status, data: executed.status === 0 ? JSON.parse(executed.stdout) : null, symlinks: fs.lstatSync(path.join(prepared.installed, 'node_modules', '@tabularis', 'service-contracts')).isSymbolicLink() };
  } finally { prepared.dispose(); }
}

test('작업공간 링크를 실체화한 뒤 상대 import와 cycle이 설치본에서 동작한다', t => {
  // given
  const value = fixture(t);
  // when
  const result = materializedImport(value.options);
  // then
  assert.equal(result.extraction, 0);
  assert.equal(result.code, 0);
  assert.deepEqual(result.data, { cosmos: '4.10.1', schema: 'materialized contract', first: '1.0.0:1.0.0', second: '2.0.0', cycle: 'bounded cycle' });
  assert.equal(result.symlinks, false);
});

for (const [title, change, expected] of [
  ['필수 UI 파일이 없으면 게시하지 않는다', value => fs.unlinkSync(path.join(value.source, 'ui/dist/index.js')), 'ASSET_MISSING'],
  ['서로 다른 두 CSS 입력은 거부한다', value => write(path.join(value.source, 'ui/dist/style.css'), 'different'), 'ASSET_CONFLICT'],
  ['라이선스가 없는 의존성은 거부한다', value => fs.unlinkSync(path.join(value.source, 'node_modules/@azure/cosmos/LICENSE')), 'LICENSE_MISSING'],
  ['의존성 내부의 파일 심볼릭 링크는 거부한다', value => fs.symlinkSync(path.join(value.root, 'launcher'), path.join(value.source, 'node_modules/first/linked.js')), 'SYMLINK_CONTENT'],
  ['필수 peer 의존성이 없으면 거부한다', value => fs.rmSync(path.join(value.source, 'node_modules/first/node_modules/peer'), { recursive: true }), 'DEPENDENCY_MISSING'],
  ['오래된 최소 호스트 버전은 거부한다', value => write(path.join(value.source, 'manifest.json'), { ...JSON.parse(fs.readFileSync(path.join(value.source, 'manifest.json'))), min_runtime_version: '0.25.0' }), 'MANIFEST_NOT_READY'],
  ['bootstrap manifest는 거부한다', value => write(path.join(value.source, 'manifest.json'), { ...JSON.parse(fs.readFileSync(path.join(value.source, 'manifest.json'))), description: 'bootstrap driver' }), 'MANIFEST_NOT_READY'],
  ['미완료 capability는 거부한다', value => { const manifest = JSON.parse(fs.readFileSync(path.join(value.source, 'manifest.json'))); manifest.capabilities.documents_v1 = false; write(path.join(value.source, 'manifest.json'), manifest); }, 'MANIFEST_NOT_READY'],
  ['지원하지 않는 SDK 버전은 거부한다', value => { const file = path.join(value.source, 'node_modules/@azure/cosmos/package.json'); const manifest = JSON.parse(fs.readFileSync(file)); manifest.version = '4.10.0'; write(file, manifest); }, 'SDK_VERSION_MISMATCH'],
  ['직접 연결된 service contracts가 없으면 거부한다', value => { const file = path.join(value.source, 'package.json'); const manifest = JSON.parse(fs.readFileSync(file)); delete manifest.dependencies['@tabularis/service-contracts']; write(file, manifest); }, 'DEPENDENCY_MISSING'],
]) {
  test(title, t => {
    // given
    const value = fixture(t);
    change(value);
    // when
    const result = capture(() => packageBundle(value.options));
    // then
    assert.equal(result.error, expected);
    assert.equal(fs.existsSync(value.options.output), false);
    assert.equal(fs.existsSync(`${value.options.output}.sha256`), false);
  });
}

test('잘못된 Node 해시에서는 tar를 실행하지 않는다', t => {
  // given
  const value = fixture(t);
  const tar = path.join(value.root, 'marker-tar');
  const marker = path.join(value.root, 'tar-called');
  write(tar, `#!/bin/sh\ntouch '${marker}'\n`);
  fs.chmodSync(tar, 0o755);
  // when
  const result = capture(() => extractPinnedRuntime({ platform: 'darwin', arch: 'arm64', archive: value.options.runtimeArchive, expectedPin: '0'.repeat(64), tar }));
  // then
  assert.equal(result.error, 'RUNTIME_HASH_MISMATCH');
  assert.equal(fs.existsSync(marker), false);
});

test('Node member가 링크이면 압축 해제 전에 거부한다', t => {
  // given
  const value = fixture(t);
  const node = path.join(value.root, 'archive/node-v24.21.0-darwin-arm64/bin/node');
  fs.unlinkSync(node);
  fs.symlinkSync(value.launcher, node);
  const archive = spawnSync(value.options.tar, ['-czf', '../runtime.tar.gz', 'node-v24.21.0-darwin-arm64'], { cwd: path.join(value.root, 'archive'), shell: false });
  if (archive.status !== 0) throw new Error('fixture archive unavailable', { cause: archive.error ?? archive.stderr?.toString('utf8') });
  const expectedPin = sha256(fs.readFileSync(value.options.runtimeArchive));
  // when
  const result = capture(() => extractPinnedRuntime({ platform: 'darwin', arch: 'arm64', archive: value.options.runtimeArchive, expectedPin, tar: value.options.tar }));
  // then
  assert.equal(result.error, 'INVALID_ARCHIVE');
});

test('CRC가 손상된 ZIP runtime은 거부한다', t => {
  // given
  const value = fixture(t);
  const archive = path.join(value.root, 'bad.zip');
  python(['-c', `import sys, zipfile
p=sys.argv[1]
with zipfile.ZipFile(p,'w',compression=zipfile.ZIP_STORED) as z:
 z.writestr('node-v24.21.0-win-x64/node.exe',b'NODE_MARKER')
 z.writestr('node-v24.21.0-win-x64/LICENSE',b'license')
data=bytearray(open(p,'rb').read());i=data.index(b'NODE_MARKER');data[i]^=1;open(p,'wb').write(data)`, archive]);
  const expectedPin = sha256(fs.readFileSync(archive));
  // when
  const result = capture(() => extractPinnedRuntime({ platform: 'win32', arch: 'x64', archive, expectedPin, tar: value.options.tar }));
  // then
  assert.equal(result.error, 'UNSUPPORTED_ARCHIVE_PARSER');
});

test('게시 전 실패는 기존 ZIP과 checksum을 보존한다', t => {
  // given
  const value = fixture(t);
  write(value.options.output, 'existing-zip');
  write(`${value.options.output}.sha256`, 'existing-checksum');
  fs.unlinkSync(path.join(value.source, 'ui/dist/index.js'));
  // when
  const result = capture(() => packageBundle({ ...value.options, replace: true }));
  // then
  assert.equal(result.error, 'ASSET_MISSING');
  assert.equal(fs.readFileSync(value.options.output, 'utf8'), 'existing-zip');
  assert.equal(fs.readFileSync(`${value.options.output}.sha256`, 'utf8'), 'existing-checksum');
});

test('기존 목적지는 기본 모드에서 교체하지 않는다', t => {
  // given
  const value = fixture(t);
  write(value.options.output, 'existing-zip');
  // when
  const result = capture(() => packageBundle(value.options));
  // then
  assert.equal(result.error, 'OUTPUT_EXISTS');
  assert.equal(fs.readFileSync(value.options.output, 'utf8'), 'existing-zip');
});

function publishWithRace(options, mode) {
  const link = fs.linkSync;
  fs.linkSync = (source, destination) => {
    if (mode === 'destination' && destination === options.output) write(destination, 'race-winner');
    if (destination === `${options.output}.sha256` && mode !== 'destination') {
      if (mode === 'changed') write(options.output, 'external-change');
      const error = new Error('synthetic publication error');
      error.code = 'EACCES';
      throw error;
    }
    return link(source, destination);
  };
  try { return capture(() => packageBundle(options)); } finally { fs.linkSync = link; }
}

test('목적지 경쟁에서 먼저 게시된 파일을 덮어쓰지 않는다', t => {
  // given
  const value = fixture(t);
  // when
  const result = publishWithRace(value.options, 'destination');
  // then
  assert.equal(result.error, 'OUTPUT_EXISTS');
  assert.equal(fs.readFileSync(value.options.output, 'utf8'), 'race-winner');
  assert.equal(fs.existsSync(`${value.options.output}.sha256`), false);
});

test('checksum 게시 실패 시 자신의 미변경 ZIP만 정리한다', t => {
  // given
  const value = fixture(t);
  // when
  const result = publishWithRace(value.options, 'checksum');
  // then
  assert.equal(result.error, 'PACKAGE_FAILED');
  assert.equal(fs.existsSync(value.options.output), false);
  assert.equal(fs.existsSync(`${value.options.output}.sha256`), false);
});

test('checksum 실패 전에 외부에서 ZIP이 바뀌면 삭제하지 않는다', t => {
  // given
  const value = fixture(t);
  // when
  const result = publishWithRace(value.options, 'changed');
  // then
  assert.equal(result.error, 'PARTIAL_PUBLICATION_APPLIED');
  assert.equal(fs.readFileSync(value.options.output, 'utf8'), 'external-change');
});

function replaceWithChecksumFailure(options) {
  const rename = fs.renameSync;
  fs.renameSync = (source, destination) => {
    if (destination === `${options.output}.sha256`) throw new Error('synthetic checksum failure');
    return rename(source, destination);
  };
  try { return capture(() => packageBundle({ ...options, replace: true })); } finally { fs.renameSync = rename; }
}

test('명시적 교체 뒤 checksum 실패는 적용된 부분 게시로 알린다', t => {
  // given
  const value = fixture(t);
  write(value.options.output, 'old-zip');
  write(`${value.options.output}.sha256`, 'old-checksum');
  // when
  const result = replaceWithChecksumFailure(value.options);
  // then
  assert.equal(result.error, 'PARTIAL_PUBLICATION_APPLIED');
  assert.notEqual(fs.readFileSync(value.options.output).subarray(0, 7).toString(), 'old-zip');
  assert.equal(fs.readFileSync(`${value.options.output}.sha256`, 'utf8'), 'old-checksum');
});

test('동일한 두 CSS 입력에서는 canonical 파일을 사용한다', t => {
  // given
  const value = fixture(t);
  write(path.join(value.source, 'ui/dist/style.css'), fs.readFileSync(path.join(value.source, 'ui/dist/azure-ui.css')));
  // when
  const result = capture(() => packageBundle(value.options));
  // then
  assert.equal(result.error, null);
  assert.ok(result.value.files > 0);
});

test('Windows 패키지만 executable과 런타임 경로를 바꾼다', t => {
  // given
  const value = fixture(t);
  const archive = path.join(value.root, 'windows.zip');
  python(['-c', `import sys, zipfile
with zipfile.ZipFile(sys.argv[1],'w') as z:
 z.writestr('node-v24.21.0-win-x64/node.exe',b'MZ fixture')
 z.writestr('node-v24.21.0-win-x64/LICENSE',b'Synthetic Windows license')`, archive]);
  const options = { ...value.options, platform: 'win32', arch: 'x64', runtimeArchive: archive, expectedRuntimePin: sha256(fs.readFileSync(archive)) };
  // when
  const result = packageAndInspect(options);
  // then
  assert.equal(result.archive.manifest.executable, 'cosmos-nosql.exe');
  assert.ok(result.archive.entries.some(item => item.name === 'runtime/node.exe'));
  assert.ok(result.archive.entries.some(item => item.name === 'cosmos-nosql.exe'));
  assert.equal(JSON.parse(fs.readFileSync(path.join(value.source, 'manifest.json'))).executable, 'cosmos-nosql');
});

test('지원하지 않는 Windows arm64 대상은 거부한다', t => {
  // given
  const value = fixture(t);
  // when
  const result = capture(() => packageBundle({ ...value.options, platform: 'win32', arch: 'arm64' }));
  // then
  assert.equal(result.error, 'UNSUPPORTED_PLATFORM');
  assert.equal(fs.existsSync(value.options.output), false);
});

test('표준 ZIP 검증기는 변조된 번들의 CRC를 거부한다', t => {
  // given
  const value = fixture(t);
  packageBundle(value.options);
  const bytes = fs.readFileSync(value.options.output);
  const firstData = 30 + bytes.readUInt16LE(26) + bytes.readUInt16LE(28);
  bytes[firstData + 2] ^= 1;
  fs.writeFileSync(value.options.output, bytes);
  // when
  const result = python(['-c', 'import sys,zipfile\nwith zipfile.ZipFile(sys.argv[1]) as z:\n for i in z.infolist(): z.read(i)', value.options.output]);
  // then
  assert.notEqual(result.status, 0);
});

test('전체 바이트의 정확한 경계는 허용하고 다음 파일은 거부한다', t => {
  // given
  const value = fixture(t);
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits({ bytes: 5 }));
  stage.add('first', '12345');
  // when
  const result = capture(() => stage.add('second', '6'));
  // then
  assert.equal(stage.bytes, 5);
  assert.equal(stage.entries.size, 1);
  assert.equal(result.error, 'PACKAGE_TOO_LARGE');
});

test('release의 파일 SHA256은 독립 ZIP 내용과 일치한다', t => {
  // given
  const value = fixture(t);
  packageBundle(value.options);
  // when
  const result = python(['-c', `import hashlib,json,sys,zipfile
with zipfile.ZipFile(sys.argv[1]) as z:
 r=json.loads(z.read('release.json'))
 print(json.dumps({'ok':all(hashlib.sha256(z.read(f['path'])).hexdigest()==f['sha256'] and len(z.read(f['path']))==f['bytes'] for f in r['files']),'count':len(r['files']),'entries':len(z.infolist())}))`, value.options.output]);
  // then
  assert.equal(result.status, 0);
  assert.deepEqual(JSON.parse(result.stdout), { ok: true, count: JSON.parse(result.stdout).entries - 1, entries: JSON.parse(result.stdout).entries });
});

for (const [name, limits] of [['파일 수', { files: 3 }], ['전체 바이트', { bytes: 64 }], ['그래프 깊이', { depth: 1 }], ['그래프 패키지 수', { packages: 1 }], ['ZIP 바이트', { zipBytes: 100 }]]) {
  test(`${name} 상한을 넘으면 ZIP을 게시하지 않는다`, t => {
    // given
    const value = fixture(t);
    // when
    const result = capture(() => packageBundle({ ...value.options, limits }));
    // then
    assert.ok(['PACKAGE_TOO_LARGE', 'GRAPH_TOO_LARGE', 'ZIP_TOO_LARGE'].includes(result.error));
    assert.equal(fs.existsSync(value.options.output), false);
  });
}

test('512MiB 기본 상한과 256MiB 단일 파일 상한을 늘릴 수 없다', () => {
  // given
  const options = { bytes: 512 * 1024 * 1024 + 1 };
  // when
  const result = capture(() => boundedLimits(options));
  // then
  assert.equal(result.error, 'INVALID_LIMIT');
  assert.equal(boundedLimits().bytes, 512 * 1024 * 1024);
  assert.equal(boundedLimits().fileBytes, 256 * 1024 * 1024);
});

test('큰 sparse 파일을 buffer로 읽기 전에 거부한다', t => {
  // given
  const value = fixture(t);
  const file = path.join(value.root, 'oversize');
  write(file, '');
  fs.truncateSync(file, 256 * 1024 * 1024 + 1);
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits());
  // when
  const result = capture(() => stage.copy(file, 'large'));
  // then
  assert.equal(result.error, 'PACKAGE_TOO_LARGE');
  assert.equal(stage.entries.size, 0);
});

for (const value of ['../escape', '/absolute', 'a\\b', 'a:b', 'a\0b', 'a//b', 'a/./b']) {
  test(`ZIP의 위험한 경로 ${JSON.stringify(value)}를 거부한다`, () => {
    // given
    const relative = value;
    // when
    const result = capture(() => safeRelative(relative));
    // then
    assert.equal(result.error, 'INVALID_PATH');
  });
}

test('동일 ZIP 경로를 두 번 등록하지 않는다', t => {
  // given
  const value = fixture(t);
  const stage = new Stage(path.join(value.root, 'stage'), boundedLimits());
  stage.add('same', 'first');
  // when
  const result = capture(() => stage.add('same', 'second'));
  // then
  assert.equal(result.error, 'DUPLICATE_PATH');
  assert.equal(stage.entries.size, 1);
});

test('CLI는 알려지지 않은 옵션과 중복 옵션을 거부한다', () => {
  // given
  const args = ['--platform', 'darwin', '--platform', 'linux', '--replace', 'true'];
  // when
  const result = capture(() => parseArguments(args));
  // then
  assert.equal(result.error, 'INVALID_ARGUMENT');
});

test('CLI에서는 런타임 pin override를 받을 수 없다', () => {
  // given
  const args = ['--expected-runtime-pin', '0'.repeat(64)];
  // when
  const result = capture(() => parseArguments(args));
  // then
  assert.equal(result.error, 'INVALID_ARGUMENT');
});

test('CLI 오류에는 비밀 canary나 절대 경로가 나타나지 않는다', t => {
  // given
  const value = fixture(t);
  const args = ['scripts/package/cli.mjs', '--platform', 'darwin', '--arch', 'arm64', '--source', value.source, '--launcher', value.launcher, '--runtime-archive', `${value.root}/SYNTHETIC_SECRET_CANARY`, '--output', value.options.output];
  // when
  const result = spawnSync(process.execPath, args, { cwd: path.resolve(import.meta.dirname, '../..'), shell: false, encoding: 'utf8' });
  // then
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.equal(result.stderr, 'INVALID_ARCHIVE\n');
  assert.equal(result.stderr.includes(value.root), false);
  assert.equal(result.stderr.includes('SYNTHETIC_SECRET_CANARY'), false);
});
