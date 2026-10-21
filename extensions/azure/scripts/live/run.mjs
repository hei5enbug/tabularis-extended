import fs from 'node:fs';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { NativeRpc, wire, privateWorkspace, safeFailure } from './rpc.mjs';
import { LiveError, object, privateFile } from './config.mjs';
import { materialize, stable, hash, userDocument, identity, ordered, groupOracle, component } from './fixture.mjs';

function ensure(condition) { if (!condition) throw new LiveError('SCENARIO_MISMATCH'); }
function values(output) {
  ensure(object(output) && object(output.data) && object(output.snapshot) && Array.isArray(output.snapshot.sets) && output.snapshot.sets.length === 1);
  const page = output.data.kind === 'documents' ? output.data.documents : output.data.kind === 'json_values' ? output.data.values : null;
  ensure(Array.isArray(page) && Array.isArray(output.snapshot.sets[0].rows));
  return page;
}
export async function runLiveScenario(config, credentials, { driver, rpcFactory, signal, now = Date.now, wholeTimeout = 600_000 } = {}) {
  const started = now(); const deadline = started + Math.min(600_000, wholeTimeout);
  const runId = randomUUID(); const connectionId = randomUUID(); const readonlyId = randomUUID();
  const documents = materialize(runId); const expected = ordered(documents);
  const owned = new Map(); const checks = []; const requiredUnobserved = [];
  const privateRoot = privateWorkspace(); let client; let created = 0; let cleaned = 0; let processes = 0;
  const privateProfiles = [];
  let knownCharge = 0; let unknownCharge = false; let retries = 0; let error = null;
  const factory = rpcFactory ?? (() => new NativeRpc({ driver }));
  const addCheck = (name, status = 'pass', evidence = 'native_rpc') => checks.push({ name, status, evidence });
  function remaining() {
    if (now() >= deadline) throw new LiveError('DEADLINE_EXCEEDED');
    return Math.max(1, Math.min(120_000, deadline - now()));
  }
  function metrics(response) {
    const value = response?.metrics;
    if (!value) return;
    if (typeof value.request_charge === 'number' && Number.isFinite(value.request_charge) && value.request_charge >= 0) knownCharge += value.request_charge;
    else unknownCharge = true;
    if (Number.isSafeInteger(value.retry_count) && value.retry_count >= 0) retries += value.retry_count;
  }
  async function start() {
    remaining(); client = factory(); processes += 1;
    if (typeof client.profile === 'string') privateProfiles.push(client.profile);
    const result = await client.request('initialize', { settings: {}, service_protocol: 1 }, { timeout: remaining(), signal });
    ensure(result?.service_capabilities?.service_protocol === 1 && result.service_capabilities.documents_v1 === true && result.service_capabilities.query_page_v1 === true && result.service_capabilities.cancel_v1 === true);
  }
  async function invoke(method, input, { auth = credentials.owner, connection = connectionId, readOnly = true, write = false, cleanup = false, invalidate = false } = {}) {
    const timeout = remaining();
    const request = wire(config, auth, connection, input, { readOnly, invalidate, deadline: timeout });
    let response;
    try { response = await client.request(method, request, { write, timeout, signal: cleanup ? undefined : signal }); }
    catch (failure) { if (failure.outcome === 'unknown' || ['DEADLINE_EXCEEDED', 'CANCELLED', 'DRIVER_EXITED'].includes(failure.code)) unknownCharge = true; throw failure; }
    metrics(response);
    if (!object(response) || response.protocol_version !== 1 || response.request_id !== request.driver_context.request_id || response.connection_id !== connection || !['succeeded', 'failed', 'cancelled', 'outcome_unknown'].includes(response.status)) throw new LiveError('INVALID_RPC_RESPONSE', write ? 'unknown' : 'not_applied');
    if (response.status !== 'succeeded' || response.error) throw safeFailure(response.error, write);
    return response.data;
  }
  async function expectedFailure(name, codes, action, evidence = 'native_rpc') {
    let observed;
    try { await action(); } catch (failure) { observed = failure; }
    ensure(observed instanceof LiveError && codes.includes(observed.code) && observed.outcome !== 'unknown');
    addCheck(name, 'pass', evidence);
  }
  function track(container, document, hierarchical = false) {
    const item = { container, identity: identity(document, hierarchical) };
    owned.set(stable(item), item);
    return item;
  }
  async function create(container, document, hierarchical = false, options = {}) {
    const item = track(container, document, hierarchical);
    const result = await invoke('create_document', { database: config.database, container, document, partition_key: item.identity.partition_key }, { ...options, write: true, readOnly: false });
    ensure(object(result?.document) && result.document.id === document.id); created += 1;
    return result;
  }
  async function read(item, options = {}) { return invoke('read_document', { database: config.database, ...item }, options); }
  async function remove(item, cleanup = false) {
    let found;
    try { found = await read(item, { cleanup }); } catch (failure) { if (failure.code === 'DOCUMENT_NOT_FOUND') return true; throw failure; }
    ensure(found?.document?.run_id === runId && found.document.id === item.identity.id && typeof found.etag === 'string'
      && stable(found.identity) === stable(item.identity) && stable(identity(found.document, item.identity.partition_key.length === 2)) === stable(item.identity));
    const result = await invoke('delete_document', { database: config.database, ...item, if_match: found.etag }, { write: true, readOnly: false, cleanup });
    ensure(result?.deleted === true); cleaned += 1;
    return true;
  }
  const query = (text, resultMode = 'json_values', container = config.container, parameters = [{ name: '@run', value: runId }]) => ({ language: 'cosmos_sql', database: config.database, container, text, parameters, page_size: 3, result_mode: resultMode });
  async function pages(input, restart = false) {
    let output = await invoke('query_page', { query: input });
    let rows = values(output);
    const native = output.cursor !== null;
    if (!native) {
      rows = output.snapshot.sets[0].rows;
      if (restart) { requiredUnobserved.push('native_cross_process_resume'); addCheck('cross_process_resume', 'not_observed', 'materialized_snapshot'); }
      return { rows, native: false, materialized: true };
    }
    ensure(rows.length <= 3);
    if (restart) {
      const file = path.join(privateRoot, 'cursor.json');
      fs.writeFileSync(file, JSON.stringify(output.cursor), { mode: 0o600, flag: 'wx' });
      const cursor = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(privateFile(file, 16 * 1024 * 1024)));
      await client.close(); await start();
      const corruptions = [
        ['changed_query', { ...input, text: `${input.text} ` }, structuredClone(cursor), connectionId],
        ['changed_parameters', { ...input, parameters: [{ name: '@run', value: 'different-synthetic-run' }] }, structuredClone(cursor), connectionId],
        ['changed_connection', input, structuredClone(cursor), randomUUID()],
        ['changed_sdk', input, { ...structuredClone(cursor), binding: { ...cursor.binding, sdk_version: '0.0.0' } }, connectionId],
        ['changed_container_rid', input, { ...structuredClone(cursor), native_state: { ...cursor.native_state, container_rid: 'different-synthetic-rid' } }, connectionId],
      ];
      for (const [name, candidate, state, connection] of corruptions) await expectedFailure(name, ['INVALID_PAGE_TOKEN'], () => invoke('query_page', { query: candidate, cursor_state: state }, { connection }), 'local_cursor_contract');
      ensure(stable(cursor) === stable(output.cursor));
      output.cursor = cursor; fs.unlinkSync(file);
    }
    let count = 0; let resumed = false;
    while (output.cursor !== null) {
      ensure(++count <= 100 && rows.length <= 10000);
      output = await invoke('query_page', { query: input, cursor_state: output.cursor });
      const next = values(output); ensure(next.length <= 3);
      if (restart && !resumed) { addCheck('cross_process_resume'); resumed = true; }
      rows = rows.concat(next);
    }
    return { rows, native: true, materialized: false };
  }
  try {
    await start();
    for (const [container, paths, kind] of [[config.container, ['/tenant'], 'Hash'], [config.hierarchical_container, ['/tenant', '/region'], 'MultiHash']]) {
      const metadata = await invoke('get_columns', { table: { database: config.database, schema: null, table: container } });
      ensure(Array.isArray(metadata?.columns) && stable(metadata.partition_key_paths) === stable(paths) && metadata.partition_key_kind === kind && metadata.system_key === false);
      ensure(kind !== 'MultiHash' || metadata.partition_key_version === 2);
    }
    addCheck('administrator_precreated_partition_metadata');
    for (const document of documents) await create(config.container, document);
    addCheck('create_37_documents');
    const item = track(config.container, documents[0]);
    await expectedFailure('duplicate_identity_409', ['DOCUMENT_ALREADY_EXISTS'], () => create(config.container, documents[0]));
    const other = { ...structuredClone(documents[0]), tenant: `${runId}:other` };
    await create(config.container, other); await remove(track(config.container, other)); addCheck('same_id_other_partition');
    for (const document of documents) {
      const found = await read(track(config.container, document));
      ensure(stable(userDocument(found.document)) === stable(document) && typeof found.etag === 'string' && found.document._etag === found.etag
        && typeof found.document._rid === 'string' && typeof found.document._ts === 'number');
    }
    addCheck('point_read_json_and_system_fields');
    const sorted = await pages(query('SELECT * FROM c WHERE c.run_id=@run ORDER BY c.sort,c.id', 'documents'), true);
    ensure(sorted.rows.length === 37 && new Set(sorted.rows.map(entry => entry.identity.id)).size === 37);
    ensure(stable(sorted.rows.map(entry => userDocument(entry.document))) === stable(expected)); addCheck('order_by_37_no_duplicate_loss');
    const filtered = await pages(query('SELECT c.id,c.status,c.nested FROM c WHERE c.run_id=@run AND c.status=@status ORDER BY c.sort,c.id', 'json_values', config.container, [{ name: '@run', value: runId }, { name: '@status', value: 'active' }]));
    ensure(stable(filtered.rows) === stable(expected.filter(document => document.status === 'active').map(({ id, status, nested }) => ({ id, status, nested })))); addCheck('filter_projection');
    for (const [expression, oracle] of [['null', expected.map(() => null)], ['c.nested.list', expected.map(document => document.nested.list)], ['c.id', expected.map(document => document.id)], ['c.amount', expected.map(document => document.amount)]]) {
      const result = await pages(query(`SELECT VALUE ${expression} FROM c WHERE c.run_id=@run ORDER BY c.sort,c.id`));
      ensure(stable(result.rows) === stable(oracle));
    }
    addCheck('scalar_null_array_string_number');
    for (const [expression, oracle] of [['COUNT(1)', 37], ['SUM(c.amount)', documents.reduce((sum, document) => sum + document.amount, 0)]]) {
      const result = await pages(query(`SELECT VALUE ${expression} FROM c WHERE c.run_id=@run`)); ensure(stable(result.rows) === stable([oracle]));
    }
    const groups = await pages(query('SELECT c.status,COUNT(1) AS count,SUM(c.amount) AS total FROM c WHERE c.run_id=@run GROUP BY c.status'));
    ensure(groups.materialized && stable([...groups.rows].sort((a, b) => a.status.localeCompare(b.status, 'en'))) === stable(groupOracle(documents))); addCheck('complete_count_sum_group_by');
    const original = await read(item); const replacement = { ...structuredClone(documents[0]), amount: 999 };
    const replaced = await invoke('replace_document', { database: config.database, ...item, if_match: original.etag, document: replacement }, { readOnly: false, write: true });
    ensure(stable(userDocument(replaced.document)) === stable(replacement));
    await expectedFailure('stale_etag_412', ['ETAG_CONFLICT'], () => invoke('replace_document', { database: config.database, ...item, if_match: original.etag, document: replacement }, { readOnly: false, write: true }));
    for (const [name, change] of [['id_immutable', { id: `${runId}-changed` }], ['partition_immutable', { tenant: `${runId}:changed` }]]) {
      await expectedFailure(name, ['PARTITION_KEY_IMMUTABLE'], () => invoke('replace_document', { database: config.database, ...item, if_match: replaced.etag, document: { ...replacement, ...change } }, { readOnly: false, write: true }), 'local_document_contract');
    }
    addCheck('etag_replace');
    for (const [index, region] of ['east', 42, null, undefined, true].entries()) {
      const document = { id: `${runId}-hpk`, run_id: runId, tenant: `${runId}:hpk`, amount: index, ...(region === undefined ? {} : { region }) };
      const result = await create(config.hierarchical_container, document, true);
      const target = track(config.hierarchical_container, document, true);
      const found = await read(target); ensure(stable(userDocument(found.document)) === stable(document));
      const updated = await invoke('replace_document', { database: config.database, ...target, if_match: result.etag, document: { ...document, amount: index + 10 } }, { readOnly: false, write: true });
      ensure(updated.document.amount === index + 10); await remove(target);
    }
    addCheck('hierarchical_typed_full_partition_crud');
    const readonly = await invoke('query_page', { query: query('SELECT VALUE COUNT(1) FROM c WHERE c.run_id=@run') }, { auth: credentials.readonly, connection: readonlyId });
    ensure(stable(values(readonly)) === stable([37])); addCheck('restricted_principal_query');
    const denied = { id: `${runId}-readonly-probe`, tenant: `${runId}:probe`, run_id: runId };
    track(config.container, denied);
    await expectedFailure('database_readonly_write_denied', ['PERMISSION_DENIED', 'WRITE_NOT_ALLOWED'], () => create(config.container, denied, false, { auth: credentials.readonly, connection: readonlyId }));
    await expectedFailure('application_readonly_write_denied', ['WRITE_NOT_ALLOWED'], () => invoke('create_document', { database: config.database, container: config.container, document: denied, partition_key: [component(denied.tenant)] }, { readOnly: true, write: true }), 'local_policy');
    const expired = { kind: 'entra_token', access_token: 'expired-local-contract-token', expires_at_ms: started - 1, tenant_id: config.tenant_id ?? randomUUID(), client_id: config.client_id ?? randomUUID(), principal_id: randomUUID(), scope: 'https://cosmos.azure.com/.default', identity: 'expired-local-contract' };
    await expectedFailure('expired_supplied_context', ['AUTH_EXPIRED', 'AUTH_REQUIRED'], () => read(item, { auth: expired }), 'local_auth_contract');
    const invalidated = await invoke('service_invalidate_auth', {}, { invalidate: true }); ensure(invalidated?.invalidated === true);
    const refreshed = await read(item); ensure(refreshed.document.id === item.identity.id); addCheck('invalidate_and_new_login', 'pass', 'cache_invalidation_not_token_revocation');
    ensure(knownCharge > 0); addCheck('positive_observed_request_charge');
  } catch (failure) {
    error = { code: failure instanceof LiveError ? failure.code : 'HARNESS_FAILURE', outcome: failure instanceof LiveError ? failure.outcome : 'unknown' };
    addCheck('scenario', 'fail');
  } finally {
    if (client) {
      for (const [key, item] of owned) {
        try { if (await remove(item, true)) owned.delete(key); }
        catch (failure) { addCheck('owned_document_cleanup', 'fail'); if (!error) error = { code: failure instanceof LiveError ? failure.code : 'CLEANUP_FAILURE', outcome: failure instanceof LiveError ? failure.outcome : 'unknown' }; }
      }
      await client.close().catch(() => { if (!error) error = { code: 'DRIVER_EXITED', outcome: 'unknown' }; });
    }
    fs.rmSync(privateRoot, { recursive: true, force: true });
  }
  const report = {
    version: 1, evidence_kind: rpcFactory ? 'synthetic_harness' : 'actual_azure', engine: 'Azure Cosmos DB NoSQL', auth_mode: config.auth_mode,
    sdk_version: '4.10.1', node_version: process.version, platform: process.platform, architecture: process.arch,
    source_commit: null, source_sha256: hash(fs.readFileSync(new URL('../../dist/index.js', import.meta.url)).toString('utf8')),
    integration_complete: false, index_policy_hash: null, index_policy_verification: 'administrator_prepared_not_observed',
    deferred: ['external_indexing_policy_evidence', 'interactive_auth_and_token_refresh', 'host_gui_adapter_parity', 'actual_rate_limit_fault'],
    required_unobserved: requiredUnobserved, checks, error, counts: { fixture_documents: 37, created, cleaned, child_processes: processes, leftovers: owned.size },
    leftovers: [...owned.values()].map(item => hash(item)), metrics: { request_charge: unknownCharge ? null : knownCharge, known_request_charge: knownCharge, retry_count: retries },
    elapsed_ms: Math.max(0, now() - started), cleanup_private_state_removed: !fs.existsSync(privateRoot) && privateProfiles.every(profile => !fs.existsSync(profile)),
  };
  return { report, exitCode: signal?.aborted ? 130 : error || owned.size || requiredUnobserved.length ? 4 : 0 };
}
