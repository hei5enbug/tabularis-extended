import { useEffect, useRef, useState } from 'react';
import { usePluginConnection, usePluginQuery } from '@tabularis/plugin-api';
import { publicEndpoint } from './models';

const CLI_CLIENT_ID = '04b07795-8ddb-461a-bbee-02f9e1bf7b46';
export function CompatConnectionFields({ extra, setExtraField, setCredentialFieldsHidden }: { extra: Record<string, string>; setExtraField: (key: string, value: string) => void; setCredentialFieldsHidden: (hidden: boolean) => void }) {
  const [error, setError] = useState('');
  useEffect(() => { setCredentialFieldsHidden(false); if (!extra.auth_mode) setExtraField('auth_mode', 'account_key'); }, [extra.auth_mode, setExtraField, setCredentialFieldsHidden]);
  const cli = extra.auth_mode === 'entra_user' && extra.auth_source === 'azure_cli';
  const mode = extra.auth_mode ?? 'account_key';
  const unsupported = mode !== 'account_key' && !cli;
  return <section className="cosmos-ui"><div className="cosmos-fields">
    <label>계정 주소<input type="url" value={extra.endpoint ?? ''} placeholder="https://account.documents.azure.com" onChange={event => { setExtraField('endpoint', event.target.value); try { if (event.target.value) publicEndpoint(event.target.value); setError(''); } catch { setError('인증 정보가 없는 Cosmos NoSQL HTTPS 주소를 입력하세요.'); } }} /></label>
    <label>인증 방식<select value={cli ? 'azure_cli' : unsupported ? 'unsupported' : 'account_key'} onChange={event => {
      const nextCli = event.target.value === 'azure_cli';
      setExtraField('auth_mode', nextCli ? 'entra_user' : 'account_key');
      setExtraField('auth_source', nextCli ? 'azure_cli' : 'oauth');
      if (nextCli) setExtraField('client_id', CLI_CLIENT_ID);
    }}><option value="account_key">계정 키</option><option value="azure_cli">Azure CLI (az login)</option>{unsupported && <option value="unsupported">확장 호스트 인증</option>}</select></label>
    {cli ? <><label>테넌트 ID<input value={extra.tenant_id ?? ''} onChange={event => setExtraField('tenant_id', event.target.value)} /></label><p>이 컴퓨터에서 az login을 실행하세요. Cosmos 데이터 읽기 권한이 필요합니다. 비밀번호는 사용하지 않습니다.</p></> : <p>계정 키를 위의 비밀번호 필드에 입력하세요. Tabularis의 연결 비밀번호 저장 기능을 사용합니다.</p>}
    <label>기본 데이터베이스<input value={extra.database ?? ''} onChange={event => setExtraField('database', event.target.value)} /></label>
    <label>기본 컨테이너<input value={extra.container ?? ''} onChange={event => setExtraField('container', event.target.value)} /></label>
  </div>{error && <p role="alert">{error}</p>}{unsupported && <p role="alert">이 인증 방식은 공통 서비스를 제공하는 호스트에서 사용할 수 있습니다.</p>}<p>이 호스트에서는 읽기 전용으로 연결합니다. Cosmos SQL의 FROM c는 여기에 지정한 기본 컨테이너를 조회합니다.</p></section>;
}

export function CompatWorkspace({ connectionId }: { connectionId: string | null }) {
  const active = usePluginConnection();
  const initialActiveConnection = useRef(active.driver === 'cosmos-nosql' ? active.connectionId : null);
  const targetConnectionId = connectionId ?? initialActiveConnection.current;
  const { executeQuery, loading } = usePluginQuery();
  const [query, setQuery] = useState('SELECT * FROM c');
  const [result, setResult] = useState('');
  const [notice, setNotice] = useState('');
  const [error, setError] = useState('');
  const generation = useRef(0);
  const identity = useRef(active.connectionId);
  identity.current = active.connectionId;
  useEffect(() => { generation.current++; setResult(''); return () => { generation.current++; }; }, [active.connectionId, targetConnectionId]);
  const valid = !!targetConnectionId && active.connectionId === targetConnectionId && active.driver === 'cosmos-nosql';
  const run = async () => {
    if (!valid) { setError('이 작업 공간의 Cosmos 연결을 활성화하세요.'); return; }
    const current = ++generation.current;
    setError(''); setNotice(''); setResult('');
    try {
      const response = await executeQuery(query) as { columns: string[]; rows: unknown[][]; truncated?: boolean };
      if (current !== generation.current || identity.current !== targetConnectionId) return;
      const sample = JSON.stringify(response.rows.slice(0, 20).map(row => row[0]), null, 2);
      setResult(new TextDecoder().decode(new TextEncoder().encode(sample).subarray(0, 64 * 1024), { stream: true }));
      setNotice(`${response.rows.length}개 조회 · 최대 20개 문서, 64 KiB를 화면에 표시합니다.${response.truncated ? ' 조회 상한에 도달했습니다(truncated).' : ''}`);
    } catch (failure) {
      if (current === generation.current && identity.current === targetConnectionId) setError(failure instanceof Error ? failure.message : 'Cosmos 조회에 실패했습니다.');
    }
  };
  return <section className="cosmos-ui"><p>기본 조회 모드 · 연결에 저장한 데이터베이스와 기본 컨테이너를 읽습니다. 쿼리 내용은 그대로 전달합니다.</p><p>문서 수정, 페이지 재개, 앱 내 Entra 로그인은 공통 서비스를 제공하는 호스트에서 사용할 수 있습니다.</p><label>Cosmos SQL<textarea value={query} onChange={event => setQuery(event.target.value)} rows={5} spellCheck={false} /></label><button disabled={!valid || loading || !query.trim()} onClick={() => void run()}>조회</button>{!valid && <p role="status">이 작업 공간의 Cosmos 연결을 활성화하세요.</p>}{error && <p role="alert">{error}</p>}{notice && <p role="status">{notice}</p>}{result && <pre>{result}</pre>}</section>;
}
