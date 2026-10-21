import { useEffect, useMemo, useRef, useState, type CSSProperties } from 'react';
import { usePluginService, usePluginTheme, usePluginTranslation } from '@tabularis/plugin-api';
import { AuthPanel } from './AuthPanel';
import { PluginStyle } from './PluginStyle';
import { ConnectionPanel } from './ConnectionPanel';
import { DocumentPanel, type DocumentSelection } from './DocumentPanel';
import { QueryPanel } from './QueryPanel';
import { connections, connectionFields, errorText, metadata, object, resultData, UiError, type ContainerMetadata, type SavedConnection, type SnapshotRef } from './models';
import { RequestScope } from './service';

export function Workspace({ pluginId, initialConnectionId = null, snapshot = null }: { pluginId: string; initialConnectionId?: string | null; snapshot?: SnapshotRef | null }) {
  const service = usePluginService();
  const theme = usePluginTheme();
  const t = usePluginTranslation(pluginId);
  const scope = useMemo(() => new RequestScope(service), [service]);
  const [list, setList] = useState<SavedConnection[]>([]);
  const [selectedId, setSelectedId] = useState(initialConnectionId || '');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const chooser = useRef<HTMLSelectElement>(null);
  const selected = list.find(connection => connection.connection_id === selectedId) || null;
  const refresh = async (next?: SavedConnection | null) => {
    const response = await scope.call('connection.list', null, {});
    setList(connections(response.data));
    if (next !== undefined) setSelectedId(next?.connection_id || '');
  };
  useEffect(() => {
    let alive = true;
    void refresh().catch(failure => { if (alive) setError(errorText(failure)); }).finally(() => { if (alive) { setLoading(false); chooser.current?.focus(); } });
    return () => { alive = false; scope.cancel(); };
  }, [scope]);
  const colors = theme.colors;
  const style = colors ? { '--cosmos-bg': colors.bg.base, '--cosmos-input': colors.bg.input, '--cosmos-text': colors.text.primary, '--cosmos-muted': colors.text.secondary, '--cosmos-border': colors.border.default, '--cosmos-accent': colors.accent.primary, '--cosmos-error': colors.accent.error } as CSSProperties : undefined;
  return <main className="cosmos-ui" style={style} aria-label={t('workspace', { defaultValue: 'Cosmos 작업 공간' })}>
    <PluginStyle pluginId={pluginId} />
    <label>Cosmos 연결<select ref={chooser} value={selectedId} onChange={event => { scope.cancel(); setSelectedId(event.target.value); setError(''); }}><option value="">연결 선택</option>{list.map(connection => <option key={connection.connection_id} value={connection.connection_id}>{connection.name}</option>)}</select></label>
    <button disabled={loading} onClick={() => { setLoading(true); void refresh().catch(failure => setError(errorText(failure))).finally(() => setLoading(false)); }}>최신 연결 목록</button>
    {loading && <p role="status">연결 목록을 불러오고 있습니다.</p>}
    {!selected && !loading && <p role="status">저장된 Cosmos 연결을 명시적으로 선택하세요.</p>}
    <ConnectionPanel key={`connection:${selectedId}`} service={service} connection={selected} onChanged={refresh} />
    {selected && <ConnectionWorkspace key={selected.connection_id} service={service} connection={selected} snapshot={selected.connection_id === initialConnectionId ? snapshot : null} />}
    {error && <p role="alert">{error}</p>}
  </main>;
}

