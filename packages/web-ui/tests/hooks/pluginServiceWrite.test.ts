import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest';
import {renderHook} from '@testing-library/react';
import {invoke} from '@tauri-apps/api/core';
import type {ServiceRequest, ServiceResponse} from '@tabularis/service-contracts';
import {usePluginService} from '../../src/hooks/usePluginService';

const writeRequest = (): ServiceRequest<'query.execute'> => ({
    protocol_version: 1,
    operation: 'query.execute',
    request_id: 'write-request',
    connection_id: 'connection-a',
    input: {
        language: 'sql',
        text: 'UPDATE items SET value = @value',
        parameters: [{name: 'value', value: {nested: [1, null]}}],
    },
});
const response: ServiceResponse = {
    protocol_version: 1,
    request_id: 'write-request',
    connection_id: 'connection-a',
    status: 'succeeded',
    job_id: null,
    result_id: null,
    data: null,
    page: {next_token: null, has_more: false, resume_mode: 'none'},
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 0, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
};

describe('플러그인 쿼리 쓰기 의도', () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
    });
    afterEach(() => {
        vi.restoreAllMocks();
    });

    it('등록 성공 이후 동일한 복제 요청을 service call에 전달한다', async () => {
        // given
        const request = writeRequest();
        const clone = vi.spyOn(globalThis, 'structuredClone');
        vi.mocked(invoke).mockResolvedValueOnce(true).mockResolvedValueOnce(response);
        const {result} = renderHook(usePluginService);
        // when
        const actual = await result.current.executeWrite(request);
        // then
        expect(actual).toEqual(response);
        expect(clone).toHaveBeenCalledExactlyOnceWith(request);
        expect(invoke).toHaveBeenNthCalledWith(1, 'service_register_query_write_intent', {request});
        expect(invoke).toHaveBeenNthCalledWith(2, 'service_call', {request});
        expect(vi.mocked(invoke).mock.calls[0][1]?.request).toBe(
            vi.mocked(invoke).mock.calls[1][1]?.request,
        );
        expect(vi.mocked(invoke).mock.calls[0][1]?.request).not.toBe(request);
    });

    it('등록 대기 중 원본 SQL과 파라미터 및 ID 변경은 dispatch에 반영하지 않는다', async () => {
        // given
        const request = writeRequest();
        const expected = writeRequest();
        let complete!: (value: boolean) => void;
        vi.mocked(invoke)
            .mockImplementationOnce(
                () =>
                    new Promise<boolean>((resolve) => {
                        complete = resolve;
                    }),
            )
            .mockResolvedValueOnce(response);
        const {result} = renderHook(usePluginService);
        const scenario = async () => {
            const pending = result.current.executeWrite(request);
            request.request_id = 'changed-request';
            request.connection_id = 'changed-connection';
            request.input.text = 'DELETE FROM other_items';
            request.input.parameters![0].value = 'changed-value';
            complete(true);
            return pending;
        };
        // when
        const actual = await scenario();
        // then
        expect(actual).toEqual(response);
        expect(invoke).toHaveBeenNthCalledWith(1, 'service_register_query_write_intent', {
            request: expected,
        });
        expect(invoke).toHaveBeenNthCalledWith(2, 'service_call', {request: expected});
    });

    it('미리 취소한 요청은 복제와 등록 및 service call을 실행하지 않는다', async () => {
        // given
        const controller = new AbortController();
        controller.abort();
        const clone = vi.spyOn(globalThis, 'structuredClone');
        const {result} = renderHook(usePluginService);
        // when
        const actual = await result.current
            .executeWrite(writeRequest(), {signal: controller.signal})
            .catch((error: unknown) => error);
        // then
        expect(actual).toMatchObject({name: 'AbortError'});
        expect(clone).not.toHaveBeenCalled();
        expect(invoke).not.toHaveBeenCalled();
    });

    it('등록 성공 대기 중 취소하면 service call을 실행하지 않는다', async () => {
        // given
        const controller = new AbortController();
        vi.mocked(invoke).mockImplementationOnce(async () => {
            controller.abort();
            return true;
        });
        const {result} = renderHook(usePluginService);
        // when
        const actual = await result.current
            .executeWrite(writeRequest(), {signal: controller.signal})
            .catch((error: unknown) => error);
        // then
        expect(actual).toMatchObject({name: 'AbortError'});
        expect(invoke).toHaveBeenCalledTimes(1);
        expect(invoke).not.toHaveBeenCalledWith('service_call', expect.anything());
    });

    it('등록 실패는 고정 오류로 반환하며 service call을 실행하지 않는다', async () => {
        // given
        vi.mocked(invoke).mockRejectedValueOnce(new Error('synthetic-write-intent-canary'));
        const {result} = renderHook(usePluginService);
        // when
        const actual = await result.current
            .executeWrite(writeRequest())
            .catch((error: unknown) => error);
        // then
        expect(actual).toEqual(new Error('The query write intent could not be registered.'));
        expect(invoke).toHaveBeenCalledTimes(1);
        expect(invoke).not.toHaveBeenCalledWith('service_call', expect.anything());
    });

    it.each([false, null, undefined, 1, 'true', {}])(
        '등록 반환값 %j는 성공으로 취급하지 않는다',
        async (registered) => {
            // given
            vi.mocked(invoke).mockResolvedValueOnce(registered);
            const {result} = renderHook(usePluginService);
            // when
            const actual = await result.current
                .executeWrite(writeRequest())
                .catch((error: unknown) => error);
            // then
            expect(actual).toEqual(new Error('The query write intent could not be registered.'));
            expect(invoke).toHaveBeenCalledTimes(1);
        },
    );

    it.each([
        ['다른 protocol', {protocol_version: 2}],
        ['다른 operation', {operation: 'row.insert'}],
        ['빈 연결 ID', {connection_id: ''}],
        ['누락 연결 ID', {connection_id: null}],
        ['긴 연결 ID', {connection_id: 'a'.repeat(129)}],
        ['빈 요청 ID', {request_id: ''}],
        ['긴 요청 ID', {request_id: 'a'.repeat(129)}],
        ['NUL 요청 ID', {request_id: 'a\0b'}],
    ])('%s 요청은 native 명령 이전에 거부한다', async (_label, override) => {
        // given
        const request = {
            ...writeRequest(),
            ...override,
        } as unknown as ServiceRequest<'query.execute'>;
        const {result} = renderHook(usePluginService);
        // when
        const actual = await result.current.executeWrite(request).catch((error: unknown) => error);
        // then
        expect(actual).toBeInstanceOf(Error);
        expect(invoke).not.toHaveBeenCalled();
    });
});

