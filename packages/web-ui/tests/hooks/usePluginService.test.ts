import {beforeEach, describe, expect, it, vi} from 'vitest';
import {renderHook} from '@testing-library/react';
import {invoke} from '@tauri-apps/api/core';
import type {ServiceRequest, ServiceResponse} from '@tabularis/service-contracts';
import {usePluginService} from '../../src/hooks/usePluginService';

const request: ServiceRequest = {
    protocol_version: 1,
    operation: 'query.execute',
    request_id: 'fixture-request',
    connection_id: 'conn-a',
    deadline_ms: 30000,
    input: {language: 'sql', text: 'SELECT 1', parameters: [], page_size: 10},
};
const response = (
    status: ServiceResponse['status'],
    data: ServiceResponse['data'] = null,
): ServiceResponse => ({
    protocol_version: 1,
    request_id: request.request_id,
    connection_id: 'conn-a',
    status,
    job_id: 'job-a',
    result_id: null,
    data,
    page: {next_token: null, has_more: false, resume_mode: 'none'},
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 0, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
});

describe('plugin service', () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
    });
    it('명시한연결과원문요청을서비스에전달한다', async () => {
        // Given
        const terminal = response('succeeded', {kind: 'json_values', values: [1]});
        vi.mocked(invoke).mockResolvedValue(terminal);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.call(request);
        // Then
        expect(invoke).toHaveBeenCalledExactlyOnceWith('service_call', {request});
        expect(actual).toEqual(terminal);
    });
    it('queued응답은ownerjob의최종결과가될때까지기다린다', async () => {
        // Given
        const terminal = response('succeeded', {kind: 'json_values', values: [1]});
        vi.mocked(invoke)
            .mockResolvedValueOnce(response('queued', {owner_instance_id: 'owner-a'}))
            .mockResolvedValueOnce(response('succeeded', {status: 'succeeded'}))
            .mockResolvedValueOnce(
                response('succeeded', {response: terminal as unknown as ServiceResponse['data']}),
            );
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.call(request);
        // Then
        expect(actual).toEqual(terminal);
        expect(invoke).toHaveBeenNthCalledWith(2, 'service_call', {
            request: expect.objectContaining({
                operation: 'job.get',
                connection_id: 'conn-a',
                input: {job_id: 'job-a', owner_instance_id: 'owner-a'},
            }),
        });
        expect(invoke).toHaveBeenNthCalledWith(3, 'service_call', {
            request: expect.objectContaining({operation: 'job.result'}),
        });
    });
    it('abort는동일연결의ownerjob을취소하고확정상태를반환한다', async () => {
        // Given
        const controller = new AbortController();
        const terminal = response('cancelled');
        let jobReady: (() => void) | undefined;
        const ready = new Promise<void>((resolve) => {
            jobReady = resolve;
        });
        vi.mocked(invoke).mockImplementation(async (_command, args) => {
            const operation = (args as {request: ServiceRequest}).request.operation;
            if (operation === 'query.execute') {
                jobReady?.();
                return response('queued', {owner_instance_id: 'owner-a'});
            }
            if (operation === 'job.get')
                return response('succeeded', {
                    status: controller.signal.aborted ? 'cancelled' : 'running',
                });
            if (operation === 'job.result')
                return response('succeeded', {
                    response: terminal as unknown as ServiceResponse['data'],
                });
            return response('succeeded', {cancel_requested: true});
        });
        const {result} = renderHook(usePluginService);
        const abortScenario = async () => {
            const pending = result.current.call(request, {signal: controller.signal});
            await ready;
            controller.abort();
            return pending;
        };
        // When
        const actual = await abortScenario();
        // Then
        expect(actual.status).toBe('cancelled');
        expect(invoke).toHaveBeenCalledWith('service_call', {
            request: expect.objectContaining({
                operation: 'job.cancel',
                connection_id: 'conn-a',
                input: {job_id: 'job-a', owner_instance_id: 'owner-a'},
            }),
        });
    });
    it('capability조회는연결ID를명시하고host명령을사용한다', async () => {
        // Given
        const capabilities = {
            service_protocol: 1,
            operations: ['query.execute'],
            spatial_v1: false,
            documents_v1: false,
            cancel_v1: true,
        };
        vi.mocked(invoke).mockResolvedValue(capabilities);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.capabilities('conn-a');
        // Then
        expect(actual).toEqual(capabilities);
        expect(invoke).toHaveBeenCalledExactlyOnceWith('service_capabilities', {
            connectionId: 'conn-a',
        });
    });

    it('job조회접근거부는원래요청의미확정결과로반환한다', async () => {
        // Given
        const denied = {
            ...response('failed'),
            request_id: 'control-request',
            error: {
                code: 'PERMISSION_DENIED' as const,
                message: 'Access denied.',
                retryable: false,
                outcome: 'not_started' as const,
                details: null,
            },
        };
        vi.mocked(invoke)
            .mockResolvedValueOnce(response('queued', {owner_instance_id: 'owner-a'}))
            .mockResolvedValueOnce(denied);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.call(request);
        // Then
        expect(actual.request_id).toBe(request.request_id);
        expect(actual.connection_id).toBe('conn-a');
        expect(actual.status).toBe('outcome_unknown');
        expect(actual.error).toMatchObject({
            code: 'PERMISSION_DENIED',
            message: 'Access denied.',
            outcome: 'unknown',
        });
        expect(actual.data).toBeNull();
        expect(actual.result_id).toBeNull();
        expect(actual.page).toEqual({next_token: null, has_more: false, resume_mode: 'none'});
    });

    it('owner단절로최종결과를읽지못해도write를시작전상태로표시하지않는다', async () => {
        // Given
        const write: ServiceRequest = {
            ...request,
            operation: 'row.insert',
            input: {table: {database: 'fixture', schema: null, table: 'items'}, values: {id: 1}},
        };
        const unavailable = {
            ...response('failed'),
            request_id: 'result-control',
            data: {owner: 'unavailable'},
            error: {
                code: 'DRIVER_EXITED' as const,
                message: 'The owner is unavailable.',
                retryable: false,
                outcome: 'not_started' as const,
                details: null,
            },
        };
        vi.mocked(invoke)
            .mockResolvedValueOnce(response('queued', {owner_instance_id: 'owner-a'}))
            .mockResolvedValueOnce(response('succeeded', {status: 'succeeded'}))
            .mockResolvedValueOnce(unavailable);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.call(write);
        // Then
        expect(actual.request_id).toBe(write.request_id);
        expect(actual.connection_id).toBe(write.connection_id);
        expect(actual.status).toBe('outcome_unknown');
        expect(actual.error).toMatchObject({
            code: 'DRIVER_EXITED',
            message: 'The owner is unavailable.',
            outcome: 'unknown',
        });
        expect(actual.data).toBeNull();
        expect(actual.result_id).toBeNull();
    });
});

