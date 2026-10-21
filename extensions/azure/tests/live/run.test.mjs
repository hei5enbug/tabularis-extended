import test from 'node:test';
import assert from 'node:assert/strict';
import { runLiveScenario } from '../../scripts/live/run.mjs';
import { DOCUMENTS, materialize, ordered, groupOracle } from '../../scripts/live/fixture.mjs';
import { CONFIG, SECRET, auth, syntheticScenario } from './synthetic-support.mjs';

test('37개 fixture는 고정 JSON과 누락 값을 보존하며 실행마다 전용 ID를 만든다', () => {
  // given
  const runId = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
  // when
  const documents = materialize(runId);
  // then
  assert.equal(documents.length, 37);
  assert.equal(new Set(documents.map(document => document.id)).size, 37);
  assert.ok(documents.every(document => document.run_id === runId && document.id.startsWith(runId) && document.tenant.startsWith(`${runId}:`)));
  assert.equal(Object.hasOwn(documents[1], 'optional'), false);
  assert.equal(documents[0].nested.present, null);
  assert.equal(Object.isFrozen(DOCUMENTS[0].nested.list), true);
  assert.equal(ordered(documents)[0].sort, 0);
  assert.equal(groupOracle(documents).reduce((count, group) => count + group.count, 0), 37);
});

test('합성 harness는 실제 검증 순서와 private cursor 재시작을 검사하고 Azure 증거로 표시하지 않는다', async () => {
  // given
  const synthetic = syntheticScenario(); const credentials = auth();
  // when
  const result = await runLiveScenario(CONFIG, credentials, { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 0);
  assert.equal(result.report.evidence_kind, 'synthetic_harness');
  assert.equal(result.report.integration_complete, false);
  assert.equal(result.report.source_commit, null);
  assert.equal(result.report.counts.fixture_documents, 37);
  assert.equal(result.report.counts.child_processes, 2);
  assert.equal(result.report.counts.leftovers, 0);
  assert.equal(synthetic.state.store.size, 0);
  assert.equal(synthetic.state.closed, 2);
  assert.ok(result.report.metrics.request_charge > 0);
  assert.equal(result.report.cleanup_private_state_removed, true);
  assert.equal(result.report.index_policy_hash, null);
  assert.ok(result.report.checks.some(check => check.name === 'cross_process_resume' && check.status === 'pass'));
  assert.ok(!JSON.stringify(result.report).includes(SECRET));
  assert.ok(synthetic.state.records.every(record => !['ping', 'test_connection', 'get_databases', 'get_tables'].includes(record.method)));
});

test('materialized 결과만 관찰하면 전체 snapshot을 검증해도 native resume는 exit4로 남긴다', async () => {
  // given
  const synthetic = syntheticScenario({ materialized: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.deepEqual(result.report.required_unobserved, ['native_cross_process_resume']);
  assert.equal(result.report.error, null);
  assert.equal(result.report.counts.child_processes, 1);
  assert.equal(result.report.counts.leftovers, 0);
  assert.ok(result.report.checks.some(check => check.name === 'order_by_37_no_duplicate_loss' && check.status === 'pass'));
});

test('유효 cursor 첫 재개가 실패하면 재시작만으로 resume 성공을 기록하지 않는다', async () => {
  // given
  const synthetic = syntheticScenario({ validResumeFails: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.error.code, 'INVALID_PAGE_TOKEN');
  assert.equal(result.report.checks.filter(check => check.name === 'cross_process_resume' && check.status === 'pass').length, 0);
  assert.equal(result.report.counts.child_processes, 2);
  assert.equal(result.report.counts.leftovers, 0);
});

test('첫 native 재개가 빈 page여도 cursor를 유지하고 37개 원본 검증을 완주한다', async () => {
  // given
  const synthetic = syntheticScenario({ emptyFirstResume: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 0);
  assert.equal(synthetic.state.emptyResumes, 1);
  assert.equal(result.report.checks.filter(check => check.name === 'cross_process_resume' && check.status === 'pass').length, 1);
  assert.ok(result.report.checks.some(check => check.name === 'order_by_37_no_duplicate_loss' && check.status === 'pass'));
  assert.equal(result.report.counts.leftovers, 0);
});

test('쓰기 응답이 unknown이면 생성 재전송 없이 소유권을 읽어 조건부 정리한다', async () => {
  // given
  const synthetic = syntheticScenario({ unknownCreate: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.error.code, 'OUTCOME_UNKNOWN');
  assert.equal(result.report.error.outcome, 'unknown');
  assert.equal(result.report.metrics.request_charge, null);
  assert.equal(synthetic.state.records.filter(record => record.method === 'create_document').length, 1);
  assert.equal(synthetic.state.deletes.length, 1);
  assert.equal(synthetic.state.store.size, 0);
  assert.equal(result.report.counts.leftovers, 0);
});

test('readonly principal의 실제 쓰기 성공은 실패로 기록하고 owner만 probe를 정리한다', async () => {
  // given
  const synthetic = syntheticScenario({ readonlyWrites: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.error.code, 'SCENARIO_MISMATCH');
  assert.equal(synthetic.state.store.size, 0);
  assert.ok(synthetic.state.records.filter(record => record.method === 'delete_document').every(record => record.params.driver_context.auth.identity === 'owner'));
});

test('다른 run 소유권을 가진 문서는 삭제하지 않고 hashed leftover를 남긴다', async () => {
  // given
  const synthetic = syntheticScenario({ foreignOwned: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.counts.leftovers, 1);
  assert.equal(synthetic.state.store.size, 1);
  assert.ok(result.report.leftovers.every(value => /^[0-9a-f]{64}$/.test(value)));
  assert.ok(!JSON.stringify(result.report).includes('foreign-owner'));
  assert.ok([...synthetic.state.store.keys()].every(key => !synthetic.state.deletes.includes(key)));
});

test('cleanup 응답이 unknown이면 자동 삭제 재시도 없이 불명확한 정리를 보고한다', async () => {
  // given
  const synthetic = syntheticScenario({ unknownCleanup: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.error.outcome, 'unknown');
  assert.equal(result.report.counts.leftovers, 1);
  assert.equal(result.report.metrics.request_charge, null);
  assert.equal(new Set(synthetic.state.deletes).size, synthetic.state.deletes.length);
});

test('partition metadata가 맞지 않으면 문서를 생성하기 전에 실패한다', async () => {
  // given
  const synthetic = syntheticScenario({ badMetadata: true });
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.counts.created, 0);
  assert.equal(synthetic.state.records.filter(record => record.method === 'create_document').length, 0);
});

test('전체 실행 deadline은 자식 시작 전에 확인한다', async () => {
  // given
  const synthetic = syntheticScenario(); let clock = 0;
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory, now: () => clock++, wholeTimeout: 1 });
  // then
  assert.equal(result.exitCode, 4);
  assert.equal(result.report.error.code, 'DEADLINE_EXCEEDED');
  assert.equal(synthetic.state.processes, 0);
});

test('사용자 취소는 성공 skip 대신 exit130과 정리 결과를 남긴다', async () => {
  // given
  const synthetic = syntheticScenario(); const controller = new AbortController(); controller.abort();
  // when
  const result = await runLiveScenario(CONFIG, auth(), { rpcFactory: synthetic.factory, signal: controller.signal });
  // then
  assert.equal(result.exitCode, 130);
  assert.equal(result.report.error.code, 'CANCELLED');
  assert.equal(result.report.cleanup_private_state_removed, true);
});
