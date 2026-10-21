import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { Stage } from '../../scripts/package/files.mjs';
import { materializeProductionGraph, resolveInstalled } from '../../scripts/package/graph.mjs';
import { fileURLToPath } from 'node:url';

const LIMITS = { packages: 4096, depth: 128, files: 50000, bytes: 512 * 1024 * 1024, fileBytes: 256 * 1024 * 1024 };

function write(file, data) {
  fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
  fs.writeFileSync(file, typeof data === 'object' ? JSON.stringify(data) : data);
}

function graphFixture(t, limits = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-graph-'));
  fs.chmodSync(root, 0o700);
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const source = path.join(root, 'source');
  const target = path.join(root, 'stage');
  fs.mkdirSync(source, { mode: 0o700 });
  fs.mkdirSync(target, { mode: 0o700 });
  write(path.join(source, 'package.json'), { name: 'graph-fixture', version: '1.0.0', dependencies: {} });
  return { root, source, target, stage: new Stage(target, { ...LIMITS, ...limits }) };
}

function code(label, children = []) {
  const expression = children.length ? `${JSON.stringify(label)}+'['+[${children.map((_, index) => `child${index}()`).join(',')}].join(',')+']'` : JSON.stringify(label);
  return {
    esm: `${children.map((name, index) => `import child${index} from ${JSON.stringify(name)};`).join('\n')}\nexport default () => ${expression};\n`,
    cjs: `module.exports = () => ${children.length ? `${JSON.stringify(label)}+'['+[${children.map(name => `require(${JSON.stringify(name)})()`).join(',')}].join(',')+']'` : JSON.stringify(label)};\n`,
  };
}

function pkg(value, id, name, version = '1.0.0', extra = {}, label = id, children = []) {
  const root = path.join(value.root, 'installed', id);
  const module = code(label, children);
  write(path.join(root, 'package.json'), { name, version, type: 'module', exports: { import: './index.mjs', require: './index.cjs' }, ...extra });
  write(path.join(root, 'index.mjs'), module.esm);
  write(path.join(root, 'index.cjs'), module.cjs);
  write(path.join(root, 'LICENSE'), `Synthetic fixture license: ${name}@${version}\n`);
  return root;
}

function bind(importer, name, root) {
  const destination = path.join(importer, 'node_modules', name);
  fs.mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
  fs.symlinkSync(root, destination, process.platform === 'win32' ? 'junction' : 'dir');
}

function direct(value, packages, extra = {}) {
  const dependencies = {};
  for (const [name, root] of Object.entries(packages)) {
    dependencies[name] = '1';
    bind(value.source, name, root);
  }
  write(path.join(value.source, 'package.json'), { name: 'graph-fixture', version: '1.0.0', dependencies, ...extra });
}

function oracle(root, names, home) {
  const entry = path.join(root, 'graph-oracle.mjs');
  write(entry, `import {createRequire} from 'node:module';
const require=createRequire(import.meta.url);
const result={imported:{},required:{}};
for(const name of ${JSON.stringify(names)}){
 const imported=(await import(name)).default;
 const required=require(name);
 result.imported[name]=typeof imported==='function'?imported():imported;
 result.required[name]=typeof required==='function'?required():required;
}
process.stdout.write(JSON.stringify(result)+'\\n');
`);
  const child = spawnSync(process.execPath, [entry], { shell: false, encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024, env: { PATH: '', HOME: home, USERPROFILE: home } });
  return { status: child.status, stderr: child.stderr, value: child.status === 0 ? JSON.parse(child.stdout) : null };
}

function materialize(value, names = []) {
  const graph = materializeProductionGraph(value.stage, value.source);
  return { graph, original: names.length ? oracle(value.source, names, value.root) : null, installed: names.length ? oracle(value.target, names, value.root) : null, paths: [...value.stage.entries.keys()] };
}

function failure(value) {
  try { return { graph: materializeProductionGraph(value.stage, value.source), error: null }; }
  catch (error) { return { graph: null, error: error.code ?? error.message, entries: value.stage.entries.size }; }
}