describe('플러그인 artifact 저장', () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
    });

    it.each([true, false])(
        'native 저장 결과 %j를 임의 path 없이 그대로 반환한다',
        async (saved) => {
            // given
            vi.mocked(invoke).mockResolvedValueOnce(saved);
            const {result} = renderHook(usePluginService);
            // when
            const actual = await result.current.saveArtifact('artifact-id');
            // then
            expect(actual).toBe(saved);
            expect(invoke).toHaveBeenCalledExactlyOnceWith('service_save_artifact', {
                artifactId: 'artifact-id',
            });
        },
    );

    it.each(['', ' ', 'a'.repeat(129), 'artifact\0id'])(
        '유효하지 않은 artifact ID %j는 native 명령 이전에 거부한다',
        async (artifactId) => {
            // given
            const {result} = renderHook(usePluginService);
            // when
            const actual = await result.current
                .saveArtifact(artifactId)
                .catch((error: unknown) => error);
            // then
            expect(actual).toEqual(new Error('A valid artifact identity is required.'));
            expect(invoke).not.toHaveBeenCalled();
        },
    );

    it.each([null, undefined, 0, 'false', {}])(
        'artifact 저장 반환값 %j는 boolean으로 변환하지 않는다',
        async (saved) => {
            // given
            vi.mocked(invoke).mockResolvedValueOnce(saved);
            const {result} = renderHook(usePluginService);
            // when
            const actual = await result.current
                .saveArtifact('artifact-id')
                .catch((error: unknown) => error);
            // then
            expect(actual).toEqual(new Error('The artifact save response is invalid.'));
            expect(invoke).toHaveBeenCalledTimes(1);
        },
    );
});
