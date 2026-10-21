import { useEffect, useMemo, useRef, useState } from 'react';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import type { JsonValue } from '@tabularis/service-contracts/types';
import { challengeUrl, connectionFields, errorText, object, UiError, type SavedConnection } from './models';
import { completed, request, RequestScope } from './service';

interface AuthStatus { auth_job_id: string; state: string; challenge: { verification_uri: string; user_code: string } | null }
function authStatus(data: JsonValue): AuthStatus {
  if (!object(data) || typeof data.auth_job_id !== 'string' || !data.auth_job_id || typeof data.state !== 'string' || !['pending', 'interaction_required', 'authenticated', 'cancelled', 'failed'].includes(data.state)) throw new UiError('INVALID_RESPONSE');
  const uri = object(data.challenge) ? challengeUrl(data.challenge.verification_uri) : null;
  return { auth_job_id: data.auth_job_id, state: data.state, challenge: uri && object(data.challenge) && typeof data.challenge.user_code === 'string' ? { verification_uri: uri, user_code: data.challenge.user_code } : null };
}
export function AuthPanel({ service, connection }: { service: UsePluginServiceReturn; connection: SavedConnection }) {
  const scope = useMemo(() => new RequestScope(service), [service]);
  const secretInput = useRef<HTMLInputElement>(null);
  const activeJob = useRef<string | null>(null);
  const [persistence, setPersistence] = useState<'session' | 'keychain'>('session');
  const [status, setStatus] = useState<AuthStatus | null>(null);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const mode = connectionFields(connection.settings).auth_mode;
  useEffect(() => { if (status && !['pending', 'interaction_required'].includes(status.state)) activeJob.current = null; }, [status]);
  useEffect(() => () => {
    scope.cancel();
    if (secretInput.current) secretInput.current.value = '';
    if (activeJob.current) void service.call(request('auth.cancel', connection.connection_id, { auth_job_id: activeJob.current })).catch(() => undefined);
  }, [scope, service, connection.connection_id]);
  useEffect(() => {
    if (!status || !['pending', 'interaction_required'].includes(status.state)) return;
    let alive = true;
    const timer = setTimeout(() => {
      void scope.call('auth.status', connection.connection_id, { auth_job_id: status.auth_job_id }).then(response => { if (alive) setStatus(authStatus(response.data)); }).catch(failure => { if (alive) setError(errorText(failure)); });
    }, 1000);
    return () => { alive = false; clearTimeout(timer); };
  }, [status, scope, connection.connection_id]);
  const run = async (action: () => Promise<void>) => {
    const generation = scope.generation;
    setBusy(true); setError(''); setMessage('인증 작업을 기다리고 있습니다.');
    try { await action(); } catch (failure) { if (generation === scope.generation) { setMessage(''); setError(errorText(failure)); } } finally { if (generation === scope.generation) setBusy(false); }
  };
  return <section aria-label="인증"><h3>인증</h3>
    <label>인증 보관<select value={persistence} onChange={event => setPersistence(event.target.value as 'session' | 'keychain')}><option value="session">이번 세션</option><option value="keychain">기기 보안 저장소</option></select></label>
    {mode !== 'entra_user' && <form onSubmit={event => {
      event.preventDefault(); const secret = secretInput.current?.value || ''; if (secretInput.current) secretInput.current.value = '';
      if (!secret) { setError('인증 값을 입력하세요.'); return; }
      void run(async () => { await scope.run(async signal => completed(await service.importCredential(connection.connection_id, mode === 'account_key' ? 'cosmos_account_key' : 'entra_client_secret', secret, { persistence, signal }))); setMessage('인증 값을 안전하게 전달했습니다.'); });
    }}>
      <label>{mode === 'account_key' ? '계정 키' : '클라이언트 비밀'}<input ref={secretInput} type="password" autoComplete="off" spellCheck={false} /></label>
      <button disabled={busy} type="submit">인증 값 등록</button>
    </form>}
    <button disabled={busy} onClick={() => void run(async () => { const response = await scope.call('auth.begin', connection.connection_id, { auth_mode: mode, persistence }); const next = authStatus(response.data); activeJob.current = next.auth_job_id; setStatus(next); setMessage(''); })}>인증 시작</button>
    <button disabled={busy || !status} onClick={() => void run(async () => { const response = await scope.call('auth.status', connection.connection_id, { auth_job_id: status!.auth_job_id }); setStatus(authStatus(response.data)); setMessage(''); })}>인증 상태 확인</button>
    <button disabled={!status || ['authenticated', 'cancelled', 'failed'].includes(status.state)} onClick={() => { scope.cancel(); void run(async () => { const response = await scope.call('auth.cancel', connection.connection_id, { auth_job_id: status!.auth_job_id }); activeJob.current = null; setStatus(authStatus(response.data)); setMessage('인증을 취소했습니다.'); }); }}>인증 취소</button>
    <button disabled={busy} onClick={() => { scope.cancel(); void run(async () => { await scope.call('auth.logout', connection.connection_id, {}); activeJob.current = null; setStatus(null); setMessage('로그아웃했습니다.'); }); }}>로그아웃</button>
    {status && <p role="status">인증 상태: {status.state}</p>}
    {status?.challenge && <div><a href={status.challenge.verification_uri} target="_blank" rel="noopener noreferrer">인증 페이지 열기</a><p>인증 코드: <strong>{status.challenge.user_code}</strong></p></div>}
    {message && <p role="status">{message}</p>}{error && <p role="alert">{error}</p>}
  </section>;
}
