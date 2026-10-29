import {useEffect, useState} from 'react';
import {useTranslation} from 'react-i18next';
import type {ServiceQuerySession} from '@/types/editor';
import type {useServiceQuery} from '@/hooks/useServiceQuery';
import {
    connectionWriteState,
    permissionPatch,
    requireServiceSuccess,
    serviceRequest,
    supportsServiceQuery,
} from '@/utils/serviceQuery';

interface ServiceQueryControlsProps {
    connectionId: string;
    database: string;
    query: ReturnType<typeof useServiceQuery>;
    session?: ServiceQuerySession;
    allowExecutionWrites?: boolean;
    onChange: (state: {
        serviceSession?: ServiceQuerySession;
        serviceAllowExecutionWrites?: boolean;
    }) => void;
}

export function ServiceQueryControls({
    connectionId,
    database,
    query,
    session,
    allowExecutionWrites = false,
    onChange,
}: ServiceQueryControlsProps) {
    const {t} = useTranslation();
    const [state, setState] = useState<{
        connectionId: string;
        supported: boolean;
        allowWrites: boolean;
        version: number;
    } | null>(null);
    const [error, setError] = useState('');
    const [busy, setBusy] = useState(false);
    useEffect(() => {
        const controller = new AbortController();
        void supportsServiceQuery(query.service, connectionId)
            .then(async (supported) => {
                if (!supported) return null;
                const permissions = connectionWriteState(
                    await query.service.call(serviceRequest(null, 'connection.list', {}), {
                        signal: controller.signal,
                    }),
                    connectionId,
                );
                return {connectionId, supported, ...permissions};
            })
            .then((next) => {
                if (next && !controller.signal.aborted) setState(next);
            })
            .catch((cause: unknown) => {
                if (!controller.signal.aborted)
                    setError(cause instanceof Error ? cause.message : 'serviceQuery.failed');
            });
        return () => controller.abort();
    }, [connectionId, query.service]);
    if (state?.connectionId !== connectionId || !state.supported)
        return error ? (
            <p role="alert" className="text-xs text-accent-error">
                {error}
            </p>
        ) : null;

    const act = async (action: () => Promise<void>) => {
        setBusy(true);
        setError('');
        try {
            await action();
        } catch (cause) {
            setError(cause instanceof Error ? cause.message : t('serviceQuery.failed'));
        } finally {
            setBusy(false);
        }
    };
    const changePermission = (allowWrites: boolean) =>
        act(async () => {
            const response = await query.service.call(
                serviceRequest(
                    connectionId,
                    'connection.update',
                    {patch: permissionPatch(allowWrites)},
                    null,
                    state.version,
                ),
            );
            if (response.error?.code === 'VERSION_CONFLICT') {
                const current = connectionWriteState(
                    await query.service.call(serviceRequest(null, 'connection.list', {})),
                    connectionId,
                );
                setState({...state, ...current});
            }
            requireServiceSuccess(response);
            const permissions = connectionWriteState(
                await query.service.call(serviceRequest(null, 'connection.list', {})),
                connectionId,
            );
            setState({...state, ...permissions});
            if (!permissions.allowWrites)
                onChange({serviceSession: session, serviceAllowExecutionWrites: false});
        });
    const finish = (action: 'session.commit' | 'session.rollback' | 'session.close') =>
        act(async () => {
            if (!session || session.connectionId !== connectionId)
                throw new Error(t('serviceQuery.sessionRequired'));
            await query.finishSession(session, action);
            onChange({
                serviceSession: undefined,
                serviceAllowExecutionWrites: allowExecutionWrites,
            });
        });
    return (
        <div className="flex flex-wrap items-center gap-2 p-2 text-xs bg-elevated border-b border-default">
            <label className="flex gap-1">
                <input
                    type="checkbox"
                    checked={state.allowWrites}
                    disabled={busy || !!session}
                    onChange={(event) => {
                        void changePermission(event.target.checked);
                    }}
                />
                {t('serviceQuery.connectionWrites')}
            </label>
            <label className="flex gap-1">
                <input
                    type="checkbox"
                    checked={allowExecutionWrites}
                    disabled={busy || !state.allowWrites || session?.mode === 'read_only'}
                    onChange={(event) =>
                        onChange({
                            serviceSession: session,
                            serviceAllowExecutionWrites: event.target.checked,
                        })
                    }
                />
                {t('serviceQuery.executionWrites')}
            </label>
            {!session ? (
                <>
                    <button
                        type="button"
                        disabled={busy || !database}
                        onClick={() =>
                            void act(async () =>
                                onChange({
                                    serviceSession: await query.openSession(
                                        connectionId,
                                        database,
                                        'read_only',
                                    ),
                                    serviceAllowExecutionWrites: false,
                                }),
                            )
                        }
                    >
                        {t('serviceQuery.beginReadOnly')}
                    </button>
                    <button
                        type="button"
                        disabled={busy || !database || !state.allowWrites}
                        onClick={() =>
                            void act(async () =>
                                onChange({
                                    serviceSession: await query.openSession(
                                        connectionId,
                                        database,
                                        'write',
                                    ),
                                    serviceAllowExecutionWrites: true,
                                }),
                            )
                        }
                    >
                        {t('serviceQuery.beginWrite')}
                    </button>
                </>
            ) : (
                <>
                    <span role="status">
                        {t(
                            session.mode === 'write'
                                ? 'serviceQuery.writeSession'
                                : 'serviceQuery.readOnlySession',
                        )}
                    </span>
                    <button
                        type="button"
                        disabled={busy}
                        onClick={() => void finish('session.commit')}
                    >
                        {t('serviceQuery.commit')}
                    </button>
                    <button
                        type="button"
                        disabled={busy}
                        onClick={() => void finish('session.rollback')}
                    >
                        {t('serviceQuery.rollback')}
                    </button>
                    <button
                        type="button"
                        disabled={busy}
                        onClick={() => void finish('session.close')}
                    >
                        {t('common.close')}
                    </button>
                </>
            )}
            <span className="text-muted">{t('serviceQuery.schemaHint')}</span>
            {busy && <span role="status">{t('serviceQuery.pending')}</span>}
            {error && (
                <span role="alert" className="text-accent-error">
                    {error}
                </span>
            )}
        </div>
    );
}
