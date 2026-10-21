import { useEffect, useMemo, useState } from 'react';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import { connectionFields, errorText, savedConnection, settingsFromFields, type SavedConnection } from './models';
import { FieldEditor } from './ConnectionFields';
import { RequestScope } from './service';

export function ConnectionPanel({ service, connection, onChanged }: { service: UsePluginServiceReturn; connection: SavedConnection | null; onChanged: (connection: SavedConnection | null) => Promise<void> }) {
  const scope = useMemo(() => new RequestScope(service), [service]);
  const [creating, setCreating] = useState(!connection);
  const [name, setName] = useState(connection?.name || '');
  const [fields, setFields] = useState(connectionFields(connection?.settings || {}));
  const [allowWrites, setAllowWrites] = useState(connection?.allow_writes || false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');
  useEffect(() => () => scope.cancel(), [scope]);
  useEffect(() => { setCreating(!connection); setName(connection?.name || ''); setFields(connectionFields(connection?.settings || {})); setAllowWrites(connection?.allow_writes || false); setConfirmDelete(false); setError(''); setStatus(''); }, [connection]);
  const run = async (action: () => Promise<void>) => {
    const generation = scope.generation;
    setBusy(true); setError(''); setStatus('승인 또는 작업 완료를 기다리고 있습니다.');
    try { await action(); } catch (failure) { if (generation === scope.generation) { setStatus(''); setError(errorText(failure)); } } finally { if (generation === scope.generation) setBusy(false); }
  };
  return <details><summary>연결 관리</summary>
    <button type="button" disabled={busy} onClick={() => { setCreating(true); setName(''); setFields(connectionFields({})); setAllowWrites(false); setStatus(''); }}>새 연결</button>
    <form onSubmit={event => { event.preventDefault(); void run(async () => {
      const settings = settingsFromFields(fields, creating ? {} : connection?.settings);
      const response = creating ? await scope.call('connection.create', null, { name, driver: 'cosmos-nosql', settings, credential_ref: null }) : await scope.call('connection.update', connection!.connection_id, { patch: { name, settings, allow_writes: allowWrites } }, connection!.version);
      await onChanged(savedConnection(response.data)); setStatus('연결을 저장했습니다.');
    }); }}>
      <label>연결 이름<input value={name} required onChange={event => setName(event.target.value)} /></label>
      <FieldEditor fields={fields} onChange={setFields} />
      {!creating && <label className="cosmos-checkbox"><input type="checkbox" checked={allowWrites} onChange={event => setAllowWrites(event.target.checked)} />이 연결의 쓰기 허용</label>}
      <button disabled={busy || !name.trim()} type="submit">{creating ? '연결 생성' : '연결 설정 저장'}</button>
    </form>
    {connection && !creating && <>
      <button disabled={busy} onClick={() => void run(async () => { await scope.call('connection.test', connection.connection_id, {}); setStatus('연결 확인을 완료했습니다.'); })}>연결 테스트</button>
      <label className="cosmos-checkbox"><input type="checkbox" checked={confirmDelete} onChange={event => setConfirmDelete(event.target.checked)} />선택한 연결 삭제 확인</label>
      <button disabled={busy || !confirmDelete} onClick={() => void run(async () => { await scope.call('connection.delete', connection.connection_id, {}, connection.version); await onChanged(null); setStatus('연결을 삭제했습니다.'); })}>연결 삭제</button>
    </>}
    {busy && <button onClick={() => { scope.cancel(); setBusy(false); setStatus('취소를 요청했습니다.'); }}>연결 작업 취소</button>}
    {status && <p role="status">{status}</p>}{error && <p role="alert">{error}</p>}
  </details>;
}
