import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { operationNames, type ServiceRequest, type ServiceResponse } from '@tabularis/service-contracts/types';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import { AuthPanel } from '../src/AuthPanel';
import { ConnectionExtraFields } from '../src/ConnectionFields';
import { ConnectionPanel } from '../src/ConnectionPanel';
import { DocumentPanel, type DocumentSelection } from '../src/DocumentPanel';
import { QueryPanel } from '../src/QueryPanel';
import { Workspace } from '../src/Workspace';
import CosmosActions from '../src/index';
import { checked, checkedResponse, connection, containerMetadata, failure, original, response } from './fixtures';

const sdk = vi.hoisted(() => ({ service: null as unknown as UsePluginServiceReturn, openModal: vi.fn(), closeModal: vi.fn(), assets: { resolve: vi.fn(async () => ({ url: "blob:synthetic-cosmos-css", dispose: vi.fn() })) } }));
vi.mock('@tabularis/plugin-api', () => ({
  usePluginService: () => sdk.service,
  usePluginAssets: () => sdk.assets,
  usePluginModal: () => ({ openModal: sdk.openModal, closeModal: sdk.closeModal }),
  usePluginTheme: () => ({ colors: null, isDark: false }),
  usePluginTranslation: () => (_key: string, options?: { defaultValue?: string }) => options?.defaultValue || _key,
}));
type Handler = (request: ServiceRequest, options?: { signal?: AbortSignal }) => Promise<ServiceResponse>;
function fakeService(handler?: Handler) {
  const call = vi.fn<Handler>(async (request, options) => {
    checked(request);
    const value = handler ? await handler(request, options) : request.operation === 'connection.list' ? response({ connections: [connection] }) : response(null);
    return checkedResponse({ ...value, request_id: request.request_id, connection_id: request.connection_id || null });
  });
  return { call, executeWrite: vi.fn<UsePluginServiceReturn['executeWrite']>(async () => { throw new Error('Query writes are unavailable in this Cosmos fixture.'); }), saveArtifact: vi.fn(async () => false), capabilities: vi.fn(async () => ({ service_protocol: 1 as const, operations: [...operationNames], documents_v1: true, spatial_v1: false, cancel_v1: true })), importCredential: vi.fn<UsePluginServiceReturn['importCredential']>(async () => response({ credential_ref: 'opaque' })), subscribeMap: vi.fn(async () => () => undefined) };
}
async function settled(): Promise<void> { await act(async () => { await Promise.resolve(); }); }
async function pointRead(): Promise<void> {
  fireEvent.change(screen.getByLabelText('문서 ID'), { target: { value: 'doc-1' } });
  fireEvent.change(screen.getByLabelText('/tenant 값'), { target: { value: '서울' } });
  fireEvent.change(screen.getByLabelText('/nested/region 형식'), { target: { value: 'null' } });
  fireEvent.change(screen.getByLabelText('/optional 형식'), { target: { value: 'undefined' } });
  fireEvent.click(screen.getByRole('button', { name: '선택한 범위에서 문서 조회' }));
  await screen.findByLabelText('원본 문서 JSON');
}
function documentService(replace?: ServiceResponse) {
  return fakeService(async request => request.operation === 'document.read' ? response(original) : request.operation === 'document.replace' ? replace || response(original) : response(null));
}
function renderDocument(service = documentService(), selection: DocumentSelection | null = null, allowWrites = true) {
  return render(<DocumentPanel service={service} connection={{ ...connection, allow_writes: allowWrites }} database="db" container="container" meta={containerMetadata} selection={selection} />);
}
beforeEach(() => { sdk.service = fakeService(); sdk.openModal.mockReset(); sdk.closeModal.mockReset(); });
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe('연결 선택과 설정', () => {
  it('연결 modal은 공개 필드만 저장하고 host 자격 증명을 숨긴다', () => {
    // given
    const setExtraField = vi.fn(), hide = vi.fn();
    render(<ConnectionExtraFields extra={{ endpoint: '', auth_mode: 'account_key' }} setExtraField={setExtraField} setCredentialFieldsHidden={hide} />);
    // when
    fireEvent.change(screen.getByLabelText('계정 주소'), { target: { value: 'https://example.documents.azure.com' } });
    // then
    expect(hide).toHaveBeenCalledWith(true);
    expect(setExtraField).toHaveBeenCalledExactlyOnceWith('endpoint', 'https://example.documents.azure.com');
    expect(screen.queryByLabelText('계정 키')).not.toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('연결을 먼저 저장');
  });
  it('연결 필드 unmount에서 host 자격 증명 표시를 복구한다', () => {
    // given
    const hide = vi.fn();
    const mounted = render(<ConnectionExtraFields extra={{}} setExtraField={vi.fn()} setCredentialFieldsHidden={hide} />);
    // when
    mounted.unmount();
    // then
    expect(hide).toHaveBeenLastCalledWith(false);
  });
  it('주소에 포함된 비밀은 extra 필드로 저장하지 않는다', () => {
    // given
    const setExtraField = vi.fn();
    render(<ConnectionExtraFields extra={{}} setExtraField={setExtraField} setCredentialFieldsHidden={vi.fn()} />);
    // when
    fireEvent.change(screen.getByLabelText('계정 주소'), { target: { value: 'https://user:synthetic-url-secret@example.documents.azure.com' } });
    // then
    expect(setExtraField).toHaveBeenCalledExactlyOnceWith('endpoint', '');
    expect(JSON.stringify(setExtraField.mock.calls)).not.toContain('synthetic-url-secret');
    expect(screen.getByRole('alert')).toHaveTextContent('인증 정보가 없는');
  });
  it('설정 actions가 host modal로 작업 공간을 연다', () => {
    // given
    render(<CosmosActions pluginId="cosmos-nosql" context={{ targetPluginId: 'cosmos-nosql' }} />);
    // when
    fireEvent.click(screen.getByRole('button', { name: 'Cosmos 작업 공간' }));
    // then
    expect(sdk.openModal).toHaveBeenCalledWith(expect.objectContaining({ title: 'Cosmos 작업 공간', size: 'xl' }));
  });
  it('실제 작업 공간은 여는 actions가 닫힌 뒤에도 자체 스타일 링크를 유지한다', async () => {
    // given
    sdk.assets.resolve.mockClear();
    const opener = render(<CosmosActions pluginId="cosmos-nosql" context={{ targetPluginId: 'cosmos-nosql' }} />);
    const openThenCloseActions = async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Cosmos 작업 공간' }));
      const content = sdk.openModal.mock.calls.at(-1)![0].content;
      opener.unmount();
      const workspace = render(content);
      await settled();
      return workspace;
    };
    // when
    const actual = await openThenCloseActions();
    // then
    expect(sdk.assets.resolve).toHaveBeenCalledTimes(2);
    expect(actual.container.querySelector('link[rel="stylesheet"]')).toHaveAttribute('href', 'blob:synthetic-cosmos-css');
    expect(screen.getByRole('main')).toHaveAttribute('aria-label', 'Cosmos 작업 공간');
  });
  it('연결 context가 보장되지 않은 footer에는 작업 버튼을 만들지 않는다', () => {
    // given
    const context = { connectionId: 'cosmos-a', driver: 'cosmos-nosql' };
    // when
    const mounted = render(<CosmosActions pluginId="cosmos-nosql" context={context} />);
    // then
    expect(mounted.container).toBeEmptyDOMElement();
  });
  it('legacy rowIndex만 있는 context 문서 버튼은 비활성화한다', () => {
    // given
    const context = { connectionId: 'cosmos-a', driver: 'cosmos-nosql', tableName: 'container', rowIndex: 2, rowData: { id: 'guess', _etag: 'guess' } };
    // when
    render(<CosmosActions pluginId="cosmos-nosql" context={context} />);
    // then
    expect(screen.getByRole('button', { name: '문서 보기' })).toBeDisabled();
    expect(sdk.service.call).not.toHaveBeenCalled();
  });
  it('명시적으로 선택한 Cosmos 연결만 서비스에 전달한다', async () => {
    // given
    const second = { ...connection, connection_id: 'cosmos-b', name: '두 번째 계정' };
    sdk.service = fakeService(async request => request.operation === 'connection.list' ? response({ connections: [connection, second, { ...connection, driver: 'postgresql', connection_id: 'pg' }] }) : response({ databases: ['db'] }));
    render(<Workspace pluginId="cosmos-nosql" initialConnectionId="cosmos-b" />);
    const scenario = async () => { await screen.findByRole('button', { name: '데이터베이스 목록' }); fireEvent.click(screen.getByRole('button', { name: '데이터베이스 목록' })); await settled(); };
    // when
    await scenario();
    // then
    expect(sdk.service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'catalog.databases', connection_id: 'cosmos-b' }), expect.anything());
    expect(screen.queryByRole('option', { name: 'postgresql' })).not.toBeInTheDocument();
    expect(screen.getByLabelText('Cosmos 연결')).toHaveValue('cosmos-b');
  });
  it('연결 생성에는 비밀 없이 credential_ref null을 전달한다', async () => {
    // given
    const service = fakeService(async () => response(connection));
    render(<ConnectionPanel service={service} connection={null} onChanged={vi.fn(async () => undefined)} />);
    const scenario = async () => { fireEvent.change(screen.getByLabelText('연결 이름'), { target: { value: '새 계정' } }); fireEvent.change(screen.getByLabelText('계정 주소'), { target: { value: 'https://example.documents.azure.com' } }); fireEvent.click(screen.getByRole('button', { name: '연결 생성' })); await screen.findByText('연결을 저장했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ connection_id: null, operation: 'connection.create', input: { name: '새 계정', driver: 'cosmos-nosql', settings: expect.any(Object), credential_ref: null } }), expect.anything());
    expect(JSON.stringify(service.call.mock.calls)).not.toContain('password');
  });
  it('쓰기 허용은 사용자 체크와 기대 버전으로만 변경한다', async () => {
    // given
    const service = fakeService(async () => response(connection));
    render(<ConnectionPanel service={service} connection={{ ...connection, allow_writes: false }} onChanged={vi.fn(async () => undefined)} />);
    const scenario = async () => { fireEvent.click(screen.getByLabelText('이 연결의 쓰기 허용')); fireEvent.click(screen.getByRole('button', { name: '연결 설정 저장' })); await screen.findByText('연결을 저장했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'connection.update', expected_version: 4, input: { patch: expect.objectContaining({ allow_writes: true }) } }), expect.anything());
    expect(JSON.stringify(service.call.mock.calls)).not.toContain('approved');
    expect(JSON.stringify(service.call.mock.calls)).not.toContain('"driver"');
  });
  it('설정 버전 충돌은 자동 저장 없이 사용자에게 표시한다', async () => {
    // given
    const service = fakeService(async () => failure('VERSION_CONFLICT'));
    render(<ConnectionPanel service={service} connection={connection} onChanged={vi.fn(async () => undefined)} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '연결 설정 저장' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(screen.getByRole('alert')).toHaveTextContent('연결 설정이 변경');
    expect(service.call).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText('연결 이름')).toHaveValue(connection.name);
  });
  it('연결 테스트와 확인한 삭제는 각각 명시적 연결과 버전으로 실행한다', async () => {
    // given
    const service = fakeService(async () => response(null));
    const changed = vi.fn(async () => undefined);
    render(<ConnectionPanel service={service} connection={connection} onChanged={changed} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '연결 테스트' })); await screen.findByText('연결 확인을 완료했습니다.'); fireEvent.click(screen.getByLabelText('선택한 연결 삭제 확인')); fireEvent.click(screen.getByRole('button', { name: '연결 삭제' })); await screen.findByText('연결을 삭제했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[0][0]).toMatchObject({ operation: 'connection.test', connection_id: 'cosmos-a', expected_version: null });
    expect(service.call.mock.calls[1][0]).toMatchObject({ operation: 'connection.delete', connection_id: 'cosmos-a', expected_version: 4 });
    expect(changed).toHaveBeenCalledExactlyOnceWith(null);
  });
  it('catalog 탐색은 canonical 객체와 full HPK metadata를 소비한다', async () => {
    // given
    sdk.service = fakeService(async request => request.operation === 'connection.list' ? response({ connections: [connection] }) : request.operation === 'catalog.databases' ? response({ databases: ['db'] }) : request.operation === 'catalog.objects' ? response({ objects: [{ name: 'container', kind: 'container' }] }) : response(containerMetadata));
    render(<Workspace pluginId="cosmos-nosql" initialConnectionId="cosmos-a" />);
    const scenario = async () => { await screen.findByLabelText('컨테이너'); fireEvent.click(screen.getByRole('button', { name: '데이터베이스 목록' })); await settled(); fireEvent.click(screen.getByRole('button', { name: '컨테이너 목록' })); await settled(); fireEvent.change(screen.getByLabelText('컨테이너'), { target: { value: 'container' } }); fireEvent.click(screen.getByRole('button', { name: '컨테이너 정보 확인' })); await screen.findByText(/파티션 키: \/tenant → \/nested\/region → \/optional/); await screen.findByLabelText('/optional 형식'); };
    // when
    await scenario();
    // then
    expect(sdk.service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'catalog.objects', input: { database: 'db', schema: null } }), expect.anything());
    expect(sdk.service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'catalog.describe', input: { table: { database: 'db', schema: null, table: 'container' } } }), expect.anything());
    expect(screen.getByLabelText('/optional 형식')).toBeInTheDocument();
  });
  it('metadata 없는 describe는 추정 없이 문서 기능을 읽기 전용으로 둔다', async () => {
    // given
    sdk.service = fakeService(async request => request.operation === 'connection.list' ? response({ connections: [connection] }) : response({ columns: [] }));
    render(<Workspace pluginId="cosmos-nosql" initialConnectionId="cosmos-a" />);
    const scenario = async () => { await screen.findByLabelText('컨테이너'); fireEvent.change(screen.getByLabelText('컨테이너'), { target: { value: 'container' } }); fireEvent.click(screen.getByRole('button', { name: '컨테이너 정보 확인' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(screen.getByRole('alert')).toHaveTextContent('파티션 키 정보를 확인할 수 없어');
    expect(screen.getByRole('button', { name: '새 문서 작성' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '선택한 범위에서 문서 조회' })).toBeDisabled();
  });
});

describe('비밀 입력과 인증', () => {
  it('계정 키 입력은 제출 전에 비우고 helper로 한 번만 전달한다', async () => {
    // given
    const service = fakeService();
    const canary = 'synthetic-ui-secret-canary';
    service.importCredential.mockImplementation(async () => { throw new Error(canary); });
    render(<AuthPanel service={service} connection={connection} />);
    const scenario = async () => { fireEvent.change(screen.getByLabelText('계정 키'), { target: { value: canary } }); fireEvent.click(screen.getByRole('button', { name: '인증 값 등록' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(screen.getByLabelText('계정 키')).toHaveValue('');
    expect(service.importCredential).toHaveBeenCalledExactlyOnceWith('cosmos-a', 'cosmos_account_key', canary, { persistence: 'session', signal: expect.any(AbortSignal) });
    expect(service.call).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain(canary);
    expect(localStorage.length).toBe(0);
  });
  it('서비스 주체 비밀은 선택한 보관 방식과 전용 helper로 전달한다', async () => {
    // given
    const service = fakeService();
    render(<AuthPanel service={service} connection={{ ...connection, settings: { extra: { auth_mode: 'entra_service_principal' } } }} />);
    const scenario = async () => { fireEvent.change(screen.getByLabelText('인증 보관'), { target: { value: 'keychain' } }); fireEvent.change(screen.getByLabelText('클라이언트 비밀'), { target: { value: 'synthetic-sp-secret' } }); fireEvent.click(screen.getByRole('button', { name: '인증 값 등록' })); await screen.findByText('인증 값을 안전하게 전달했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.importCredential).toHaveBeenCalledWith('cosmos-a', 'entra_client_secret', 'synthetic-sp-secret', { persistence: 'keychain', signal: expect.any(AbortSignal) });
    expect(screen.getByLabelText('클라이언트 비밀')).toHaveValue('');
  });
  it('Entra 사용자 인증은 공식 HTTPS challenge만 표시한다', async () => {
    // given
    const service = fakeService(async () => response({ auth_job_id: 'auth-a', state: 'interaction_required', challenge: { verification_uri: 'https://microsoft.com/devicelogin', user_code: 'ABCD' } }));
    render(<AuthPanel service={service} connection={{ ...connection, settings: { extra: { auth_mode: 'entra_user' } } }} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '인증 시작' })); await screen.findByRole('link'); };
    // when
    await scenario();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'auth.begin', input: { auth_mode: 'entra_user', persistence: 'session' } }), expect.anything());
    expect(screen.getByRole('link')).toHaveAttribute('href', 'https://microsoft.com/devicelogin');
    expect(screen.getByText('ABCD')).toBeInTheDocument();
    expect(screen.queryByLabelText('계정 키')).not.toBeInTheDocument();
  });
  it.each(['https://login.example.com/devicelogin', 'https://login.microsoft.com/devicelogin', 'https://microsoft.com/other', 'https://user:pass@microsoft.com/devicelogin'])('공식 주소가 아닌 challenge %s는 링크와 코드를 표시하지 않는다', async verificationUri => {
    // given
    const service = fakeService(async () => response({ auth_job_id: 'auth-a', state: 'interaction_required', challenge: { verification_uri: verificationUri, user_code: 'UNSAFE-CODE' } }));
    render(<AuthPanel service={service} connection={{ ...connection, settings: { extra: { auth_mode: 'entra_user' } } }} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '인증 시작' })); await screen.findByText('인증 상태: interaction_required'); };
    // when
    await scenario();
    // then
    expect(screen.queryByRole('link')).not.toBeInTheDocument();
    expect(screen.queryByText('UNSAFE-CODE')).not.toBeInTheDocument();
  });
  it('device 인증 취소와 로그아웃을 명시적인 작업으로 전달한다', async () => {
    // given
    const service = fakeService(async request => response({ auth_job_id: 'auth-a', state: request.operation === 'auth.cancel' ? 'cancelled' : 'pending' }));
    render(<AuthPanel service={service} connection={connection} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '인증 시작' })); await screen.findByText('인증 상태: pending'); fireEvent.click(screen.getByRole('button', { name: '인증 취소' })); await screen.findByText('인증을 취소했습니다.'); fireEvent.click(screen.getByRole('button', { name: '로그아웃' })); await screen.findByText('로그아웃했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'auth.cancel', connection_id: 'cosmos-a', input: { auth_job_id: 'auth-a' } }), expect.anything());
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'auth.logout', connection_id: 'cosmos-a', input: {} }), expect.anything());
  });
  it('인증 창 unmount는 진행 중인 device 인증을 취소한다', async () => {
    // given
    const service = fakeService(async () => response({ auth_job_id: 'auth-a', state: 'pending' }));
    const mounted = render(<AuthPanel service={service} connection={connection} />);
    fireEvent.click(screen.getByRole('button', { name: '인증 시작' }));
    await screen.findByText('인증 상태: pending');
    // when
    mounted.unmount();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'auth.cancel', connection_id: 'cosmos-a' }));
  });
  it('인증 polling은 auth_job_id로 상태를 확인하고 완료 뒤 멈춘다', async () => {
    // given
    vi.useFakeTimers();
    const service = fakeService(async request => response({ auth_job_id: 'auth-a', state: request.operation === 'auth.begin' ? 'pending' : 'authenticated' }));
    render(<AuthPanel service={service} connection={connection} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '인증 시작' })); await settled(); await act(async () => { await vi.advanceTimersByTimeAsync(1000); }); await act(async () => { await vi.advanceTimersByTimeAsync(3000); }); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls.filter(call => call[0].operation === 'auth.status')).toHaveLength(1);
    expect(service.call.mock.calls[1][0]).toMatchObject({ operation: 'auth.status', input: { auth_job_id: 'auth-a' } });
    expect(screen.getByText('인증 상태: authenticated')).toBeInTheDocument();
  });
  it('인증 값 제출 중에는 password input이 이미 비어 있고 신호가 unmount에서 취소된다', async () => {
    // given
    const service = fakeService();
    service.importCredential.mockImplementation(() => new Promise(() => undefined));
    const mounted = render(<AuthPanel service={service} connection={connection} />);
    const scenario = () => { fireEvent.change(screen.getByLabelText('계정 키'), { target: { value: 'one-shot' } }); fireEvent.click(screen.getByRole('button', { name: '인증 값 등록' })); const cleared = (screen.getByLabelText('계정 키') as HTMLInputElement).value; mounted.unmount(); return cleared; };
    // when
    const cleared = scenario();
    // then
    expect(cleared).toBe('');
    expect(service.importCredential.mock.calls[0][3].signal?.aborted).toBe(true);
  });
});