test('다이아몬드 의존성은 같은 실제 패키지를 한 번만 복사하고 Node 해석을 유지한다', t => {
  // given
  const value = graphFixture(t);
  const shared = pkg(value, 'shared', '@fixture/shared');
  const left = pkg(value, 'left', 'left', '1.0.0', { dependencies: { '@fixture/shared': '1' } }, 'left', ['@fixture/shared']);
  const right = pkg(value, 'right', 'right', '1.0.0', { dependencies: { '@fixture/shared': '1' } }, 'right', ['@fixture/shared']);
  bind(left, '@fixture/shared', shared);
  bind(right, '@fixture/shared', shared);
  direct(value, { left, right });
  // when
  const result = materialize(value, ['left', 'right']);
  // then
  assert.equal(result.graph.packages.filter(item => item.name === '@fixture/shared').length, 1);
  assert.equal(result.graph.packages.find(item => item.name === '@fixture/shared').path, 'node_modules/@fixture/shared');
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.equal(result.installed.stderr, '');
  assert.deepEqual(result.installed.value, result.original.value);
  assert.deepEqual(result.installed.value.imported, { left: 'left[shared]', right: 'right[shared]' });
});

test('직접 의존성과 전이 의존성의 버전이 다르면 로컬 패키지를 유지한다', t => {
  // given
  const value = graphFixture(t);
  const global = pkg(value, 'global', 'shared', '1.0.0');
  const local = pkg(value, 'local', 'shared', '2.0.0');
  const parent = pkg(value, 'parent', 'parent', '1.0.0', { dependencies: { shared: '2' } }, 'parent', ['shared']);
  bind(parent, 'shared', local);
  direct(value, { parent, shared: global });
  // when
  const result = materialize(value, ['parent', 'shared']);
  // then
  assert.deepEqual(result.graph.packages.filter(item => item.name === 'shared').map(item => [item.version, item.path]), [['2.0.0', 'node_modules/parent/node_modules/shared'], ['1.0.0', 'node_modules/shared']]);
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
  assert.equal(result.installed.value.imported.parent, 'parent[local]');
});

test('가까운 상위 패키지가 전역 바인딩을 가리면 하위에 정확한 의존성을 다시 배치한다', t => {
  // given
  const value = graphFixture(t);
  const global = pkg(value, 'global', 'shared');
  const shadow = pkg(value, 'shadow', 'shared', '2.0.0');
  const branch = pkg(value, 'branch-one', 'branch', '1.0.0', { dependencies: { shared: '1' } }, 'branch', ['shared']);
  const otherBranch = pkg(value, 'branch-two', 'branch', '2.0.0');
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { shared: '2', branch: '1' } }, 'first', ['shared', 'branch']);
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { branch: '2' } }, 'second', ['branch']);
  bind(branch, 'shared', global);
  bind(first, 'shared', shadow);
  bind(first, 'branch', branch);
  bind(second, 'branch', otherBranch);
  direct(value, { first, second, shared: global });
  // when
  const result = materialize(value, ['first', 'second', 'shared']);
  // then
  assert.ok(result.graph.packages.some(item => item.path === 'node_modules/first/node_modules/branch/node_modules/shared' && item.version === '1.0.0'));
  assert.equal(result.graph.packages.filter(item => item.name === 'shared' && item.version === '1.0.0').length, 2);
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
  assert.equal(result.installed.value.imported.first, 'first[shadow,branch[global]]');
});

test('순환 의존성은 예약된 바인딩으로 종료하고 require와 import 모두 동작한다', t => {
  // given
  const value = graphFixture(t);
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { second: '1' } });
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { first: '1' } });
  write(path.join(first, 'index.mjs'), "import second from 'second';export default ()=>'first>'+second();\n");
  write(path.join(first, 'index.cjs'), "module.exports=()=>'first>'+require('second')();\n");
  write(path.join(second, 'index.mjs'), "import first from 'first';export default ()=>'second:'+typeof first;\n");
  write(path.join(second, 'index.cjs'), "module.exports=()=>'second:'+typeof require('first');\n");
  bind(first, 'second', second);
  bind(second, 'first', first);
  direct(value, { first });
  // when
  const result = materialize(value, ['first']);
  // then
  assert.equal(result.graph.packages.length, 2);
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
  assert.equal(result.installed.value.imported.first, 'first>second:function');
});

test('설치된 선택 의존성은 포함하고 없는 선택 의존성은 생략한다', t => {
  // given
  const value = graphFixture(t);
  const optional = pkg(value, 'optional', 'optional');
  const parent = pkg(value, 'parent', 'parent', '1.0.0', { optionalDependencies: { optional: '1', missing: '1' }, peerDependencies: { missingPeer: '1' }, peerDependenciesMeta: { missingPeer: { optional: true } } }, 'parent', ['optional']);
  bind(parent, 'optional', optional);
  direct(value, { parent }, { optionalDependencies: { missingRoot: '1' } });
  // when
  const result = materialize(value, ['parent']);
  // then
  assert.deepEqual(result.graph.packages.map(item => item.name), ['optional', 'parent']);
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
});

