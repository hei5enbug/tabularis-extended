import { useEffect, useMemo, useState } from 'react';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import type { ServiceResponse } from '@tabularis/service-contracts/types';
import { errorText, parameters, resultData, UiError, validatePartition, type ContainerMetadata, type Origin, type ResultData, type SavedConnection } from './models';
import type { DocumentSelection } from './DocumentPanel';
import { keyComponents, PartitionKeyFields, type KeyField } from './PartitionKeyFields';
import { RequestScope } from './service';

interface QueryPage { response: ServiceResponse; data: ResultData; origin: Origin }
export function QueryPanel({ service, connection, database, container, meta, onSelect }: { service: UsePluginServiceReturn; connection: SavedConnection; database: string; container: string; meta: ContainerMetadata | null; onSelect: (selection: DocumentSelection) => void }) {
  const scope = useMemo(() => new RequestScope(service), [service]);
  const [text, setText] = useState('SELECT * FROM c');
  const [parameterText, setParameterText] = useState('[]');
  const [mode, setMode] = useState<'json_values' | 'documents'>('json_values');
  const [pageSize, setPageSize] = useState('100');
  const [ruBudget, setRuBudget] = useState('100');
  const [targeted, setTargeted] = useState(false);
  const [keyCount, setKeyCount] = useState(1);
  const [keys, setKeys] = useState<KeyField[]>([]);
  const [pages, setPages] = useState<QueryPage[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [failedResponse, setFailedResponse] = useState<ServiceResponse | null>(null);
  useEffect(() => () => scope.cancel(), [scope]);
  useEffect(() => { scope.cancel(); setPages([]); setBusy(false); setError(''); setMessage(''); setFailedResponse(null); setTargeted(false); setKeyCount(1); setKeys(meta?.partition_key_paths.map(() => ({ type: 'string', value: '' })) || []); }, [database, container, meta, scope]);
  const run = async (action: () => Promise<void>) => { const generation = scope.generation; setBusy(true); setError(''); setFailedResponse(null); setMessage('쿼리 결과를 기다리고 있습니다.'); try { await action(); if (generation === scope.generation) setMessage('쿼리 페이지를 받았습니다.'); } catch (failure) { if (generation === scope.generation) { setMessage(''); setError(errorText(failure)); if (failure instanceof UiError && failure.response) setFailedResponse(failure.response); } } finally { if (generation === scope.generation) setBusy(false); } };
  const last = pages.at(-1);
  const totalRu = pages.reduce<number | null>((sum, page) => {
    const charge = page.response.metrics.request_charge;
    return sum === null || typeof charge !== 'number' ? null : sum + charge;
  }, 0);
  return <section aria-label="쿼리"><h3>쿼리</h3><form onSubmit={event => { event.preventDefault(); scope.cancel(); setPages([]); void run(async () => {
    const size = Number(pageSize), budget = Number(ruBudget);
    if (!text.trim() || !database || !container || !Number.isInteger(size) || size < 1 || size > 1000 || !Number.isFinite(budget) || budget <= 0) throw new UiError('INVALID_QUERY_OPTIONS');
    let partition_key = null;
    if (targeted) { if (!meta) throw new UiError('METADATA_UNAVAILABLE'); partition_key = keyComponents(keys.slice(0, keyCount)); validatePartition(partition_key, meta, true); }
    const origin = { database, container };
    const response = await scope.call('query.execute', connection.connection_id, { language: 'cosmos_sql', text, parameters: parameters(parameterText), page_size: size, database, container, partition_key, ru_budget: budget, result_mode: mode });
    setPages([{ response, data: resultData(response.data), origin }]);
  }); }}>
    <label>Cosmos SQL<textarea rows={5} value={text} spellCheck={false} onChange={event => setText(event.target.value)} /></label>
    <label>이름 있는 매개변수 JSON<textarea rows={3} value={parameterText} spellCheck={false} onChange={event => setParameterText(event.target.value)} /></label>
    <div className="cosmos-fields"><label>페이지 크기<input type="number" min={1} max={1000} value={pageSize} onChange={event => setPageSize(event.target.value)} /></label><label>RU 예산<input type="number" min="0.001" step="any" value={ruBudget} onChange={event => setRuBudget(event.target.value)} /></label><label>결과 형식<select value={mode} onChange={event => setMode(event.target.value as 'json_values' | 'documents')}><option value="json_values">JSON 값 (읽기 전용)</option><option value="documents">원본 문서</option></select></label></div>
    <label className="cosmos-checkbox"><input type="checkbox" checked={targeted} disabled={!meta} onChange={event => setTargeted(event.target.checked)} />파티션 범위 지정</label>
    {targeted && meta && <><label>앞에서부터 사용할 구성 요소<select value={keyCount} onChange={event => setKeyCount(Number(event.target.value))}>{meta.partition_key_paths.map((_, index) => <option key={index} value={index + 1}>{index + 1}</option>)}</select></label><PartitionKeyFields paths={meta.partition_key_paths.slice(0, keyCount)} fields={keys.slice(0, keyCount)} onChange={changed => setKeys([...changed, ...keys.slice(keyCount)])} partial label="쿼리 파티션 키" /></>}
    <button disabled={busy || !database || !container} type="submit">쿼리 실행</button>
  </form>
  {pages.map((page, pageIndex) => <div key={pageIndex} className="cosmos-page"><h4>페이지 {pageIndex + 1}</h4><p>RU: {page.response.metrics.request_charge ?? '정보 없음'} · 재시도: {page.response.metrics.retry_count} · 결과: {page.response.status}</p>
    {page.response.limits.truncated && <p role="status">결과 제한: {page.response.limits.reasons.join(', ')}</p>}
    {page.response.warnings.map((warning, index) => <p role="status" key={index}>{warning}</p>)}
    {page.data.kind === 'documents' ? page.data.documents.map((document, index) => <div key={index}><button onClick={() => onSelect({ entry: document, origin: page.origin, selection_id: crypto.randomUUID() })}>페이지 {pageIndex + 1} 문서 {index + 1} 보기</button><pre>{JSON.stringify(document.document, null, 2)}</pre></div>) : page.data.values.map((value, index) => <pre key={index}>{JSON.stringify(value, null, 2)}</pre>)}
    {!page.data.values.length && !page.data.documents.length && <p>이 페이지에 결과가 없습니다.</p>}
  </div>)}
  {last?.response.page.has_more && <button disabled={busy || !last.response.page.next_token} onClick={() => void run(async () => { const response = await scope.call('query.next', connection.connection_id, { next_token: last.response.page.next_token! }); setPages(previous => [...previous, { response, data: resultData(response.data), origin: last.origin }]); })}>다음 페이지</button>}
  {last && <p role="status">{last.response.page.has_more ? '다음 페이지가 있습니다.' : '마지막 페이지입니다.'} · 총 RU: {totalRu ?? '정보 없음'}</p>}
  {busy && <button onClick={() => { scope.cancel(); setBusy(false); setMessage('쿼리 취소를 요청했습니다.'); }}>쿼리 취소</button>}
  {failedResponse && <div><p role="status">RU: {failedResponse.metrics.request_charge ?? '정보 없음'} · 재시도: {failedResponse.metrics.retry_count}</p>{failedResponse.limits.truncated && <p role="status">결과 제한: {failedResponse.limits.reasons.join(', ')}</p>}{failedResponse.warnings.map((warning, index) => <p role="status" key={index}>{warning}</p>)}</div>}
  {message && <p role="status">{message}</p>}{error && <p role="alert">{error}</p>}
  </section>;
}