describe('쿼리 페이지와 원본 구분', () => {
  it('빈 페이지라도 has_more의 불투명 token으로 다음 페이지만 요청한다', async () => {
    // given
    const service = fakeService(async request => request.operation === 'query.execute' ? response({ kind: 'json_values', values: [] }, { page: { next_token: 'opaque-public-token', has_more: true, resume_mode: 'native' }, warnings: ['주의 문구'] }) : response({ kind: 'json_values', values: [null, ['nested']] }, { metrics: { request_charge: 1.5, retry_count: 2, elapsed_ms: 3 }, limits: { truncated: true, reasons: ['RU_BUDGET'] } }));
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); await screen.findByText('이 페이지에 결과가 없습니다.'); fireEvent.click(screen.getByRole('button', { name: '다음 페이지' })); await screen.findByText('페이지 2'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[1][0]).toMatchObject({ operation: 'query.next', input: { next_token: 'opaque-public-token' } });
    expect(screen.getByText(/총 RU: 4/)).toBeInTheDocument();
    expect(screen.getByText('결과 제한: RU_BUDGET')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: '다음 페이지' })).not.toBeInTheDocument();
  });
  it.each([
    { title: '측정값 없는 단일 페이지', charges: [null], total: '정보 없음' },
    { title: '측정값 없는 페이지들', charges: [null, null], total: '정보 없음' },
    { title: '측정값 뒤에 정보 없는 페이지', charges: [2.5, null], total: '정보 없음' },
    { title: '정보 없는 페이지 뒤에 측정값', charges: [null, 1.5], total: '정보 없음' },
    { title: '모든 페이지가 측정됨', charges: [2.5, 1.5], total: '4' },
    { title: '측정된 영 RU', charges: [0, 0], total: '0' },
  ])('$title의 총 RU를 정확하게 표시한다', async ({ charges, total }) => {
    // given
    let pageIndex = 0;
    const service = fakeService(async () => {
      const index = pageIndex++;
      return response({ kind: 'json_values', values: [`page-${index}`] }, { metrics: { elapsed_ms: 1, retry_count: 0, request_charge: charges[index] }, page: { next_token: index + 1 < charges.length ? 'opaque-token' : null, has_more: index + 1 < charges.length, resume_mode: 'native' } });
    });
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); await screen.findByText('"page-0"'); for (let index = 1; index < charges.length; index += 1) { fireEvent.click(screen.getByRole('button', { name: '다음 페이지' })); await screen.findByText(`"page-${index}"`); } };
    // when
    await scenario();
    // then
    expect(screen.getByText(`마지막 페이지입니다. · 총 RU: ${total}`)).toBeInTheDocument();
    expect(screen.getAllByText(/^RU:/).map(element => element.textContent)).toEqual(charges.map(charge => `RU: ${charge ?? '정보 없음'} · 재시도: 0 · 결과: succeeded`));
  });
  it('쿼리 원문과 이름 있는 매개변수 및 부분 HPK를 그대로 전달한다', async () => {
    // given
    const service = fakeService(async () => response({ kind: 'json_values', values: [3] }));
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const text = 'SELECT VALUE c.amount FROM c WHERE c.tenant = @tenant';
    const scenario = async () => { fireEvent.change(screen.getByLabelText('Cosmos SQL'), { target: { value: text } }); fireEvent.change(screen.getByLabelText('이름 있는 매개변수 JSON'), { target: { value: '[{"name":"@tenant","value":"서울"}]' } }); fireEvent.click(screen.getByLabelText('파티션 범위 지정')); fireEvent.change(screen.getByLabelText('/tenant 값'), { target: { value: '서울' } }); fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); await screen.findByText('마지막 페이지입니다. · 총 RU: 2.5'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[0][0].input).toEqual({ language: 'cosmos_sql', text, parameters: [{ name: '@tenant', value: '서울' }], page_size: 100, ru_budget: 100, database: 'db', container: 'container', partition_key: [{ type: 'string', value: '서울' }], result_mode: 'json_values' });
  });
  it('id와 ETag가 있는 projection도 JSON 보기만 제공한다', async () => {
    // given
    const service = fakeService(async () => response({ kind: 'json_values', values: [{ id: 'looks-original', _etag: 'looks-original' }, ['array'], 1, null] }));
    const select = vi.fn();
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={select} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); await screen.findByText(/looks-original/); };
    // when
    await scenario();
    // then
    expect(screen.queryByRole('button', { name: /문서 1 보기/ })).not.toBeInTheDocument();
    expect(select).not.toHaveBeenCalled();
  });
  it('검증된 documents entry만 실행 시 범위와 함께 선택한다', async () => {
    // given
    const service = fakeService(async () => response({ kind: 'documents', documents: [original] }));
    const select = vi.fn();
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={select} />);
    const scenario = async () => { fireEvent.change(screen.getByLabelText('결과 형식'), { target: { value: 'documents' } }); fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); fireEvent.click(await screen.findByRole('button', { name: '페이지 1 문서 1 보기' })); };
    // when
    await scenario();
    // then
    expect(select).toHaveBeenCalledWith({ entry: original, origin: { database: 'db', container: 'container' }, selection_id: expect.any(String) });
  });
  it('쿼리 취소 뒤 늦게 도착한 결과는 표시하지 않는다', async () => {
    // given
    let resolve!: (value: ServiceResponse) => void;
    const service = fakeService(() => new Promise(done => { resolve = done; }));
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); fireEvent.click(screen.getByRole('button', { name: '쿼리 취소' })); await act(async () => resolve(response({ kind: 'json_values', values: ['stale-result'] }))); };
    // when
    await scenario();
    // then
    expect(screen.queryByText(/stale-result/)).not.toBeInTheDocument();
    expect(service.call.mock.calls[0][1]?.signal?.aborted).toBe(true);
  });
  it('연결 변경은 이전 쿼리 신호를 취소하고 응답을 새 연결에 적용하지 않는다', async () => {
    // given
    let resolve!: (value: ServiceResponse) => void;
    const second = { ...connection, connection_id: 'cosmos-b', name: '두 번째 계정' };
    const service = fakeService(async request => request.operation === 'connection.list' ? response({ connections: [connection, second] }) : new Promise(done => { resolve = done; }));
    sdk.service = service;
    render(<Workspace pluginId="cosmos-nosql" initialConnectionId="cosmos-a" />);
    const scenario = async () => { await screen.findByLabelText('컨테이너'); fireEvent.change(screen.getByLabelText('컨테이너'), { target: { value: 'container' } }); fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); fireEvent.change(screen.getByLabelText('Cosmos 연결'), { target: { value: 'cosmos-b' } }); await act(async () => resolve(response({ kind: 'json_values', values: ['old-connection-result'] }))); await screen.findByLabelText('Cosmos SQL'); };
    // when
    await scenario();
    // then
    expect(screen.getByLabelText('Cosmos 연결')).toHaveValue('cosmos-b');
    expect(screen.queryByText(/old-connection-result/)).not.toBeInTheDocument();
    expect(service.call.mock.calls.find(call => call[0].operation === 'query.execute')?.[1]?.signal?.aborted).toBe(true);
  });
  it('실패한 쿼리의 RU 재시도 경고와 적용 결과를 성공 없이 표시한다', async () => {
    // given
    const service = fakeService(async () => ({ ...failure('RATE_LIMITED'), metrics: { elapsed_ms: 4, request_charge: 1.2, retry_count: 3 }, warnings: ['요청 제한'], limits: { truncated: true, reasons: ['RU_BUDGET'] } }));
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '쿼리 실행' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(screen.getByRole('alert')).toHaveTextContent('상태: 실패 · 결과: 적용되지 않음');
    expect(screen.getByText('RU: 1.2 · 재시도: 3')).toBeInTheDocument();
    expect(screen.getByText('요청 제한')).toBeInTheDocument();
    expect(screen.queryByText('쿼리 페이지를 받았습니다.')).not.toBeInTheDocument();
  });
  it('페이지 크기 상한을 넘으면 쿼리를 전송하지 않는다', async () => {
    // given
    const service = fakeService();
    render(<QueryPanel service={service} connection={connection} database="db" container="container" meta={containerMetadata} onSelect={vi.fn()} />);
    const scenario = async () => { fireEvent.change(screen.getByLabelText('페이지 크기'), { target: { value: '1001' } }); fireEvent.submit(screen.getByLabelText('Cosmos SQL').closest('form')!); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(service.call).not.toHaveBeenCalled();
    expect(screen.getByRole('alert')).toHaveTextContent('INVALID_QUERY_OPTIONS');
  });
});

