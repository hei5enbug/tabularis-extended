import { useEffect, useState } from 'react';
import type { AuthMode } from '@tabularis/service-contracts/types';
import { publicEndpoint, type ConnectionFields as Fields } from './models';

export function FieldEditor({ fields, onChange }: { fields: Fields; onChange: (fields: Fields) => void }) {
  const set = (key: keyof Fields, value: string) => onChange({ ...fields, [key]: value });
  return <div className="cosmos-fields">
    <label>계정 주소<input type="url" value={fields.endpoint} placeholder="https://account.documents.azure.com" onChange={event => set('endpoint', event.target.value)} /></label>
    <label>인증 방식<select value={fields.auth_mode} onChange={event => onChange({ ...fields, auth_mode: event.target.value as AuthMode, ...(fields.auth_source ? { auth_source: 'oauth' } : {}) })}><option value="account_key">계정 키</option><option value="entra_user">Microsoft Entra 사용자</option><option value="entra_service_principal">Microsoft Entra 서비스 주체</option></select></label>
    {fields.auth_mode === 'entra_user' && <label>로그인 소스<select value={fields.auth_source ?? 'oauth'} onChange={event => onChange({ ...fields, auth_source: event.target.value as 'oauth' | 'azure_cli', ...(event.target.value === 'azure_cli' ? { client_id: '04b07795-8ddb-461a-bbee-02f9e1bf7b46' } : {}) })}><option value="oauth">앱에서 로그인</option><option value="azure_cli">Azure CLI (az login)</option></select></label>}
    {fields.auth_source === 'azure_cli' && fields.auth_mode === 'entra_user' && <p>이 컴퓨터에서 az login을 먼저 실행하세요. 앱 로그아웃은 Azure CLI 세션을 종료하지 않습니다.</p>}
    {fields.auth_mode !== 'account_key' && <><label>테넌트 ID<input value={fields.tenant_id} onChange={event => set('tenant_id', event.target.value)} /></label><label>클라이언트 ID<input value={fields.client_id} readOnly={fields.auth_source === 'azure_cli'} onChange={event => set('client_id', event.target.value)} /></label></>}
    <label>기본 데이터베이스<input value={fields.database} onChange={event => set('database', event.target.value)} /></label>
  </div>;
}
export function ConnectionExtraFields({ extra, setExtraField, setCredentialFieldsHidden }: { extra: Record<string, string>; setExtraField: (key: string, value: string) => void; setCredentialFieldsHidden: (hidden: boolean) => void }) {
  useEffect(() => { setCredentialFieldsHidden(true); return () => setCredentialFieldsHidden(false); }, [setCredentialFieldsHidden]);
  const mode = extra.auth_mode;
  const [fields, setFields] = useState<Fields>({ endpoint: extra.endpoint || '', auth_mode: mode === 'entra_user' || mode === 'entra_service_principal' ? mode : 'account_key', ...(extra.auth_source ? { auth_source: extra.auth_source as 'oauth' | 'azure_cli' } : {}), tenant_id: extra.tenant_id || '', client_id: extra.client_id || '', database: extra.database || '' });
  const [endpointError, setEndpointError] = useState(false);
  return <section className="cosmos-ui"><FieldEditor fields={fields} onChange={next => {
    setFields(next);
    for (const key of Object.keys(next) as (keyof Fields)[]) if (next[key] !== fields[key]) {
      if (key === 'endpoint') { try { setExtraField(key, publicEndpoint(next.endpoint)); setEndpointError(false); } catch { setExtraField(key, ''); setEndpointError(!!next.endpoint); } }
      else setExtraField(key, next[key] ?? '');
    }
  }} />{endpointError && <p role="alert">인증 정보가 없는 Cosmos NoSQL 계정 주소를 입력하세요.</p>}<p role="status">연결을 먼저 저장한 뒤 Cosmos 작업 공간에서 인증하세요.</p></section>;
}