test('필수 peer 의존성은 설치된 실제 경로를 포함한다', t => {
  // given
  const value = graphFixture(t);
  const peer = pkg(value, 'peer', 'peer');
  const parent = pkg(value, 'parent', 'parent', '1.0.0', { peerDependencies: { peer: '1' } }, 'parent', ['peer']);
  bind(parent, 'peer', peer);
  direct(value, { parent });
  // when
  const result = materialize(value, ['parent']);
  // then
  assert.equal(result.graph.packages.filter(item => item.name === 'peer').length, 1);
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
});

test('필수 peer가 없으면 어떤 파일도 복사하기 전에 실패한다', t => {
  // given
  const value = graphFixture(t);
  const parent = pkg(value, 'parent', 'parent', '1.0.0', { peerDependencies: { missing: '1' } });
  direct(value, { parent });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'DEPENDENCY_MISSING');
  assert.equal(result.entries, 0);
});

test('이름과 버전이 같아도 실제 경로가 다르면 서로 합치지 않는다', t => {
  // given
  const value = graphFixture(t);
  const firstShared = pkg(value, 'shared-one', 'shared');
  const secondShared = pkg(value, 'shared-two', 'shared');
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { shared: '1' } }, 'first', ['shared']);
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { shared: '1' } }, 'second', ['shared']);
  bind(first, 'shared', firstShared);
  bind(second, 'shared', secondShared);
  direct(value, { first, second });
  // when
  const result = materialize(value, ['first', 'second']);
  // then
  assert.equal(result.graph.packages.filter(item => item.name === 'shared').length, 2);
  assert.ok(result.graph.packages.filter(item => item.name === 'shared').every(item => item.path.includes('/node_modules/shared')));
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.deepEqual(result.installed.value, result.original.value);
  assert.deepEqual(result.installed.value.required, { first: 'first[shared-one]', second: 'second[shared-two]' });
});

test('고유 노드 수 상한을 넘으면 복사 전에 실패한다', t => {
  // given
  const value = graphFixture(t, { packages: 2 });
  const leaf = pkg(value, 'leaf', 'leaf');
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { leaf: '1' } });
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { second: '1' } });
  bind(first, 'second', second);
  bind(second, 'leaf', leaf);
  direct(value, { first });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'GRAPH_TOO_LARGE');
  assert.equal(result.entries, 0);
});

test('중복 배치로 실제 복사 수 상한을 넘으면 복사 전에 실패한다', t => {
  // given
  const value = graphFixture(t, { packages: 4 });
  const global = pkg(value, 'global', 'shared');
  const local = pkg(value, 'local', 'shared', '2.0.0');
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { shared: '2' } });
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { shared: '2' } });
  bind(first, 'shared', local);
  bind(second, 'shared', local);
  direct(value, { first, second, shared: global });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'GRAPH_TOO_LARGE');
  assert.equal(result.entries, 0);
});

test('의존성 깊이 상한을 넘으면 복사 전에 실패한다', t => {
  // given
  const value = graphFixture(t, { depth: 2 });
  const third = pkg(value, 'third', 'third');
  const second = pkg(value, 'second', 'second', '1.0.0', { dependencies: { third: '1' } });
  const first = pkg(value, 'first', 'first', '1.0.0', { dependencies: { second: '1' } });
  bind(first, 'second', second);
  bind(second, 'third', third);
  direct(value, { first });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'GRAPH_TOO_LARGE');
  assert.equal(result.entries, 0);
});

for (const [title, limits, expected] of [
  ['파일 수', { files: 3 }, 'PACKAGE_TOO_LARGE'],
  ['전체 바이트', { bytes: 1 }, 'PACKAGE_TOO_LARGE'],
  ['단일 파일 바이트', { fileBytes: 1 }, 'PACKAGE_TOO_LARGE'],
]) {
  test(`${title} 상한은 hoisting 후에도 그대로 적용된다`, t => {
    // given
    const value = graphFixture(t, limits);
    direct(value, { parent: pkg(value, 'parent', 'parent') });
    // when
    const result = failure(value);
    // then
    assert.equal(result.error, expected);
    assert.equal(result.graph, null);
  });
}

