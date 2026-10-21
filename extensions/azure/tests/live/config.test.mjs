import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { parseArguments, readConfig, validateConfig } from '../../scripts/live/config.mjs';
import { main, writeReport } from '../../scripts/live/cli.mjs';
import { CONFIG, SECRET, privateFixture, capture, auth } from './synthetic-support.mjs';

test('승인 플래그와 서로 다른 두 FD를 명시해야 CLI 입력을 수용한다', t => {
  // given
  const value = privateFixture(t);
  const args = ['--config', value.config, '--credential-fd', '3', '--readonly-credential-fd', '255', '--allow-live', '--allow-fixture-writes'];
  // when
  const result = parseArguments(args);
  // then
  assert.equal(result.config, value.config);
  assert.equal(result.credentialFd, 3);
  assert.equal(result.readonlyCredentialFd, 255);
});

for (const [title, extra] of [
  ['TLS 검증 우회', { endpoint: 'http://synthetic.documents.azure.com' }],
  ['Mongo API', { endpoint: 'https://synthetic.mongo.cosmos.azure.com' }],
  ['다른 cloud', { endpoint: 'https://synthetic.documents.azure.cn' }],
  ['사용자 정보', { endpoint: 'https://user@synthetic.documents.azure.com' }],
  ['endpoint 경로', { endpoint: 'https://synthetic.documents.azure.com/other' }],
  ['임의 query', { endpoint: 'https://synthetic.documents.azure.com/?key=x' }],
  ['비전용 container', { container: 'existing_user_documents' }],
  ['동일 container', { hierarchical_container: CONFIG.container }],
  ['시스템 DB', { database: 'master' }],
  ['control 문자', { database: 'bad\u0000db' }],
  ['비밀 설정', { account_key: SECRET }],
  ['credential 참조', { credential_ref: 'existing' }],
  ['Entra ID 누락', { auth_mode: 'entra_user' }],
]) {
  test(`${title} 설정은 고정 오류로 거부한다`, () => {
    // given
    const value = { ...CONFIG, ...extra };
    // when
    const result = capture(() => validateConfig(value));
    // then
    assert.equal(result.code, 'INVALID_CONFIG');
    assert.ok(!JSON.stringify(result).includes(SECRET));
  });
}

test('private regular 설정 파일은 public endpoint로 정규화한다', t => {
  // given
  const value = privateFixture(t);
  // when
  const config = readConfig(value.config);
  // then
  assert.deepEqual(config, CONFIG);
});

for (const [title, mutation] of [
  ['symlink', value => { const linked = path.join(value.root, 'linked.json'); fs.symlinkSync(value.config, linked); return linked; }],
  ['과대 크기', value => { fs.writeFileSync(value.config, 'x'.repeat(65537)); return value.config; }],
  ['잘못된 UTF-8', value => { fs.writeFileSync(value.config, Buffer.from([0xff])); return value.config; }],
  ...(process.platform === 'win32' ? [] : [['공개 권한', value => { fs.chmodSync(value.config, 0o644); return value.config; }]]),
]) {
  test(`${title} 설정 파일은 읽기 경계에서 거부한다`, t => {
    // given
    const value = privateFixture(t);
    const file = mutation(value);
    // when
    const result = capture(() => readConfig(file));
    // then
    assert.equal(result.code, 'INVALID_CONFIG');
  });
}

