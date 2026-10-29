import {useCallback, useEffect, useRef} from 'react';
import type {QueryResult, ServiceQuerySession} from '@/types/editor';
import {usePluginService} from './usePluginService';
import {
    serviceRequest,
    servicePageSize,
    snapshotResult,
    supportsServiceQuery,
    requireServiceSuccess,
    serviceObject,
    readSnapshotPage,
    readAllSnapshotRows,
    connectionWriteState,
} from '@/utils/serviceQuery';

interface ServiceQueryInput {
    scopeId: string;
    connectionId: string;
    driverId: string;
    database: string;
    text: string;
    pageSize?: number;
    write?: boolean;
    session?: ServiceQuerySession;
}

export function useServiceQuery() {
    const service = usePluginService();
    const runs = useRef(
        new Map<string, {controller: AbortController; connectionId: string; generation: number}>(),
    );
    const generations = useRef(new Map<string, number>());

    useEffect(
        () => () => {
            for (const run of runs.current.values()) run.controller.abort();
            runs.current.clear();
        },
        [],
    );

    const cancel = useCallback((scopeId: string) => {
        const run = runs.current.get(scopeId);
        if (!run) return false;
        run.controller.abort();
        generations.current.set(scopeId, (generations.current.get(scopeId) ?? 0) + 1);
        runs.current.delete(scopeId);
        return true;
    }, []);

    const isCurrent = useCallback((scopeId: string, connectionId: string, generation: number) => {
        const run = runs.current.get(scopeId);
        return (
            !!run &&
            run.connectionId === connectionId &&
            run.generation === generation &&
            !run.controller.signal.aborted
        );
    }, []);

    const execute = useCallback(
        async (input: ServiceQueryInput): Promise<QueryResult | null> => {
            cancel(input.scopeId);
            const generation = (generations.current.get(input.scopeId) ?? 0) + 1;
            generations.current.set(input.scopeId, generation);
            const controller = new AbortController();
            runs.current.set(input.scopeId, {
                controller,
                connectionId: input.connectionId,
                generation,
            });
            const check = () => {
                if (!isCurrent(input.scopeId, input.connectionId, generation))
                    throw new DOMException('The request was superseded.', 'AbortError');
            };
            if (!(await supportsServiceQuery(service, input.connectionId))) {
                check();
                runs.current.delete(input.scopeId);
                return null;
            }
            check();
            if (!input.database)
                throw new Error(
                    'Select the saved connection database before running a service query.',
                );
            if (input.session && input.session.connectionId !== input.connectionId)
                throw new Error('The session belongs to another connection.');
            if (input.write && input.session?.mode === 'read_only')
                throw new Error('The session is read-only.');
            const write = input.session?.mode === 'write' || input.write === true;
            if (write) {
                const permissions = connectionWriteState(
                    await service.call(serviceRequest(null, 'connection.list', {}), {
                        signal: controller.signal,
                    }),
                    input.connectionId,
                );
                check();
                if (!permissions.allowWrites)
                    throw new Error('WRITE_NOT_ALLOWED: Enable writes for this connection first.');
            }
            const pageSize = servicePageSize(input.pageSize);
            const request = serviceRequest(
                input.connectionId,
                'query.execute',
                {
                    language: 'sql',
                    text: input.text,
                    parameters: [],
                    page_size: pageSize,
                    database: input.database,
                },
                input.session?.sessionId ?? null,
            );
            const response = await (write
                ? service.executeWrite(request, {signal: controller.signal})
                : service.call(request, {signal: controller.signal}));
            check();
            const context = {
                connectionId: input.connectionId,
                driverId: input.driverId,
                generation,
                pageSize,
            };
            const result = snapshotResult(response, context);
            const ref = result.service!;
            const additional: QueryResult[] = [];
            for (let index = 1; index < ref.resultSetCount; index++) {
                const response = await service.call(
                    serviceRequest(input.connectionId, 'result.get', {
                        result_id: ref.resultId,
                        result_set_index: index,
                        offset: 0,
                        limit: pageSize,
                    }),
                    {signal: controller.signal},
                );
                check();
                additional.push(snapshotResult(response, context));
            }
            return additional.length ? {...result, additional_results: additional} : result;
        },
        [cancel, isCurrent, service],
    );

    const page = useCallback(
        async (scopeId: string, result: QueryResult, pageNumber: number, pageSize?: number) => {
            const ref = result.service;
            if (!ref) throw new Error('The result is not a service snapshot.');
            cancel(scopeId);
            const generation = (generations.current.get(scopeId) ?? 0) + 1;
            generations.current.set(scopeId, generation);
            const controller = new AbortController();
            runs.current.set(scopeId, {controller, connectionId: ref.connectionId, generation});
            try {
                const next = await readSnapshotPage(
                    service,
                    result,
                    pageNumber,
                    pageSize,
                    controller.signal,
                );
                if (!isCurrent(scopeId, ref.connectionId, generation))
                    throw new DOMException('The page was superseded.', 'AbortError');
                return next;
            } catch (error) {
                if (!isCurrent(scopeId, ref.connectionId, generation))
                    throw new DOMException('The page was superseded.', 'AbortError');
                throw error;
            }
        },
        [cancel, isCurrent, service],
    );

    const copyAll = useCallback(
        async (scopeId: string, result: QueryResult) => {
            const ref = result.service;
            if (!ref) throw new Error('The result is not a service snapshot.');
            cancel(scopeId);
            const generation = (generations.current.get(scopeId) ?? 0) + 1;
            generations.current.set(scopeId, generation);
            const controller = new AbortController();
            runs.current.set(scopeId, {controller, connectionId: ref.connectionId, generation});
            try {
                const next = await readAllSnapshotRows(service, result, controller.signal);
                if (!isCurrent(scopeId, ref.connectionId, generation))
                    throw new DOMException('The copy was superseded.', 'AbortError');
                return next;
            } catch (error) {
                if (!isCurrent(scopeId, ref.connectionId, generation))
                    throw new DOMException('The copy was superseded.', 'AbortError');
                throw error;
            }
        },
        [cancel, isCurrent, service],
    );

    const openSession = useCallback(
        async (
            connectionId: string,
            database: string,
            mode: ServiceQuerySession['mode'],
        ): Promise<ServiceQuerySession> => {
            const response = requireServiceSuccess(
                await service.call(serviceRequest(connectionId, 'session.open', {database, mode})),
            );
            const data = serviceObject(response.data);
            if (typeof data.session_id !== 'string' || !data.session_id)
                throw new Error('The service session identity is unavailable.');
            return {sessionId: data.session_id, mode, connectionId};
        },
        [service],
    );

    const finishSession = useCallback(
        async (
            session: ServiceQuerySession,
            action: 'session.commit' | 'session.rollback' | 'session.close',
        ) => {
            requireServiceSuccess(
                await service.call(
                    serviceRequest(session.connectionId, action, {}, session.sessionId),
                ),
            );
        },
        [service],
    );

    return {service, execute, page, copyAll, cancel, openSession, finishSession};
}