test('패키지의 비밀 파일과 테스트와 개발 의존성은 복사하지 않는다', t => {
  // given
  const value = graphFixture(t);
  const parent = pkg(value, 'parent', 'parent');
  const development = pkg(value, 'development', 'development');
  for (const name of ['.env', '.npmrc', 'credentials.json', 'secret.key', 'debug.log', 'tests/data.json', '.git/config']) write(path.join(parent, name), 'SYNTHETIC_GRAPH_SECRET_CANARY');
  direct(value, { parent }, { devDependencies: { development: '1' } });
  bind(value.source, 'development', development);
  // when
  const result = materialize(value, ['parent']);
  // then
  assert.deepEqual(result.graph.packages.map(item => item.name), ['parent']);
  assert.ok(result.paths.every(item => !/\.env|\.npmrc|credentials|secret\.key|debug\.log|tests|\.git/.test(item)));
  assert.ok([...value.stage.entries.values()].every(item => !fs.readFileSync(item.path).includes(Buffer.from('SYNTHETIC_GRAPH_SECRET_CANARY'))));
  assert.equal(result.installed.status, 0);
});

test('패키지 내부의 일반 파일 symlink는 허용하지 않는다', t => {
  // given
  const value = graphFixture(t);
  const parent = pkg(value, 'parent', 'parent');
  fs.symlinkSync(path.join(parent, 'index.mjs'), path.join(parent, 'linked.mjs'));
  direct(value, { parent });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'SYMLINK_CONTENT');
  assert.equal(result.graph, null);
});

test('패키지 버전이 비어 있으면 전체 그래프 확인 중 복사 없이 거부한다', t => {
  // given
  const value = graphFixture(t);
  direct(value, { parent: pkg(value, 'parent', 'parent', '') });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'INVALID_DEPENDENCY');
  assert.equal(result.entries, 0);
});

test('직접 패키지 이름이 설치 manifest와 다르면 복사 없이 거부한다', t => {
  // given
  const value = graphFixture(t);
  direct(value, { requested: pkg(value, 'wrong', 'different') });
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'DEPENDENCY_MISSING');
  assert.equal(result.entries, 0);
});

const PUBLIC_CREDENTIAL_FILES = [
  'dist/esm/auth/credentials.js', 'dist/esm/auth/credentials.js.map', 'dist/esm/auth/credentials.d.ts',
  'dist/commonjs/auth/credentials.js', 'dist/commonjs/auth/credentials.js.map', 'dist/commonjs/auth/credentials.d.ts',
];

function credentialFixture(value, name = '@typespec/ts-http-runtime', version = '0.3.9') {
  const source = fileURLToPath(new URL('../../', import.meta.url));
  const cosmos = resolveInstalled(source, '@azure/cosmos');
  const pipeline = resolveInstalled(cosmos, '@azure/core-rest-pipeline');
  const installed = resolveInstalled(pipeline, '@typespec/ts-http-runtime');
  const root = pkg(value, 'credential-code', name, version, { exports: { import: './dist/esm/auth/credentials.js', require: './dist/commonjs/auth/credentials.js' } });
  const originals = new Map();
  for (const relative of PUBLIC_CREDENTIAL_FILES) {
    const bytes = fs.readFileSync(path.join(installed, relative));
    originals.set(relative, bytes);
    fs.mkdirSync(path.dirname(path.join(root, relative)), { recursive: true, mode: 0o700 });
    fs.writeFileSync(path.join(root, relative), bytes);
  }
  write(path.join(root, 'dist/commonjs/package.json'), { type: 'commonjs' });
  direct(value, { [name]: root });
  return { root, originals };
}

function credentialOracle(root, home) {
  const entry = path.join(root, 'credential-oracle.mjs');
  write(entry, `import * as esm from '@typespec/ts-http-runtime';
import {createRequire} from 'node:module';
const commonjs=createRequire(import.meta.url)('@typespec/ts-http-runtime');
function check(api){return {
 oauth:api.isOAuth2TokenCredential({getOAuth2Token(){}}),
 bearer:api.isBearerTokenCredential({getBearerToken(){}}),
 basic:api.isBasicCredential({username:'synthetic',password:'synthetic'}),
 key:api.isApiKeyCredential({key:'synthetic'}),
 empty:api.isBasicCredential({}),
 incomplete:api.isBasicCredential({username:'synthetic'})
};}
process.stdout.write(JSON.stringify({esm:check(esm),commonjs:check(commonjs)})+'\\n');
`);
  const child = spawnSync(process.execPath, [entry], { shell: false, encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024, env: { PATH: '', HOME: home, USERPROFILE: home } });
  return { status: child.status, stderr: child.stderr, value: child.status === 0 ? JSON.parse(child.stdout) : null };
}