for (const [title, args] of [
  ['승인 누락', ['--config', '/synthetic/config', '--credential-fd', '3', '--readonly-credential-fd', '4']],
  ['FD 누락', ['--config', '/synthetic/config', '--allow-live', '--allow-fixture-writes']],
  ['같은 FD', ['--config', '/synthetic/config', '--credential-fd', '3', '--readonly-credential-fd', '3', '--allow-live', '--allow-fixture-writes']],
  ['상대 경로', ['--config', 'config.json', '--credential-fd', '3', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes']],
  ['표준 입력 FD', ['--config', '/synthetic/config', '--credential-fd', '0', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes']],
  ['중복 인자', ['--allow-live', '--allow-live']],
  ['알 수 없는 인자', ['--account-key', SECRET]],
]) {
  test(`${title} CLI는 credential 읽기와 실행 없이 exit2를 반환한다`, async () => {
    // given
    let reads = 0; let starts = 0; const outputs = [];
    // when
    const code = await main(args, { configReader: () => CONFIG, credentialReader: () => { reads += 1; }, scenario: () => { starts += 1; }, output: value => outputs.push(value), noSignals: true });
    // then
    assert.equal(code, 2);
    assert.equal(reads, 0);
    assert.equal(starts, 0);
    assert.equal(outputs[0].status, 'failed');
    assert.ok(!JSON.stringify(outputs).includes(SECRET));
  });
}

test('설정 검증 실패는 두 credential FD 모두 읽기 전에 종료한다', async () => {
  // given
  let reads = 0; const outputs = [];
  const args = ['--config', '/synthetic/config', '--credential-fd', '3', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes'];
  // when
  const code = await main(args, { configReader: () => validateConfig({ ...CONFIG, secret: SECRET }), credentialReader: () => { reads += 1; }, output: value => outputs.push(value), noSignals: true });
  // then
  assert.equal(code, 2);
  assert.equal(reads, 0);
  assert.equal(outputs[0].code, 'INVALID_CONFIG');
});

test('Windows ACL 허용은 설정 검증 뒤 credential과 시나리오 실행을 허용한다', async () => {
  // given
  const credentials = auth(); const events = []; const outputs = [];
  const args = ['--config', '/synthetic/config', '--credential-fd', '3', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes'];
  // when
  const code = await main(args, {
    platform: 'win32',
    ownerAclChecker: () => events.push('acl'),
    configReader: () => { events.push('config'); return CONFIG; },
    credentialReader: fd => { events.push('credential'); return fd === 3 ? credentials.owner : credentials.readonly; },
    scenario: async () => { events.push('scenario'); return { report: { evidence_kind: 'synthetic_harness' }, exitCode: 0 }; },
    output: result => outputs.push(result),
    noSignals: true,
  });
  // then
  assert.equal(code, 0);
  assert.deepEqual(events, ['acl', 'config', 'credential', 'credential', 'scenario']);
  assert.deepEqual(outputs, [{ evidence_kind: 'synthetic_harness' }]);
  assert.equal(credentials.owner.account_key, '');
  assert.equal(credentials.readonly.account_key, '');
});

for (const [title, deniedTarget] of [
  ['설정 파일', 'config'],
  ['보고서 디렉터리', 'report'],
]) {
  test(`Windows ${title} ACL 거부는 설정, credential, 시나리오를 시작하지 않는다`, async t => {
    // given
    const value = privateFixture(t); let configReads = 0; let credentialReads = 0; let starts = 0; const outputs = [];
    const report = path.join(value.root, 'report.json');
    const denied = deniedTarget === 'config' ? value.config : value.root;
    const args = ['--config', value.config, '--credential-fd', '3', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes', ...(deniedTarget === 'report' ? ['--report', report] : [])];
    // when
    const code = await main(args, {
      platform: 'win32',
      ownerAclChecker: target => { if (target === denied) throw new Error(SECRET); },
      configReader: () => { configReads += 1; return CONFIG; },
      credentialReader: () => { credentialReads += 1; return auth().owner; },
      scenario: () => { starts += 1; },
      output: result => outputs.push(result),
      noSignals: true,
    });
    // then
    assert.equal(code, 2);
    assert.equal(configReads, 0);
    assert.equal(credentialReads, 0);
    assert.equal(starts, 0);
    assert.equal(outputs[0].code, deniedTarget === 'config' ? 'INVALID_CONFIG' : 'INVALID_REPORT_PATH');
    assert.ok(!JSON.stringify(outputs).includes(SECRET));
  });
}

async function reportTwice(file) {
  writeReport(file, { evidence_kind: 'synthetic_harness', count: 37 });
  return capture(() => writeReport(file, { count: 0 }));
}
test('보고서는 private 파일로 게시하고 기존 결과를 덮어쓰지 않는다', async t => {
  // given
  const value = privateFixture(t); const file = path.join(value.root, 'report.json');
  // when
  const result = await reportTwice(file);
  // then
  assert.equal(result.code, 'INVALID_REPORT_PATH');
  assert.deepEqual(JSON.parse(fs.readFileSync(file)), { evidence_kind: 'synthetic_harness', count: 37 });
  assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  assert.equal(fs.readdirSync(value.root).filter(name => name.endsWith('.tmp')).length, 0);
});

test('CLI 종료는 전달받은 credential 값을 해제하고 보고서에 포함하지 않는다', async t => {
  // given
  const value = privateFixture(t); const credentials = auth(); const outputs = [];
  const args = ['--config', value.config, '--credential-fd', '3', '--readonly-credential-fd', '4', '--allow-live', '--allow-fixture-writes'];
  // when
  const code = await main(args, { credentialReader: fd => fd === 3 ? credentials.owner : credentials.readonly, scenario: async () => ({ report: { evidence_kind: 'synthetic_harness' }, exitCode: 0 }), output: report => outputs.push(report), noSignals: true });
  // then
  assert.equal(code, 0);
  assert.equal(credentials.owner.account_key, '');
  assert.equal(credentials.readonly.account_key, '');
  assert.ok(!JSON.stringify(outputs).includes(SECRET));
});