describe('GUI credential helper', () => {
    const secret = 'synthetic-sdk-credential-canary';
    const channel = {kind: 'gui_buffer', channel_id: '12345678-1234-4567-89ab-123456789abc'};
    const terminalFor = (
        request: ServiceRequest,
        status: ServiceResponse['status'] = 'succeeded',
    ): ServiceResponse => ({
        ...response(status),
        request_id: request.request_id,
        connection_id: request.connection_id ?? null,
        job_id: null,
    });
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
    });

    it('비밀은_native_등록에만_보내고_공통_요청과_응답에는_채널_ID만_사용한다', async () => {
        // Given
        const common: ServiceRequest[] = [];
        vi.mocked(invoke).mockImplementation(async (command, args) => {
            if (command === 'service_gui_secret_register') return channel;
            if (command === 'service_gui_secret_discard') return null;
            const input = (args as {request: ServiceRequest}).request;
            common.push(input);
            return terminalFor(input);
        });
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.importCredential('conn-a', 'db_password', secret, {
            persistence: 'session',
        });
        // Then
        expect(invoke).toHaveBeenNthCalledWith(1, 'service_gui_secret_register', {
            connectionId: 'conn-a',
            requestId: actual.request_id,
            secret,
        });
        expect(common).toEqual([
            {
                protocol_version: 1,
                operation: 'credential.import',
                request_id: actual.request_id,
                connection_id: 'conn-a',
                session_id: null,
                map_id: null,
                expected_version: null,
                deadline_ms: 30000,
                input: {slot: 'db_password', input_channel: channel, persistence: 'session'},
            },
        ]);
        expect(actual.request_id).toMatch(/^[0-9a-f-]{36}$/);
        expect(JSON.stringify(common)).not.toContain(secret);
        expect(JSON.stringify(actual)).not.toContain(secret);
        expect(invoke).toHaveBeenNthCalledWith(3, 'service_gui_secret_discard', {
            requestId: actual.request_id,
        });
    });

    it('native_등록_실패는_고정_오류로_전달하고_비밀_없이_채널을_정리한다', async () => {
        // Given
        vi.mocked(invoke).mockRejectedValueOnce(new Error(secret)).mockResolvedValueOnce(null);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current
            .importCredential('conn-a', 'cosmos_account_key', secret, {persistence: 'keychain'})
            .catch((error: unknown) => error);
        // Then
        expect(actual).toEqual(new Error('The credential input could not be registered.'));
        expect(invoke).toHaveBeenCalledTimes(2);
        expect(invoke).toHaveBeenLastCalledWith('service_gui_secret_discard', {
            requestId: expect.any(String),
        });
        expect(JSON.stringify(vi.mocked(invoke).mock.calls.slice(1))).not.toContain(secret);
    });

    it.each([
        ['비밀 필드', {...channel, secret}],
        ['대문자 UUID', {...channel, channel_id: channel.channel_id.toUpperCase()}],
        ['빈 UUID', {...channel, channel_id: ''}],
        [
            '잘못된 version',
            {...channel, channel_id: channel.channel_id.replace('-4567-', '-1567-')},
        ],
        [
            '잘못된 variant',
            {...channel, channel_id: channel.channel_id.replace('-89ab-', '-79ab-')},
        ],
        ['다른 kind', {kind: 'protected_stdin'}],
    ])('native가 반환한 %s 채널을 거부하고 공통 요청을 보내지 않는다', async (_label, invalid) => {
        // Given
        vi.mocked(invoke).mockResolvedValueOnce(invalid).mockResolvedValueOnce(null);
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current
            .importCredential('conn-a', 'entra_client_secret', secret, {persistence: 'session'})
            .catch((error: unknown) => error);
        // Then
        expect(actual).toEqual(new Error('The credential input could not be registered.'));
        expect(invoke).toHaveBeenCalledTimes(2);
        expect(invoke).toHaveBeenLastCalledWith('service_gui_secret_discard', {
            requestId: expect.any(String),
        });
    });

    it('공통_import_실패_응답을_보존하고_cleanup_실패는_응답을_덮지_않는다', async () => {
        // Given
        vi.mocked(invoke).mockImplementation(async (command, args) => {
            if (command === 'service_gui_secret_register') return channel;
            if (command === 'service_gui_secret_discard')
                throw new Error('Synthetic cleanup failure.');
            const input = (args as {request: ServiceRequest}).request;
            return {
                ...terminalFor(input, 'failed'),
                error: {
                    code: 'PERMISSION_DENIED',
                    message: 'Access denied.',
                    outcome: 'not_started',
                    retryable: false,
                    details: null,
                },
            };
        });
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.importCredential(
            'conn-a',
            'entra_refresh_token',
            secret,
            {persistence: 'keychain'},
        );
        // Then
        expect(actual.error).toMatchObject({code: 'PERMISSION_DENIED', outcome: 'not_started'});
        expect(JSON.stringify(actual)).not.toContain(secret);
        expect(invoke).toHaveBeenLastCalledWith('service_gui_secret_discard', {
            requestId: actual.request_id,
        });
    });

    it('등록_대기_중_abort되면_공통_요청을_보내지_않고_채널을_정리한다', async () => {
        // Given
        const controller = new AbortController();
        vi.mocked(invoke).mockImplementation(async (command) => {
            if (command === 'service_gui_secret_register') {
                controller.abort();
                return channel;
            }
            return null;
        });
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current
            .importCredential('conn-a', 'db_password', secret, {
                persistence: 'session',
                signal: controller.signal,
            })
            .catch((error: unknown) => error);
        // Then
        expect(actual).toMatchObject({name: 'AbortError'});
        expect(invoke).toHaveBeenCalledTimes(2);
        expect(invoke).toHaveBeenLastCalledWith('service_gui_secret_discard', {
            requestId: expect.any(String),
        });
    });

    it('실행_중_abort는_원래_owner_job을_취소한_뒤_확정_응답과_cleanup을_완료한다', async () => {
        // Given
        const controller = new AbortController();
        let original: ServiceRequest | undefined;
        vi.mocked(invoke).mockImplementation(async (command, args) => {
            if (command === 'service_gui_secret_register') return channel;
            if (command === 'service_gui_secret_discard') return null;
            const input = (args as {request: ServiceRequest}).request;
            if (input.operation === 'credential.import') {
                original = input;
                return {
                    ...terminalFor(input, 'queued'),
                    job_id: 'job-a',
                    data: {owner_instance_id: 'gui-owner'},
                };
            }
            if (input.operation === 'job.get') {
                controller.abort();
                return {...terminalFor(input), data: {status: 'cancelled'}};
            }
            if (input.operation === 'job.result')
                return {
                    ...terminalFor(input),
                    data: {
                        response: terminalFor(
                            original!,
                            'cancelled',
                        ) as unknown as ServiceResponse['data'],
                    },
                };
            return terminalFor(input);
        });
        const {result} = renderHook(usePluginService);
        // When
        const actual = await result.current.importCredential('conn-a', 'db_password', secret, {
            persistence: 'session',
            signal: controller.signal,
        });
        // Then
        expect(actual.status).toBe('cancelled');
        expect(actual.request_id).toBe(original?.request_id);
        expect(invoke).toHaveBeenCalledWith('service_call', {
            request: expect.objectContaining({
                operation: 'job.cancel',
                connection_id: 'conn-a',
                input: {job_id: 'job-a', owner_instance_id: 'gui-owner'},
            }),
        });
        expect(
            JSON.stringify(
                vi.mocked(invoke).mock.calls.filter(([command]) => command === 'service_call'),
            ),
        ).not.toContain(secret);
        expect(invoke).toHaveBeenLastCalledWith('service_gui_secret_discard', {
            requestId: actual.request_id,
        });
    });
});
