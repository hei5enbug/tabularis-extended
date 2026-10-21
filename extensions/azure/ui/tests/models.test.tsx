import { describe, expect, it } from 'vitest';
import { challengeUrl, connections, documentPayload, entry, metadata, parameters, parseJson, partitionFromDocument, resultData, settingsFromFields, snapshotRef, validatePartition } from '../src/models';
import { keyComponents } from '../src/PartitionKeyFields';
import { connection, containerMetadata, json, original } from './fixtures';

describe('원본 문서와 설정 검증', () => {
  it('세 구성 요소의 중첩 값과 null 및 missing을 순서대로 보존한다', () => {
    // given
    const document = original.document;
    // when
    const key = partitionFromDocument(document, containerMetadata);
    // then
    expect(key).toEqual(original.identity.partition_key);
  });
  it('따옴표로 감싼 slash 속성과 배열의 중첩 경로를 원본 규칙대로 읽는다', () => {
    // given
    const document = { 'tenant/name': '서울', records: [{ region: false }] };
    const meta = { ...containerMetadata, partition_key_paths: ['/"tenant/name"', '/records/0/region'] };
    // when
    const key = partitionFromDocument(document, meta);
    // then
    expect(key).toEqual([{ type: 'string', value: '서울' }, { type: 'boolean', value: false }]);
  });
  it('typed 구성 요소의 객체 필드 순서가 달라도 동일 identity로 교체한다', () => {
    // given
    const reordered = { ...original, identity: { ...original.identity, partition_key: [{ value: '서울', type: 'string' as const }, { type: 'null' as const }, { type: 'undefined' as const }] } };
    // when
    const payload = documentPayload(JSON.stringify(original.document), containerMetadata, reordered);
    // then
    expect(payload.id).toBe('doc-1');
    expect(payload.tenant).toBe('서울');
  });
  it('날짜 배열 사용자 밑줄 속성은 보존하고 시스템 속성만 제거한다', () => {
    // given
    const text = JSON.stringify(original.document);
    // when
    const payload = documentPayload(text, containerMetadata, original);
    // then
    expect(payload).toEqual({ id: 'doc-1', tenant: '서울', nested: { region: null }, date: '2026-10-03T00:00:00Z', array: [1, null, { child: true }], _custom: '보존' });
    expect(payload).not.toHaveProperty('_etag');
  });
  it.each([
    ['ID 변경', { ...original.document, id: 'changed' }],
    ['파티션 키 변경', { ...original.document, tenant: '다른 값' }],
    ['시스템 ETag 변경', { ...original.document, _etag: 'changed' }],
    ['시스템 시간 변경', { ...original.document, _ts: 3 }],
  ])('%s을 원본 교체에서 거부한다', (_, document) => {
    // given
    const text = JSON.stringify(document);
    // when
    const action = () => documentPayload(text, containerMetadata, original);
    // then
    expect(action).toThrow();
  });
  it.each(['9007199254740993', '1e309', '-9007199254740993', '{"nested":[9007199254740993]}'])('정확하게 표현할 수 없는 숫자 %s를 거부한다', text => {
    // given
    const input = text;
    // when
    const action = () => parseJson(input);
    // then
    expect(action).toThrow('UNSAFE_NUMBER');
  });
  it('숫자로 보이는 문자열은 손실 없이 보존한다', () => {
    // given
    const text = '{"id":"9007199254740993","value":null,"date":"2026-10-03"}';
    // when
    const value = parseJson(text);
    // then
    expect(value).toEqual({ id: '9007199254740993', value: null, date: '2026-10-03' });
  });
  it('부분 파티션 키는 문서 쓰기에서 거부한다', () => {
    // given
    const key = original.identity.partition_key.slice(0, 1);
    // when
    const action = () => validatePartition(key, containerMetadata);
    // then
    expect(action).toThrow('INCOMPLETE_PARTITION_KEY');
  });
  it('부분 파티션 키는 쿼리 범위에서 허용한다', () => {
    // given
    const key = original.identity.partition_key.slice(0, 1);
    // when
    const action = () => validatePartition(key, containerMetadata, true);
    // then
    expect(action).not.toThrow();
  });
  it('파티션 metadata가 없는 describe 응답을 거부한다', () => {
    // given
    const data = json({ columns: [] });
    // when
    const action = () => metadata(data);
    // then
    expect(action).toThrow('METADATA_UNAVAILABLE');
  });
  it('시스템 파티션 키 컨테이너의 쓰기를 거부한다', () => {
    // given
    const text = JSON.stringify(original.document);
    // when
    const action = () => documentPayload(text, { ...containerMetadata, system_key: true }, original);
    // then
    expect(action).toThrow('SYSTEM_PARTITION_KEY');
  });
  it('타입 입력의 null과 missing을 별도 wire 값으로 만든다', () => {
    // given
    const fields = [{ type: 'null' as const, value: '' }, { type: 'undefined' as const, value: '' }, { type: 'boolean' as const, value: 'false' }];
    // when
    const key = keyComponents(fields);
    // then
    expect(key).toEqual([{ type: 'null' }, { type: 'undefined' }, { type: 'boolean', value: false }]);
  });
  it('SELECT VALUE scalar와 배열을 원본 문서로 추측하지 않는다', () => {
    // given
    const data = json({ kind: 'json_values', values: [null, 3, ['a'], { id: 'looks-original', _etag: 'etag' }] });
    // when
    const result = resultData(data);
    // then
    expect(result.documents).toEqual([]);
    expect(result.values).toEqual([null, 3, ['a'], { id: 'looks-original', _etag: 'etag' }]);
  });
  it('원본 entry의 불일치 ID를 거부한다', () => {
    // given
    const data = json({ ...original, identity: { ...original.identity, id: 'another' } });
    // when
    const action = () => entry(data);
    // then
    expect(action).toThrow('INVALID_DOCUMENT_ENTRY');
  });
  it('출처가 있는 원본 결과의 DB와 컨테이너만 소비한다', () => {
    // given
    const data = json({ kind: 'documents', documents: [original], origin: { database: 'actual-db', container: 'actual-container' } });
    // when
    const result = resultData(data);
    // then
    expect(result.origin).toEqual({ database: 'actual-db', container: 'actual-container' });
    expect(result.documents[0].etag).toBe('etag-1');
  });
  it('신뢰 가능한 네 가지 snapshot 식별자로 정확한 ordinal을 만든다', () => {
    // given
    const context = { resultId: 'result-a', resultSetIndex: 2, resultGeneration: 5, resultRowOrdinal: 7, rowIndex: 99 };
    // when
    const snapshot = snapshotRef(context);
    // then
    expect(snapshot).toEqual({ result_id: 'result-a', result_set_index: 2, generation: 5, row_ordinal: 7 });
  });
  it.each([{ resultId: 'a', resultSetIndex: 0, resultGeneration: 0, rowIndex: 1 }, { resultId: 'a', resultSetIndex: 0, resultGeneration: 0, resultRowOrdinal: -1 }, { resultId: 'a', resultSetIndex: 0, resultGeneration: 0, resultRowOrdinal: 0, isInsertion: true }])('불완전하거나 삽입 중인 snapshot은 추측하지 않는다 %j', context => {
    // given
    const input = context;
    // when
    const snapshot = snapshotRef(input);
    // then
    expect(snapshot).toBeNull();
  });
  it('연결 목록에서 Cosmos 연결만 선택 가능하게 한다', () => {
    // given
    const data = json({ connections: [connection, { ...connection, connection_id: 'pg', driver: 'postgresql' }] });
    // when
    const list = connections(data);
    // then
    expect(list).toEqual([connection]);
  });
  it('기존 설정 구조를 유지하면서 공개 연결 필드만 작성한다', () => {
    // given
    const fields = { endpoint: 'https://example.documents.azure.com/', auth_mode: 'entra_user' as const, tenant_id: 'tenant', client_id: 'client', database: 'db' };
    // when
    const settings = settingsFromFields(fields, { name: 'existing', extra: { custom: 'keep' } });
    // then
    expect(settings).toEqual({ name: 'existing', extra: { ...fields, endpoint: 'https://example.documents.azure.com', custom: 'keep' } });
    expect(settings).not.toHaveProperty('password');
  });
  it.each(['http://example.documents.azure.com', 'https://user:secret@example.documents.azure.com', 'https://example.mongo.cosmos.azure.com', 'https://example.documents.azure.com/?key=secret'])('비밀 또는 잘못된 endpoint %s를 거부한다', endpoint => {
    // given
    const fields = { endpoint, auth_mode: 'account_key' as const, tenant_id: '', client_id: '', database: '' };
    // when
    const action = () => settingsFromFields(fields);
    // then
    expect(action).toThrow('INVALID_ENDPOINT');
  });
  it.each([
    'http://microsoft.com/devicelogin', 'https://login.example.com/devicelogin', 'https://login.microsoft.com/devicelogin',
    'https://microsoft.com.evil.example/devicelogin', 'https://microsoft.com/another', 'https://microsoft.com/devicelogin/',
    'https://microsoft.com:444/devicelogin', 'https://user:pass@microsoft.com/devicelogin', 'https://@microsoft.com/devicelogin',
    'https://microsoft.com/devicelogin?code=123', 'https://microsoft.com/devicelogin#code',
    'https://microsoft.com/devicelogin?', 'https://microsoft.com/devicelogin#', 'javascript:alert(1)',
  ])('공식 URI 조건을 벗어난 인증 링크 %s를 표시하지 않는다', url => {
    // given
    const input = url;
    // when
    const safe = challengeUrl(input);
    // then
    expect(safe).toBeNull();
  });
  it.each(['https://microsoft.com/devicelogin', 'https://microsoft.com:443/devicelogin'])('공식 device 인증 주소 %s만 허용한다', url => {
    // given
    const input = url;
    // when
    const safe = challengeUrl(input);
    // then
    expect(safe).toBe('https://microsoft.com/devicelogin');
  });
  it('이름 있는 매개변수의 JSON 값을 보존한다', () => {
    // given
    const text = '[{"name":"@tenant","value":{"x":[null,true]}}]';
    // when
    const values = parameters(text);
    // then
    expect(values).toEqual([{ name: '@tenant', value: { x: [null, true] } }]);
  });
});