function ConnectionWorkspace({ service, connection, snapshot }: { service: ReturnType<typeof usePluginService>; connection: SavedConnection; snapshot: SnapshotRef | null }) {
  const scope = useMemo(() => new RequestScope(service), [service]);
  const [ready, setReady] = useState(false);
  const [database, setDatabase] = useState(snapshot ? '' : connectionFields(connection.settings).database);
  const [container, setContainer] = useState('');
  const [databases, setDatabases] = useState<string[]>([]);
  const [containers, setContainers] = useState<string[]>([]);
  const [meta, setMeta] = useState<ContainerMetadata | null>(null);
  const [selection, setSelection] = useState<DocumentSelection | null>(null);
  const [snapshotValues, setSnapshotValues] = useState<unknown[] | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);
  useEffect(() => {
    let alive = true;
    void scope.run(() => service.capabilities(connection.connection_id)).then(capabilities => {
      if (!capabilities.documents_v1 || !capabilities.operations.includes('query.execute') || !capabilities.operations.includes('document.read')) throw new UiError('CAPABILITY_UNAVAILABLE');
      if (alive) setReady(true);
    }).catch(failure => { if (alive) setError(errorText(failure)); });
    return () => { alive = false; scope.cancel(); };
  }, [scope, service, connection.connection_id]);
  useEffect(() => {
    if (!snapshot || !ready) return;
    let alive = true;
    void scope.call('result.get', connection.connection_id, { result_id: snapshot.result_id, result_set_index: snapshot.result_set_index, offset: snapshot.row_ordinal, limit: 1 }).then(response => {
      if (!alive) return;
      const data = resultData(response.data);
      if (data.kind === 'documents' && data.documents[0]) {
        setSelection({ entry: data.documents[0], origin: data.origin, selection_id: crypto.randomUUID() });
        if (data.origin) { setDatabase(data.origin.database); setContainer(data.origin.container); }
      } else setSnapshotValues(data.values);
    }).catch(failure => { if (alive) setError(errorText(failure)); });
    return () => { alive = false; };
  }, [ready, snapshot, scope, connection.connection_id]);
  const changeScope = (nextDatabase: string, nextContainer: string) => { generation.current += 1; scope.cancel(); setDatabase(nextDatabase); setContainer(nextContainer); setMeta(null); setSelection(null); setSnapshotValues(null); setError(''); setBusy(false); };
  const run = async (action: () => Promise<void>) => {
    const current = generation.current; setBusy(true); setError('');
    try { await action(); } catch (failure) { if (current === generation.current) setError(errorText(failure)); } finally { if (current === generation.current) setBusy(false); }
  };
  return <>
    <AuthPanel service={service} connection={connection} />
    {ready && <>
      <section aria-label="컨테이너 탐색"><h3>데이터 범위</h3>
        <div className="cosmos-fields"><label>데이터베이스<input list={`databases-${connection.connection_id}`} value={database} onChange={event => { setContainers([]); changeScope(event.target.value, ''); }} /></label><datalist id={`databases-${connection.connection_id}`}>{databases.map(name => <option value={name} key={name} />)}</datalist>
        <label>컨테이너<input list={`containers-${connection.connection_id}`} value={container} onChange={event => changeScope(database, event.target.value)} /></label><datalist id={`containers-${connection.connection_id}`}>{containers.map(name => <option value={name} key={name} />)}</datalist></div>
        <button disabled={busy} onClick={() => void run(async () => { const response = await scope.call('catalog.databases', connection.connection_id, {}); if (!object(response.data) || !Array.isArray(response.data.databases) || !response.data.databases.every(name => typeof name === 'string')) throw new UiError('INVALID_RESPONSE'); setDatabases(response.data.databases as string[]); })}>데이터베이스 목록</button>
        <button disabled={busy || !database} onClick={() => void run(async () => { const response = await scope.call('catalog.objects', connection.connection_id, { database, schema: null }); if (!object(response.data) || !Array.isArray(response.data.objects)) throw new UiError('INVALID_RESPONSE'); const names = response.data.objects.map(item => object(item) ? item.name ?? item.table_name : null); if (!names.every(name => typeof name === 'string')) throw new UiError('INVALID_RESPONSE'); setContainers(names as string[]); })}>컨테이너 목록</button>
        <button disabled={busy || !database || !container} onClick={() => void run(async () => { setMeta(null); const response = await scope.call('catalog.describe', connection.connection_id, { table: { database, schema: null, table: container } }); setMeta(metadata(response.data)); })}>컨테이너 정보 확인</button>
        {meta && <><p>파티션 키: {meta.partition_key_paths.join(' → ')} · {meta.partition_key_kind} · 버전 {meta.partition_key_version}{meta.system_key ? ' · 시스템 키 (쓰기 미지원)' : ''}</p><pre>{JSON.stringify(meta.columns, null, 2)}</pre></>}
      </section>
      <QueryPanel service={service} connection={connection} database={database} container={container} meta={meta} onSelect={setSelection} />
      {snapshotValues && <section aria-label="저장된 JSON 결과"><h3>저장된 JSON 결과 (읽기 전용)</h3>{snapshotValues.map((value, index) => <pre key={index}>{JSON.stringify(value, null, 2)}</pre>)}</section>}
      <DocumentPanel service={service} connection={connection} database={database} container={container} meta={meta} selection={selection} />
    </>}
    {busy && <p role="status">컨테이너 정보를 기다리고 있습니다.</p>}
    {error && <p role="alert">{error}</p>}
  </>;
}
