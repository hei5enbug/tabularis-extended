import { describe, expect, it, vi } from 'vitest';
import type { UsePluginServiceReturn } from '@tabularis/plugin-api';
import type { ServiceRequest } from '@tabularis/service-contracts/types';
import { completed, request, RequestScope } from '../src/service';
import { checked, checkedResponse, failure, response } from './fixtures';

describe('명시적 연결과 요청 수명', () => {
  it('각 요청에 별도 ID와 연결 및 null 세션 범위를 넣는다', () => {
    // given
    const input = { database: 'db', schema: null };
    // when
    const requests = [request('catalog.objects', 'cosmos-a', input), request('catalog.objects', 'cosmos-b', input)];
    // then
    expect(requests[0].request_id).not.toBe(requests[1].request_id);
    expect(requests[0]).toMatchObject({ connection_id: 'cosmos-a', session_id: null, map_id: null, expected_version: null, deadline_ms: 60_000 });
    expect(() => requests.forEach(checked)).not.toThrow();
  });
  it('설정 변경은 최신 기대 버전을 전달한다', () => {
    // given
    const version = 4;
    // when
    const value = request('connection.update', 'cosmos-a', { patch: { allow_writes: true } }, version);
    // then
    expect(value.expected_version).toBe(4);
    expect(() => checked(value)).not.toThrow();
  });
  it.each(['failed', 'cancelled', 'outcome_unknown', 'interrupted'] as const)('%s 응답을 성공으로 처리하지 않는다', status => {
    // given
    const value = checkedResponse(response(null, { status }));
    // when
    const action = () => completed(value);
    // then
    expect(action).toThrow();
  });
  it('확인되지 않은 쓰기는 자동 재전송하지 않는다', async () => {
    // given
    const call = vi.fn(async () => failure('OUTCOME_UNKNOWN', 'outcome_unknown'));
    const scope = new RequestScope({ call } as unknown as UsePluginServiceReturn);
    // when
    const result = await scope.call('document.delete', 'cosmos-a', { database: 'db', container: 'c', identity: { id: 'a', partition_key: [{ type: 'null' }] }, if_match: 'etag' }).catch(error => error);
    // then
    expect(result.code).toBe('OUTCOME_UNKNOWN');
    expect(call).toHaveBeenCalledTimes(1);
  });
  it('취소를 무시한 이전 응답도 적용하지 않는다', async () => {
    // given
    let resolve!: (value: ReturnType<typeof response>) => void;
    const call = vi.fn<(_request: ServiceRequest, _options?: { signal?: AbortSignal }) => Promise<ReturnType<typeof response>>>(() => new Promise(done => { resolve = done; }));
    const scope = new RequestScope({ call } as unknown as UsePluginServiceReturn);
    const scenario = async () => { const pending = scope.call('catalog.databases', 'cosmos-a', {}).catch(error => error); scope.cancel(); resolve(response({ databases: ['stale'] })); return pending; };
    // when
    const result = await scenario();
    // then
    expect(result.code).toBe('STALE_RESPONSE');
    expect(call.mock.calls[0][1]?.signal?.aborted).toBe(true);
  });
});
