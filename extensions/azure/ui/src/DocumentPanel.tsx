import { useEffect, useMemo, useState } from 'react';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import { documentPayload, entry, errorText, metadata, partitionFromDocument, samePartition, UiError, validateId, validatePartition, type ContainerMetadata, type DocumentEntry, type Origin, type SavedConnection } from './models';
import { keyComponents, PartitionKeyFields, type KeyField } from './PartitionKeyFields';
import { RequestScope } from './service';

export interface DocumentSelection { entry: DocumentEntry; origin: Origin | null; selection_id: string }
function fieldsFor(document: DocumentEntry, paths: string[]): KeyField[] {
  return paths.map((_, index) => { const part = document.identity.partition_key[index]; return part ? { type: part.type, value: 'value' in part ? String(part.value) : '' } : { type: 'string', value: '' }; });
}
export function DocumentPanel({ service, connection, database, container, meta, selection }: { service: UsePluginServiceReturn; connection: SavedConnection; database: string; container: string; meta: ContainerMetadata | null; selection: DocumentSelection | null }) {
  const scope = useMemo(() => new RequestScope(service), [service]);
  const [id, setId] = useState('');
  const [keys, setKeys] = useState<KeyField[]>([]);
  const [current, setCurrent] = useState<DocumentEntry | null>(null);
  const [draftBase, setDraftBase] = useState<DocumentEntry | null>(null);
  const [verified, setVerified] = useState<{ origin: Origin; meta: ContainerMetadata } | null>(null);
  const [text, setText] = useState('');
  const [dirty, setDirty] = useState(false);
  const [creating, setCreating] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  useEffect(() => () => scope.cancel(), [scope]);
  useEffect(() => {
    scope.cancel(); setKeys(meta?.partition_key_paths.map(() => ({ type: 'string', value: '' })) || []); setVerified(null); setCreating(false); setBusy(false); setError('');
  }, [database, container, meta, scope]);
  useEffect(() => {
    scope.cancel(); setCurrent(selection?.entry || null); setDraftBase(selection?.entry || null); setVerified(null); setCreating(false); setBusy(false); setText(selection ? JSON.stringify(selection.entry.document, null, 2) : ''); setDirty(false); setConfirmDelete(false); setError(''); setMessage('');
    if (selection) { setId(selection.entry.identity.id); if (meta) setKeys(fieldsFor(selection.entry, meta.partition_key_paths)); }
  }, [selection, scope]);
  const run = async (action: () => Promise<void>) => {
    const generation = scope.generation;
    setBusy(true); setError(''); setMessage('승인 또는 작업 완료를 기다리고 있습니다.');
    try { await action(); } catch (failure) { if (generation === scope.generation) { setMessage(''); setError(errorText(failure)); } } finally { if (generation === scope.generation) setBusy(false); }
  };
  const accept = (next: DocumentEntry, origin: Origin, nextMeta: ContainerMetadata, preserveDraft: boolean) => {
    validatePartition(next.identity.partition_key, nextMeta);
    if (!samePartition(partitionFromDocument(next.document, nextMeta), next.identity.partition_key)) throw new UiError('INVALID_DOCUMENT_ENTRY');
    setCurrent(next); setVerified({ origin, meta: nextMeta }); setCreating(false); setConfirmDelete(false);
    if (!preserveDraft) { setText(JSON.stringify(next.document, null, 2)); setDraftBase(next); setDirty(false); }
  };
  const latest = async () => {
    if (!current) throw new UiError('INVALID_DOCUMENT_ENTRY');
    const origin = verified?.origin || selection?.origin;
    if (!origin) throw new UiError('MISSING_DOCUMENT_ORIGIN');
    const nextMeta = verified?.meta || metadata((await scope.call('catalog.describe', connection.connection_id, { table: { database: origin.database, schema: null, table: origin.container } })).data);
    validatePartition(current.identity.partition_key, nextMeta);
    const response = await scope.call('document.read', connection.connection_id, { ...origin, identity: current.identity });
    accept(entry(response.data), origin, nextMeta, dirty); setMessage(dirty ? '최신 ETag를 확인했습니다. 작성 중인 JSON은 유지했습니다.' : '최신 문서를 조회했습니다.');
  };
  const canWrite = connection.allow_writes && !busy && !(creating ? meta?.system_key : verified?.meta.system_key);
  return <section aria-label="원본 문서"><h3>원본 문서</h3>
    <form onSubmit={event => { event.preventDefault(); void run(async () => {
      if (!meta || !database || !container) throw new UiError('METADATA_UNAVAILABLE');
      validateId(id); const partition_key = keyComponents(keys); validatePartition(partition_key, meta);
      const response = await scope.call('document.read', connection.connection_id, { database, container, identity: { id, partition_key } });
      accept(entry(response.data), { database, container }, meta, false); setMessage('원본 문서를 조회했습니다.');
    }); }}>
      <label>문서 ID<input value={id} onChange={event => setId(event.target.value)} /></label>
      {meta && <PartitionKeyFields paths={meta.partition_key_paths} fields={keys} onChange={setKeys} />}
      <button disabled={busy || !meta || !database || !container} type="submit">선택한 범위에서 문서 조회</button>
    </form>
    <button disabled={!canWrite || !meta || meta.system_key || !database || !container} onClick={() => { scope.cancel(); setCreating(true); setCurrent(null); setVerified(null); setText('{\n  "id": ""\n}'); setDirty(false); setError(''); setMessage(''); }}>새 문서 작성</button>
    {current && <>
      <p>원본 ID: {current.identity.id} · ETag: {current.etag}</p>
      <p>파티션 키: {JSON.stringify(current.identity.partition_key)}</p>
      <p>{verified ? `${verified.origin.database} / ${verified.origin.container}` : selection?.origin ? `${selection.origin.database} / ${selection.origin.container} · 저장된 결과` : '출처가 없는 저장된 결과 · JSON 보기'}</p>
      <button disabled={busy || (!verified && !selection?.origin)} onClick={() => void run(latest)}>최신 문서 조회</button>
      {!verified && <p role="status">원본을 편집하려면 최신 문서를 명시적으로 조회하세요.</p>}
    </>}
    {(current || creating) && <>
      <label>{creating ? '새 문서 JSON' : '원본 문서 JSON'}<textarea rows={12} spellCheck={false} value={text} readOnly={!connection.allow_writes || (!creating && (!verified || verified.meta.system_key))} onChange={event => { setText(event.target.value); setDirty(true); }} /></label>
      <p>ID, 파티션 키와 시스템 속성은 변경할 수 없습니다. 날짜 문자열과 사용자 속성은 그대로 저장됩니다.</p>
      <button disabled={!canWrite || (!creating && !verified)} onClick={() => void run(async () => {
        const targetMeta = creating ? meta : verified?.meta;
        if (!targetMeta) throw new UiError('METADATA_UNAVAILABLE');
        const document = documentPayload(text, targetMeta, creating ? undefined : draftBase || current!);
        if (creating) {
          const response = await scope.call('document.create', connection.connection_id, { database, container, document, partition_key: partitionFromDocument(document, targetMeta) });
          accept(entry(response.data), { database, container }, targetMeta, false);
        } else {
          const response = await scope.call('document.replace', connection.connection_id, { ...verified!.origin, identity: current!.identity, if_match: current!.etag, document });
          accept(entry(response.data), verified!.origin, targetMeta, false);
        }
        setMessage('문서를 저장했습니다.');
      })}>{creating ? '문서 생성' : '문서 저장'}</button>
      {current && <><label className="cosmos-checkbox"><input type="checkbox" checked={confirmDelete} onChange={event => setConfirmDelete(event.target.checked)} />이 문서 삭제 확인</label><button disabled={!canWrite || !verified || !confirmDelete} onClick={() => void run(async () => { await scope.call('document.delete', connection.connection_id, { ...verified!.origin, identity: current.identity, if_match: current.etag }); setCurrent(null); setVerified(null); setText(''); setDirty(false); setMessage('문서를 삭제했습니다.'); })}>문서 삭제</button></>}
    </>}
    {!connection.allow_writes && <p role="status">이 연결은 읽기 전용입니다.</p>}
    {!meta && <p role="status">파티션 키 정보를 확인한 후 문서 조회와 쓰기를 사용할 수 있습니다.</p>}
    {busy && <button onClick={() => { scope.cancel(); setBusy(false); setMessage('취소를 요청했습니다. 결과가 확인되지 않은 쓰기는 다시 전송하지 마세요.'); }}>문서 작업 취소</button>}
    {message && <p role="status">{message}</p>}{error && <p role="alert">{error}</p>}
  </section>;
}
