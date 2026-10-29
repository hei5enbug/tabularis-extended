import {act, fireEvent, render, screen} from '@testing-library/react';
import type {ComponentProps} from 'react';
import {beforeEach, expect, it, vi} from 'vitest';
import type {ServiceRequest, ServiceResponse} from '@tabularis/service-contracts';
import {NotebookView} from '../../../src/components/notebook/NotebookView';
import type {NotebookCellWrapper} from '../../../src/components/notebook/NotebookCellWrapper';
import type {NotebookToolbar} from '../../../src/components/notebook/NotebookToolbar';
import type {NotebookState} from '../../../src/types/notebook';
import type {Tab} from '../../../src/types/editor';

const state = vi.hoisted(() => ({
    notebook: {cells: []} as NotebookState,
    call: vi.fn(),
    executeWrite: vi.fn(),
    alert: vi.fn(),
    invoke: vi.fn(),
}));
const service = vi.hoisted(() => ({
    call: state.call,
    executeWrite: state.executeWrite,
    capabilities: vi.fn().mockResolvedValue({service_protocol: 1, operations: ['query.execute']}),
}));
vi.mock('../../../src/hooks/usePluginService', () => ({usePluginService: () => service}));
vi.mock('../../../src/hooks/useDatabase', () => ({
    useDatabase: () => ({
        activeSchema: 'public',
        activeCapabilities: null,
        selectedDatabases: [],
        activeDriver: 'postgresql',
        connections: [{id: 'conn-notebook', params: {driver: 'postgresql', database: 'fixture'}}],
    }),
}));
vi.mock('../../../src/hooks/useSettings', () => ({
    useSettings: () => ({settings: {resultPageSize: 50, safetyConfirmationDelayEnabled: false}}),
}));
vi.mock('../../../src/hooks/useAlert', () => ({useAlert: () => ({showAlert: state.alert})}));
vi.mock('../../../src/hooks/useKeybindings', () => ({
    useKeybindings: () => ({matchesShortcut: () => false}),
}));
vi.mock('../../../src/hooks/useQueryGuards', () => ({
    useQueryGuards: () => ({pending: null, guardQuery: async () => true, resolve: vi.fn()}),
}));
vi.mock('../../../src/hooks/useSqlAutocompleteRegistration', () => ({
    useSqlAutocompleteRegistration: vi.fn(),
}));
vi.mock('../../../src/utils/notebookStore', () => ({
    getNotebookState: () => state.notebook,
    setNotebookState: (_id: string, value: NotebookState) => {
        state.notebook = value;
    },
    loadNotebook: vi.fn(),
    setNotebookTitle: vi.fn(),
    createNotebookFromState: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({invoke: state.invoke, isTauri: () => true}));
vi.mock('@/hooks/useTabularisClient', () => import('../../support/tauriBackedHooks'));
vi.mock('@/hooks/usePlatformCapabilities', () => import('../../support/tauriBackedHooks'));
vi.mock('../../../src/components/service/ServiceQueryControls', () => ({
    ServiceQueryControls: () => null,
}));
vi.mock('../../../src/components/modals/ConfirmModal', () => ({ConfirmModal: () => null}));
vi.mock('../../../src/components/notebook/NotebookHistoryPanel', () => ({
    NotebookHistoryPanel: () => null,
}));
vi.mock('../../../src/components/notebook/AddCellButton', () => ({AddCellButton: () => null}));
vi.mock('../../../src/components/notebook/RunAllSummary', () => ({RunAllSummary: () => null}));
vi.mock('../../../src/components/notebook/ParamsPanel', () => ({ParamsPanel: () => null}));
vi.mock('../../../src/components/notebook/NotebookOutline', () => ({NotebookOutline: () => null}));
vi.mock('../../../src/components/notebook/NotebookToolbar', () => ({
    NotebookToolbar: ({onRunAll, isRunning}: ComponentProps<typeof NotebookToolbar>) => (
        <button disabled={isRunning} onClick={onRunAll}>
            Run All
        </button>
    ),
}));
vi.mock('../../../src/components/notebook/NotebookCellWrapper', () => ({
    NotebookCellWrapper: ({cell, onRun}: ComponentProps<typeof NotebookCellWrapper>) => (
        <section>
            <output aria-label={cell.id}>{cell.error || ''}</output>
            <button onClick={onRun}>Run {cell.id}</button>
        </section>
    ),
}));

const succeeded = (request: ServiceRequest): ServiceResponse => ({
    protocol_version: 1,
    request_id: request.request_id,
    connection_id: request.connection_id ?? null,
    status: 'succeeded',
    job_id: null,
    result_id: 'result-notebook',
    data: {
        kind: 'tabular',
        columns: [{name: 'value', column_index: 0}],
        rows: [[1]],
        offset: 0,
        result_set_index: 0,
        result_set_count: 1,
        result_revision: 'rev-1',
        affected_rows: null,
    },
    page: {has_more: false, next_token: null, resume_mode: 'materialized'},
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 0, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
});
const tab = (session = true): Tab => ({
    id: 'tab-notebook',
    type: 'notebook',
    notebookId: 'fixture-notebook',
    title: 'Fixture',
    connectionId: 'conn-notebook',
    query: '',
    result: null,
    error: '',
    executionTime: null,
    page: 1,
    activeTable: null,
    pkColumns: null,
    ...(session
        ? {
              serviceSession: {
                  sessionId: 'native-session',
                  mode: 'read_only',
                  connectionId: 'conn-notebook',
              },
          }
        : {}),
});
const runAll = async () => {
    await act(async () => {
        fireEvent.click(screen.getByRole('button', {name: 'Run All'}));
    });
};

beforeEach(() => {
    state.notebook = {
        cells: [
            {id: 'first', type: 'sql', content: ' SELECT 1 ', isParallel: true},
            {id: 'second', type: 'sql', content: 'SELECT failed', isParallel: true},
            {id: 'third', type: 'sql', content: 'SELECT never', isParallel: true},
        ],
    };
    state.call.mockReset().mockImplementation(async (request: ServiceRequest) =>
        (request.input as {text: string}).text === 'SELECT failed'
            ? {
                  ...succeeded(request),
                  status: 'outcome_unknown',
                  error: {
                      code: 'OUTCOME_UNKNOWN',
                      message: 'Unknown.',
                      retryable: false,
                      outcome: 'unknown',
                      details: null,
                  },
              }
            : succeeded(request),
    );
    state.executeWrite.mockReset();
    state.alert.mockReset();
    state.invoke.mockReset();
});

it('Run All은 실제 session에서 직렬 실행하고 첫 실패 뒤 나머지 셀을 실행하지 않는다', async () => {
    // Given
    render(<NotebookView tab={tab()} updateTab={vi.fn()} connectionId="conn-notebook" isActive />);
    // When
    await runAll();
    // Then
    expect(state.call.mock.calls.map(([request]) => request.input.text)).toEqual([
        ' SELECT 1 ',
        'SELECT failed',
    ]);
    expect(
        state.call.mock.calls.every(
            ([request]) =>
                request.session_id === 'native-session' &&
                request.connection_id === 'conn-notebook' &&
                request.operation === 'query.execute',
        ),
    ).toBe(true);
    expect(screen.getByLabelText('second')).toHaveTextContent('OUTCOME_UNKNOWN');
    expect(screen.getByLabelText('third')).toHaveTextContent('serviceQuery.notRun');
    expect(state.executeWrite).not.toHaveBeenCalled();
    expect(state.invoke).not.toHaveBeenCalled();
});

it('서비스 Run All에 session이 없으면 안내하고 쿼리와 자동 트랜잭션을 실행하지 않는다', async () => {
    // Given
    render(
        <NotebookView tab={tab(false)} updateTab={vi.fn()} connectionId="conn-notebook" isActive />,
    );
    // When
    await runAll();
    // Then
    expect(state.alert).toHaveBeenCalledExactlyOnceWith('serviceQuery.sessionRequired', {
        kind: 'error',
    });
    expect(state.call).not.toHaveBeenCalled();
    expect(state.executeWrite).not.toHaveBeenCalled();
    expect(state.invoke).not.toHaveBeenCalled();
});

it('capability 응답 대기 중 연결이 바뀌면 이전 셀 상태와 쿼리 요청을 적용하지 않는다', async () => {
    // Given
    let started: (() => void) | undefined;
    let finish: ((value: {service_protocol: number; operations: string[]}) => void) | undefined;
    const waiting = new Promise<void>((resolve) => {
        started = resolve;
    });
    service.capabilities.mockImplementationOnce(() => {
        started?.();
        return new Promise((resolve) => {
            finish = resolve;
        });
    });
    const original = tab();
    const view = render(
        <NotebookView tab={original} updateTab={vi.fn()} connectionId="conn-notebook" isActive />,
    );
    const scenario = async () => {
        await act(async () => {
            fireEvent.click(screen.getByRole('button', {name: 'Run first'}));
            await waiting;
            view.rerender(
                <NotebookView
                    tab={{...original, connectionId: 'conn-next'}}
                    updateTab={vi.fn()}
                    connectionId="conn-next"
                    isActive
                />,
            );
        });
        await act(async () => {
            finish?.({service_protocol: 1, operations: ['query.execute']});
        });
    };
    // When
    await scenario();
    // Then
    expect(state.call).not.toHaveBeenCalled();
    expect(state.executeWrite).not.toHaveBeenCalled();
    expect(state.invoke).not.toHaveBeenCalled();
    expect(state.notebook.cells[0]).not.toHaveProperty('isLoading');
    expect(state.notebook.cells[0]).not.toHaveProperty('result');
    expect(screen.getByLabelText('first')).toBeEmptyDOMElement();
});