function materializeCredentials(value) {
  const graph = materializeProductionGraph(value.stage, value.source);
  return { graph, original: credentialOracle(value.source, value.root), installed: credentialOracle(value.target, value.root), paths: [...value.stage.entries.keys()] };
}

test('검증된 공개 SDK credential 검사 코드는 원문 그대로 복사하고 import와 require로 실행한다', t => {
  // given
  const value = graphFixture(t);
  const fixture = credentialFixture(value);
  const expected = { oauth: true, bearer: true, basic: true, key: true, empty: false, incomplete: false };
  // when
  const result = materializeCredentials(value);
  // then
  assert.equal(result.original.status, 0);
  assert.equal(result.installed.status, 0);
  assert.equal(result.installed.stderr, '');
  assert.deepEqual(result.installed.value, result.original.value);
  assert.deepEqual(result.installed.value, { esm: expected, commonjs: expected });
  assert.ok(PUBLIC_CREDENTIAL_FILES.every(relative => result.paths.includes(`node_modules/@typespec/ts-http-runtime/${relative}`)));
  assert.ok(PUBLIC_CREDENTIAL_FILES.every(relative => fs.readFileSync(value.stage.entries.get(`node_modules/@typespec/ts-http-runtime/${relative}`).path).equals(fixture.originals.get(relative))));
});

for (const relative of PUBLIC_CREDENTIAL_FILES) {
  test(`${relative}의 공개 원문 해시가 바뀌면 패키징을 거부한다`, t => {
    // given
    const value = graphFixture(t);
    const fixture = credentialFixture(value);
    const changed = Buffer.from(fixture.originals.get(relative));
    changed[changed.length - 1] ^= 1;
    fs.writeFileSync(path.join(fixture.root, relative), changed);
    // when
    const result = failure(value);
    // then
    assert.equal(result.error, 'INVALID_DEPENDENCY');
    assert.equal(result.graph, null);
  });
}

for (const [title, name, version] of [
  ['다른 패키지', '@fixture/ts-http-runtime', '0.3.9'],
  ['다른 버전', '@typespec/ts-http-runtime', '0.3.10'],
]) {
  test(`${title}의 credentials 파일에는 공개 코드 예외를 적용하지 않는다`, t => {
    // given
    const value = graphFixture(t);
    credentialFixture(value, name, version);
    // when
    const result = materialize(value);
    // then
    assert.ok(PUBLIC_CREDENTIAL_FILES.every(relative => !result.paths.includes(`node_modules/${name}/${relative}`)));
    assert.equal(result.graph.packages.length, 1);
  });
}

test('공개 SDK 코드 예외가 있어도 일반 비밀 파일과 다른 credential 경로는 제외한다', t => {
  // given
  const value = graphFixture(t);
  const fixture = credentialFixture(value);
  const secrets = ['credentials.json', 'credential', '.env', 'private.key', 'dist/esm/auth/credentials.extra.js', 'dist/other/credentials.js'];
  for (const relative of secrets) write(path.join(fixture.root, relative), 'SYNTHETIC_CREDENTIAL_DATA_CANARY');
  // when
  const result = materializeCredentials(value);
  // then
  assert.ok(secrets.every(relative => !result.paths.includes(`node_modules/@typespec/ts-http-runtime/${relative}`)));
  assert.ok([...value.stage.entries.values()].every(item => !fs.readFileSync(item.path).includes(Buffer.from('SYNTHETIC_CREDENTIAL_DATA_CANARY'))));
  assert.equal(result.installed.status, 0);
});

test('검증 대상 SDK 코드가 symlink이면 공개 코드 예외를 허용하지 않는다', t => {
  // given
  const value = graphFixture(t);
  const fixture = credentialFixture(value);
  const relative = PUBLIC_CREDENTIAL_FILES[0];
  const publicCopy = path.join(value.root, 'public-copy.js');
  fs.writeFileSync(publicCopy, fixture.originals.get(relative));
  fs.unlinkSync(path.join(fixture.root, relative));
  fs.symlinkSync(publicCopy, path.join(fixture.root, relative));
  // when
  const result = failure(value);
  // then
  assert.equal(result.error, 'INVALID_DEPENDENCY');
  assert.equal(result.graph, null);
});
