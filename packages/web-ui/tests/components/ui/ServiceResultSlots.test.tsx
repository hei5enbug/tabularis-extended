import {render, screen, fireEvent} from '@testing-library/react';
import type {ComponentProps} from 'react';
import {describe, expect, it, vi} from 'vitest';
import {ResultEntryContent} from '../../../src/components/ui/ResultEntryContent';
import {RowEditorPanel} from '../../../src/components/ui/RowEditorPanel';
import type {DataGrid} from '../../../src/components/ui/DataGrid';
import type {SlotContext} from '../../../src/types/pluginSlots';
import {snapshotResult} from '../../../src/utils/serviceQuery';
import type {ServiceResponse} from '@tabularis/service-contracts';

const state = vi.hoisted(() => ({
    contexts: [] as {name: string; context: SlotContext}[],
    grids: [] as ComponentProps<typeof DataGrid>[],
    update: vi.fn(),
}));
vi.mock('../../../src/components/ui/DataGrid', () => ({
    DataGrid: (props: ComponentProps<typeof DataGrid>) => {
        state.grids.push(props);
        return <div data-testid="snapshot-grid" />;
    },
}));
vi.mock('../../../src/components/ui/SlotAnchor', () => ({
    SlotAnchor: (props: {name: string; context: SlotContext}) => {
        state.contexts.push(props);
        return <span data-slot={props.name} />;
    },
}));
vi.mock('../../../src/hooks/useDatabase', () => ({
    useDatabase: () => ({connectionDataMap: {}}),
}));
vi.mock('../../../src/hooks/useRowEditor', () => ({
    useRowEditor: ({initialData}: {initialData: Record<string, unknown>}) => ({
        editedData: initialData,
        updateField: state.update,
    }),
}));
vi.mock('../../../src/components/ui/FieldEditor', () => ({
    FieldEditor: (props: {name: string; value: unknown; onChange: (value: unknown) => void}) => (
        <input
            aria-label={props.name}
            value={String(props.value)}
            onChange={(event) => props.onChange(event.target.value)}
        />
    ),
}));
const envelope: ServiceResponse = {
    protocol_version: 1,
    request_id: 'req',
    connection_id: 'conn-origin',
    status: 'succeeded',
    job_id: null,
    result_id: 'snapshot-a',
    data: {
        kind: 'tabular',
        columns: [{name: 'label', column_index: 0}],
        rows: [['<img src=x onerror=alert(1)>']],
        result_set_index: 1,
        result_set_count: 2,
        offset: 300,
        result_revision: 'rev-a',
    },
    page: {has_more: false, next_token: null, resume_mode: 'materialized'},
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 1, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
};

it.each([false, true])(
    '일반과 compact 결과는 snapshot 연결과 집합 참조를 그대로 grid에 전달한다 (%s)',
    (compact) => {
        // Given
        state.grids.length = 0;
        const result = snapshotResult(envelope, {
            connectionId: 'conn-origin',
            driverId: 'postgresql',
            generation: 4,
            pageSize: 100,
        });
        const entry = {
            id: 'entry-a',
            queryIndex: 0,
            query: 'SELECT label',
            result,
            error: '',
            executionTime: 1,
            isLoading: false,
            page: 1,
            activeTable: null,
            pkColumns: null,
        };
        // When
        render(
            <ResultEntryContent
                entry={entry}
                connectionId="later-active-conn"
                copyFormat="json"
                csvDelimiter=","
                csvIncludeHeaders
                compact={compact}
                onPageChange={vi.fn()}
            />,
        );
        // Then
        expect(state.grids[0].connectionId).toBe('conn-origin');
        expect(state.grids[0].serviceReference).toMatchObject({
            resultId: 'snapshot-a',
            resultSetIndex: 1,
            resultRowOffset: 300,
            resultGeneration: 4,
        });
        expect(state.grids[0].readonly).toBe(true);
    },
);

it('sidebar 슬롯은 원본 ordinal을 전달하며 읽기 전용 원본 텍스트를 HTML로 실행하지 않는다', () => {
    // Given
    state.contexts.length = 0;
    state.update.mockClear();
    const result = snapshotResult(envelope, {
        connectionId: 'conn-origin',
        driverId: 'postgresql',
        generation: 4,
        pageSize: 100,
    });
    const scenario = () => {
        render(
            <RowEditorPanel
                rowData={{label: '<img src=x onerror=alert(1)>'}}
                rowIndex={7}
                isInsertion={false}
                columns={[{name: 'label'}]}
                connectionId="conn-origin"
                serviceReference={result.service}
                readonly
                onChange={vi.fn()}
                onClose={vi.fn()}
            />,
        );
        fireEvent.change(screen.getByLabelText('label'), {target: {value: 'changed'}});
    };
    // When
    scenario();
    // Then
    expect(
        state.contexts
            .filter(
                (item) =>
                    item.name === 'row-editor-sidebar.header.actions' ||
                    item.name === 'row-edit-modal.footer.before',
            )
            .every(
                (item) =>
                    item.context.resultRowOrdinal === 307 &&
                    item.context.rowIndex === 307 &&
                    item.context.resultId === 'snapshot-a',
            ),
    ).toBe(true);
    expect(screen.getByLabelText('label')).toBeDisabled();
    expect(screen.getByLabelText('label')).toHaveValue('<img src=x onerror=alert(1)>');
    expect(document.querySelector('img')).toBeNull();
    expect(state.update).not.toHaveBeenCalled();
});

it('로컬 삽입 sidebar에는 snapshot ID와 원본 ordinal을 전달하지 않는다', () => {
    // Given
    state.contexts.length = 0;
    const result = snapshotResult(envelope, {
        connectionId: 'conn-origin',
        driverId: 'postgresql',
        generation: 4,
        pageSize: 100,
    });
    // When
    render(
        <RowEditorPanel
            rowData={{label: 'new'}}
            rowIndex={99}
            isInsertion
            columns={[{name: 'label'}]}
            serviceReference={result.service}
            onChange={vi.fn()}
            onClose={vi.fn()}
        />,
    );
    // Then
    expect(
        state.contexts.every(
            (item) =>
                item.context.resultId === undefined && item.context.resultRowOrdinal === undefined,
        ),
    ).toBe(true);
});