describe('문서 원본과 ETag 편집', () => {
  it('정확한 result ordinal만 조회하고 rowData는 사용하지 않는다', async () => {
    // given
    const service = fakeService(async request => request.operation === 'connection.list' ? response({ connections: [connection] }) : response({ kind: 'documents', documents: [original], origin: { database: 'actual-db', container: 'actual-container' } }));
    sdk.service = service;
    // when
    render(<Workspace pluginId="cosmos-nosql" initialConnectionId="cosmos-a" snapshot={{ result_id: 'snapshot-a', result_set_index: 2, row_ordinal: 7, generation: 9 }} />);
    // then
    await waitFor(() => expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'result.get', input: { result_id: 'snapshot-a', result_set_index: 2, offset: 7, limit: 1 } }), expect.anything()));
    expect(await screen.findByLabelText('원본 문서 JSON')).toHaveAttribute('readonly');
    expect(service.call.mock.calls.some(call => call[0].operation === 'document.read')).toBe(false);
    expect(service.call.mock.calls.some(call => call[0].operation === 'query.execute')).toBe(false);
  });
  it('출처 없는 snapshot은 기본 DB를 추정하지 않고 보기만 제공한다', () => {
    // given
    const selection = { entry: original, origin: null, selection_id: 's' };
    // when
    renderDocument(documentService(), selection);
    // then
    expect(screen.getByLabelText('원본 문서 JSON')).toHaveAttribute('readonly');
    expect(screen.getByRole('button', { name: '최신 문서 조회' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '문서 저장' })).toBeDisabled();
  });
  it('point read는 full HPK의 null과 missing을 보내고 원본 편집을 허용한다', async () => {
    // given
    const service = documentService();
    renderDocument(service);
    // when
    await pointRead();
    // then
    expect(service.call).toHaveBeenCalledWith(expect.objectContaining({ operation: 'document.read', input: { database: 'db', container: 'container', identity: original.identity } }), expect.anything());
    expect(screen.getByLabelText('원본 문서 JSON')).not.toHaveAttribute('readonly');
    expect(screen.getByRole('button', { name: '문서 저장' })).toBeEnabled();
  });
  it('읽기 전용 연결은 원본을 조회해도 저장 삭제 생성이 비활성화된다', async () => {
    // given
    renderDocument(documentService(), null, false);
    // when
    await pointRead();
    // then
    expect(screen.getByRole('button', { name: '문서 저장' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '문서 삭제' })).toBeDisabled();
    expect(screen.getByRole('button', { name: '새 문서 작성' })).toBeDisabled();
  });
  it('원본 교체는 읽은 ETag와 identity를 바인딩하고 시스템 속성을 제거한다', async () => {
    // given
    const service = documentService();
    renderDocument(service);
    await pointRead();
    const scenario = async () => { fireEvent.change(screen.getByLabelText('원본 문서 JSON'), { target: { value: JSON.stringify({ ...original.document, title: '변경' }) } }); fireEvent.click(screen.getByRole('button', { name: '문서 저장' })); await screen.findByText('문서를 저장했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls.find(call => call[0].operation === 'document.replace')?.[0].input).toMatchObject({ database: 'db', container: 'container', identity: original.identity, if_match: 'etag-1', document: { title: '변경', _custom: '보존', array: original.document.array } });
    expect(service.call.mock.calls.find(call => call[0].operation === 'document.replace')?.[0].input).not.toHaveProperty('document._etag');
  });
  it.each([['ID', { id: 'changed' }], ['파티션 키', { tenant: 'changed' }], ['시스템 속성', { _etag: 'changed' }]])('%s 변경은 raw JSON을 유지하고 쓰기를 보내지 않는다', async (_, patch) => {
    // given
    const service = documentService();
    renderDocument(service);
    await pointRead();
    const text = JSON.stringify({ ...original.document, ...patch });
    const scenario = async () => { fireEvent.change(screen.getByLabelText('원본 문서 JSON'), { target: { value: text } }); fireEvent.click(screen.getByRole('button', { name: '문서 저장' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls.filter(call => call[0].operation === 'document.replace')).toHaveLength(0);
    expect(screen.getByLabelText('원본 문서 JSON')).toHaveValue(text);
  });
  it('ETag 충돌 뒤 수동 최신 조회는 편집 문구를 보존하고 새 ETag로만 저장한다', async () => {
    // given
    let reads = 0, writes = 0;
    const service = fakeService(async request => {
      if (request.operation === 'document.read') { reads += 1; return response(reads === 1 ? original : { ...original, document: { ...original.document, _etag: 'etag-2' }, etag: 'etag-2' }); }
      if (request.operation === 'document.replace') { writes += 1; return writes === 1 ? failure('ETAG_CONFLICT') : response(original); }
      return response(null);
    });
    renderDocument(service);
    await pointRead();
    const text = JSON.stringify({ ...original.document, title: '작성 중인 문구' });
    const scenario = async () => { fireEvent.change(screen.getByLabelText('원본 문서 JSON'), { target: { value: text } }); fireEvent.click(screen.getByRole('button', { name: '문서 저장' })); await screen.findByRole('alert'); fireEvent.click(screen.getByRole('button', { name: '최신 문서 조회' })); await screen.findByText('최신 ETag를 확인했습니다. 작성 중인 JSON은 유지했습니다.'); fireEvent.click(screen.getByRole('button', { name: '문서 저장' })); await screen.findByText('문서를 저장했습니다.'); };
    // when
    await scenario();
    // then
    expect(writes).toBe(2);
    expect(service.call.mock.calls.filter(call => call[0].operation === 'document.replace').map(call => (call[0].input as { if_match: string }).if_match)).toEqual(['etag-1', 'etag-2']);
    expect(service.call.mock.calls.filter(call => call[0].operation === 'document.replace')[1][0].input).toHaveProperty('document.title', '작성 중인 문구');
  });
  it('결과 불명의 문서 쓰기는 성공 표시와 자동 retry를 하지 않는다', async () => {
    // given
    const service = documentService(failure('OUTCOME_UNKNOWN', 'outcome_unknown'));
    renderDocument(service);
    await pointRead();
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '문서 저장' })); await screen.findByRole('alert'); };
    // when
    await scenario();
    // then
    expect(screen.getByRole('alert')).toHaveTextContent('다시 전송하지 말고');
    expect(screen.queryByText('문서를 저장했습니다.')).not.toBeInTheDocument();
    expect(service.call.mock.calls.filter(call => call[0].operation === 'document.replace')).toHaveLength(1);
  });
  it('JSON 속성의 HTML과 스크립트는 텍스트로만 표시한다', () => {
    // given
    const selection = { entry: { ...original, document: { ...original.document, title: '<img src=x onerror=alert(1)><script>evil()</script>' } }, origin: null, selection_id: 'xss' };
    // when
    const mounted = renderDocument(documentService(), selection);
    // then
    expect(mounted.container.querySelector('img')).toBeNull();
    expect(mounted.container.querySelector('script')).toBeNull();
    expect(screen.getByLabelText('원본 문서 JSON')).toHaveValue(JSON.stringify(selection.entry.document, null, 2));
  });
  it('키보드 폼 제출은 라벨이 있는 문서 ID로 조회한다', async () => {
    // given
    renderDocument();
    fireEvent.change(screen.getByLabelText('문서 ID'), { target: { value: 'doc-1' } });
    fireEvent.change(screen.getByLabelText('/tenant 값'), { target: { value: '서울' } });
    fireEvent.change(screen.getByLabelText('/nested/region 형식'), { target: { value: 'null' } });
    fireEvent.change(screen.getByLabelText('/optional 형식'), { target: { value: 'undefined' } });
    const scenario = async () => { fireEvent.submit(screen.getByLabelText('문서 ID').closest('form')!); await screen.findByLabelText('원본 문서 JSON'); };
    // when
    await scenario();
    // then
    expect(screen.getByRole('status')).toHaveTextContent('원본 문서를 조회했습니다.');
    expect(screen.getByLabelText('문서 ID')).toHaveValue('doc-1');
  });
  it('snapshot 원본은 출처 metadata와 명시적 최신 read 뒤에만 편집 가능하다', async () => {
    // given
    const service = fakeService(async request => request.operation === 'catalog.describe' ? response(containerMetadata) : response(original));
    renderDocument(service, { entry: original, origin: { database: 'actual-db', container: 'actual-container' }, selection_id: 'snapshot' });
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '최신 문서 조회' })); await screen.findByText('최신 문서를 조회했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[1][0]).toMatchObject({ operation: 'document.read', input: { database: 'actual-db', container: 'actual-container', identity: original.identity } });
    expect(screen.getByRole('button', { name: '문서 저장' })).toBeEnabled();
    expect(screen.getByLabelText('원본 문서 JSON')).not.toHaveAttribute('readonly');
  });
  it('문서 생성은 raw JSON에서 full HPK를 도출하고 시스템 필드를 보내지 않는다', async () => {
    // given
    const service = fakeService(async () => response(original));
    renderDocument(service);
    const document = { id: 'doc-1', tenant: '서울', nested: { region: null }, array: [null, true], _custom: 'keep' };
    const scenario = async () => { fireEvent.click(screen.getByRole('button', { name: '새 문서 작성' })); fireEvent.change(screen.getByLabelText('새 문서 JSON'), { target: { value: JSON.stringify(document) } }); fireEvent.click(screen.getByRole('button', { name: '문서 생성' })); await screen.findByText('문서를 저장했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[0][0]).toMatchObject({ operation: 'document.create', input: { database: 'db', container: 'container', document, partition_key: original.identity.partition_key } });
  });
  it('명시적 삭제 확인 후 읽은 ETag로만 문서를 삭제한다', async () => {
    // given
    const service = documentService();
    renderDocument(service);
    await pointRead();
    const scenario = async () => { fireEvent.click(screen.getByLabelText('이 문서 삭제 확인')); fireEvent.click(screen.getByRole('button', { name: '문서 삭제' })); await screen.findByText('문서를 삭제했습니다.'); };
    // when
    await scenario();
    // then
    expect(service.call.mock.calls[1][0]).toMatchObject({ operation: 'document.delete', input: { database: 'db', container: 'container', identity: original.identity, if_match: 'etag-1' } });
    expect(screen.queryByLabelText('원본 문서 JSON')).not.toBeInTheDocument();
  });
  it('시스템 키 metadata는 문서 생성 버튼을 비활성화한다', () => {
    // given
    const service = documentService();
    // when
    render(<DocumentPanel service={service} connection={connection} database="db" container="container" meta={{ ...containerMetadata, system_key: true }} selection={null} />);
    // then
    expect(screen.getByRole('button', { name: '새 문서 작성' })).toBeDisabled();
    expect(service.call).not.toHaveBeenCalled();
  });
});


it("Azure CLI를 선택하면 공식 클라이언트를 지정하고 직접 변경을 막는다", () => {
  // given
  const setExtraField = vi.fn();
  render(<ConnectionExtraFields extra={{ auth_mode: 'entra_user', tenant_id: 'synthetic', client_id: 'synthetic' }} setExtraField={setExtraField} setCredentialFieldsHidden={vi.fn()} />);
  // when
  fireEvent.change(screen.getByLabelText('로그인 소스'), { target: { value: 'azure_cli' } });
  // then
  expect(setExtraField).toHaveBeenCalledWith('auth_source', 'azure_cli');
  expect(setExtraField).toHaveBeenCalledWith('client_id', '04b07795-8ddb-461a-bbee-02f9e1bf7b46');
  expect(screen.getByLabelText('클라이언트 ID')).toHaveAttribute('readonly');
});
