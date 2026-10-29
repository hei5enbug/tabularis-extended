import {fireEvent, render, screen, waitFor} from '@testing-library/react';
import {describe, expect, it, vi} from 'vitest';
import {ServiceQueryControls} from '../../../src/components/service/ServiceQueryControls';
import type {useServiceQuery} from '../../../src/hooks/useServiceQuery';
import type {ServiceRequest, ServiceResponse} from '@tabularis/service-contracts';

const response = (request: ServiceRequest): ServiceResponse => ({
    protocol_version: 1,
    request_id: request.request_id,
    connection_id: request.connection_id ?? null,
    status: 'succeeded',
    job_id: null,
    result_id: null,
    data: {connections: [{connection_id: 'conn-a', version: 3, allow_writes: false}]},
    page: {has_more: false, next_token: null, resume_mode: 'none'},
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 0, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
});
const query = (): ReturnType<typeof useServiceQuery> => ({
    service: {
        call: vi.fn(async (request: ServiceRequest) => response(request)),
        capabilities: vi
            .fn()
            .mockResolvedValue({service_protocol: 1, operations: ['query.execute']}),
        executeWrite: vi.fn(),
        saveArtifact: vi.fn(),
        importCredential: vi.fn(),
        subscribeMap: vi.fn(),
    },
    execute: vi.fn(),
    page: vi.fn(),
    copyAll: vi.fn(),
    cancel: vi.fn(),
    openSession: vi
        .fn()
        .mockResolvedValue({sessionId: 'real-session', mode: 'read_only', connectionId: 'conn-a'}),
    finishSession: vi.fn(),
});

describe('서비스 query controls', () => {
    it('연결의 쓰기가 꺼져 있으면 실행 쓰기와 쓰기 session 버튼을 막는다', async () => {
        // Given
        const api = query();
        const scenario = async () => {
            render(
                <ServiceQueryControls
                    connectionId="conn-a"
                    database="fixture"
                    query={api}
                    onChange={vi.fn()}
                />,
            );
            await waitFor(() =>
                expect(screen.getByText('serviceQuery.connectionWrites')).toBeInTheDocument(),
            );
        };
        // When
        await scenario();
        // Then
        expect(screen.getByLabelText('serviceQuery.executionWrites')).toBeDisabled();
        expect(screen.getByText('serviceQuery.beginWrite')).toBeDisabled();
        expect(api.service.call).toHaveBeenCalledWith(
            expect.objectContaining({operation: 'connection.list', connection_id: null}),
            {signal: expect.any(AbortSignal)},
        );
    });

    it('읽기 전용 시작은 반환된 실제 session ID만 저장한다', async () => {
        // Given
        const api = query();
        const changed = vi.fn();
        render(
            <ServiceQueryControls
                connectionId="conn-a"
                database="fixture"
                query={api}
                onChange={changed}
            />,
        );
        await waitFor(() => expect(screen.getByText('serviceQuery.beginReadOnly')).toBeEnabled());
        const scenario = async () => {
            fireEvent.click(screen.getByText('serviceQuery.beginReadOnly'));
            await waitFor(() => expect(changed).toHaveBeenCalled());
        };
        // When
        await scenario();
        // Then
        expect(api.openSession).toHaveBeenCalledExactlyOnceWith('conn-a', 'fixture', 'read_only');
        expect(changed).toHaveBeenCalledWith({
            serviceSession: {sessionId: 'real-session', mode: 'read_only', connectionId: 'conn-a'},
            serviceAllowExecutionWrites: false,
        });
    });

    it('commit이 미확정이면 session을 닫았다고 표시하지 않는다', async () => {
        // Given
        const api = query();
        const changed = vi.fn();
        vi.mocked(api.finishSession).mockRejectedValue(new Error('OUTCOME_UNKNOWN: Unknown.'));
        render(
            <ServiceQueryControls
                connectionId="conn-a"
                database="fixture"
                query={api}
                session={{sessionId: 'real-session', connectionId: 'conn-a', mode: 'write'}}
                onChange={changed}
            />,
        );
        await waitFor(() => expect(screen.getByText('serviceQuery.commit')).toBeEnabled());
        const scenario = async () => {
            fireEvent.click(screen.getByText('serviceQuery.commit'));
            await screen.findByRole('alert');
        };
        // When
        await scenario();
        // Then
        expect(changed).not.toHaveBeenCalled();
        expect(screen.getByRole('alert')).toHaveTextContent('OUTCOME_UNKNOWN');
        expect(api.finishSession).toHaveBeenCalledTimes(1);
    });
});
