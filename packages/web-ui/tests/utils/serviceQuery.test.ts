import {describe, expect, it, vi} from 'vitest';
import type {ServiceResponse} from '@tabularis/service-contracts';
import {
    captureQueryCapabilities,
    isCurrentSnapshot,
    queryUsesSnapshot,
    readAllSnapshotRows,
    readSnapshotPage,
    snapshotResult,
    snapshotSlotContext,
    supportsServiceQuery,
    type QueryService,
} from '../../src/utils/serviceQuery';
import {reconstructTableQuery} from '../../src/utils/editor';
import {extractQueryParams, interpolateQueryParams} from '../../src/utils/queryParameters';
import type {DriverCapabilities} from '../../src/types/plugins';
import type {Tab} from '../../src/types/editor';

const response = (
    offset = 0,
    rows: unknown[][] = [[1, null]],
    hasMore = false,
): ServiceResponse => ({
    protocol_version: 1,
    request_id: 'request',
    connection_id: 'conn-a',
    status: 'succeeded',
    job_id: null,
    result_id: 'result-a',
    data: {
        kind: 'tabular',
        columns: [
            {name: 'same', column_index: 0, native_type: null},
            {name: 'same', column_index: 1, native_type: 'geometry', srid: null},
        ],
        rows: rows as ServiceResponse['data'],
        offset,
        result_revision: 'revision-a',
        result_set_index: 0,
        result_set_count: 1,
        affected_rows: null,
    },
    page: {
        has_more: hasMore,
        next_token: hasMore ? 'opaque-token' : null,
        resume_mode: 'materialized',
    },
    limits: {truncated: false, reasons: []},
    metrics: {elapsed_ms: 0, request_charge: null, retry_count: 0},
    warnings: [],
    error: null,
});

it('명시적 페이지 이동만 저장된 결과를 읽고 새 실행은 refresh와 필터 변경에도 새 쿼리를 허용한다', () => {
    // Given
    const result = snapshotResult(response(), context);
    const scenario = () => ({
        page: queryUsesSnapshot('page', result),
        refresh: queryUsesSnapshot('run', result),
        filter: queryUsesSnapshot('run', result),
        missing: queryUsesSnapshot('page', null),
    });
    // When
    const routes = scenario();
    // Then
    expect(routes).toEqual({page: true, refresh: false, filter: false, missing: false});
});

it('새 generation과 다른 원본 참조에는 이전 페이지 응답을 적용하지 않는다', () => {
    // Given
    const ref = snapshotResult(response(), context).service!;
    const scenario = () => [
        isCurrentSnapshot(ref, ref, 'conn-a'),
        isCurrentSnapshot(ref, {...ref, resultGeneration: ref.resultGeneration + 1}, 'conn-a'),
        isCurrentSnapshot(ref, {...ref, resultId: 'new-result'}, 'conn-a'),
        isCurrentSnapshot(ref, ref, 'conn-b'),
        isCurrentSnapshot(ref, undefined, 'conn-a'),
    ];
    // When
    const current = scenario();
    // Then
    expect(current).toEqual([true, false, false, false, false]);
});

it('native 다음 페이지는 Cosmos 작업 공간 안내로 거부하고 query.next를 호출하지 않는다', async () => {
    // Given
    const envelope = response(0, [[1, null]], true);
    envelope.page!.resume_mode = 'native';
    const result = snapshotResult(envelope, context);
    const api = service();
    // When
    const error = await readSnapshotPage(api, result, 2).catch((error: unknown) => error);
    // Then
    expect(error).toEqual(new Error('Use the Cosmos workspace to continue this query.'));
    expect(api.call).not.toHaveBeenCalled();
});
const context = {connectionId: 'conn-a', driverId: 'postgresql', generation: 7, pageSize: 100};
const service = (
    call = vi.fn<QueryService['call']>().mockResolvedValue(response()),
): QueryService => ({
    call,
    executeWrite: vi.fn(),
    capabilities: vi.fn().mockResolvedValue({service_protocol: 1, operations: ['query.execute']}),
});

describe('서비스 스냅샷', () => {
    it('중복 열 이름과 공간 NULL 메타데이터 및 미확인 영향 행 수를 보존한다', () => {
        // Given
        const envelope = response();
        // When
        const result = snapshotResult(envelope, context);
        // Then
        expect(result.columns).toEqual(['same', 'same']);
        expect(result.rows).toEqual([[1, null]]);
        expect(result.affected_rows).toBeNull();
        expect(result.service).toMatchObject({
            resultId: 'result-a',
            resultGeneration: 7,
            columnMetadata: [{native_type: null}, {native_type: 'geometry', srid: null}],
        });
    });

    it('짧은 페이지 다음 위치는 실제 행 수로 계산하고 SQL을 다시 실행하지 않는다', async () => {
        // Given
        const first = snapshotResult(
            response(
                0,
                [
                    [1, null],
                    [2, null],
                ],
                true,
            ),
            context,
        );
        const api = service();
        vi.mocked(api.call).mockResolvedValue(response(2, [[3, null]], true));
        // When
        const next = await readSnapshotPage(api, first, 2);
        // Then
        expect(api.call).toHaveBeenCalledExactlyOnceWith(
            expect.objectContaining({
                operation: 'result.get',
                connection_id: 'conn-a',
                input: {result_id: 'result-a', result_set_index: 0, offset: 2, limit: 100},
            }),
            {signal: undefined},
        );
        expect(next.service?.pageOffsets).toEqual([0, 2, 3]);
        expect(next.service?.resultRowOffset).toBe(2);
    });

    it('페이지 크기 변경은 원래 스냅샷의 offset 0부터 읽는다', async () => {
        // Given
        const first = snapshotResult(response(400), context);
        const api = service();
        // When
        const next = await readSnapshotPage(api, first, 1, 50);
        // Then
        expect(api.call).toHaveBeenCalledWith(
            expect.objectContaining({
                operation: 'result.get',
                input: {result_id: 'result-a', result_set_index: 0, offset: 0, limit: 50},
            }),
            {signal: undefined},
        );
        expect(next.service?.pageOffsets).toEqual([0, 1]);
    });

    it('모르는 페이지로 건너뛰면 조회나 SQL 실행 없이 거부한다', async () => {
        // Given
        const first = snapshotResult(response(0, [[1, null]], true), context);
        const api = service();
        // When
        const error = await readSnapshotPage(api, first, 10).catch((error: unknown) => error);
        // Then
        expect(error).toEqual(new Error('Read the next snapshot page before jumping ahead.'));
        expect(api.call).not.toHaveBeenCalled();
    });

    it('다른 revision 응답을 원래 스냅샷에 합치지 않는다', async () => {
        // Given
        const first = snapshotResult(response(0, [[1, null]], true), context);
        const api = service();
        const changed = response(1);
        changed.data = {...(changed.data as object), result_revision: 'revision-b'};
        vi.mocked(api.call).mockResolvedValue(changed);
        // When
        const error = await readSnapshotPage(api, first, 2).catch((error: unknown) => error);
        // Then
        expect(error).toEqual(new Error('The immutable result reference changed.'));
        expect(api.call).toHaveBeenCalledTimes(1);
    });

    it('JSON 값과 문서는 원본 구조 및 검증된 origin을 유지한다', () => {
        // Given
        const envelope = response();
        envelope.data = {
            kind: 'documents',
            documents: [
                {
                    document: {id: 'x', nested: [null, true]},
                    identity: {id: 'x', partition_key: [{type: 'null'}]},
                    etag: 'tag',
                },
            ],
            result_set_index: 0,
            result_set_count: 1,
            offset: 0,
            result_revision: 'revision-a',
            origin: {database: 'db', container: 'items'},
        };
        // When
        const result = snapshotResult(envelope, context);
        // Then
        expect(result.service?.kind).toBe('documents');
        expect(result.service?.origin).toEqual({database: 'db', container: 'items'});
        expect(result.rows[0][0]).toEqual({
            document: {id: 'x', nested: [null, true]},
            identity: {id: 'x', partition_key: [{type: 'null'}]},
            etag: 'tag',
        });
    });

    it('원본 행 순서에 offset을 더하고 로컬 삽입에는 참조를 주지 않는다', () => {
        // Given
        const ref = snapshotResult(response(200), context).service;
        const scenario = () => [snapshotSlotContext(ref, 9), snapshotSlotContext(ref, 99, true)];
        // When
        const result = scenario();
        // Then
        expect(result[0]).toMatchObject({
            resultId: 'result-a',
            resultRowOrdinal: 209,
            rowIndex: 209,
            resultGeneration: 7,
            connectionId: 'conn-a',
        });
        expect(result[1]).toEqual({});
    });

    it('copy-all은 저장된 페이지를 읽고 10000행 상한을 명확히 표시한다', async () => {
        // Given
        const api = service(
            vi.fn(async (request) => {
                const input = request.input as {offset: number};
                return response(
                    input.offset,
                    Array.from({length: 1000}, (_, index) => [input.offset + index, null]),
                    true,
                );
            }),
        );
        const first = snapshotResult(response(), context);
        // When
        const all = await readAllSnapshotRows(api, first);
        // Then
        expect(all.rows).toHaveLength(10000);
        expect(all.truncated).toBe(true);
        expect(all.service?.limitReasons).toContain('COPY_LIMIT');
        expect(
            vi.mocked(api.call).mock.calls.every(([request]) => request.operation === 'result.get'),
        ).toBe(true);
    });

    it('인증 오류는 legacy 경로 사용 여부로 바꾸지 않는다', async () => {
        // Given
        const api = service();
        vi.mocked(api.capabilities).mockRejectedValue(new Error('AUTH_REQUIRED'));
        // When
        const error = await supportsServiceQuery(api, 'conn-a').catch((error: unknown) => error);
        // Then
        expect(error).toEqual(new Error('AUTH_REQUIRED'));
        expect(api.call).not.toHaveBeenCalled();
    });

    it('알려진 빈 operation 목록만 미지원으로 반환한다', async () => {
        // Given
        const api = service();
        vi.mocked(api.capabilities).mockResolvedValue({service_protocol: 1, operations: []});
        // When
        const supported = await supportsServiceQuery(api, 'conn-a');
        // Then
        expect(supported).toBe(false);
        expect(api.capabilities).toHaveBeenCalledExactlyOnceWith('conn-a');
    });
});

it('복합 PK는 컬럼 표시 순서와 달라도 실제 PK 위치 순서로 정렬한다', async () => {
    // Given
    const {orderedServicePrimaryKey} = await import('../../src/utils/serviceQuery');
    const columns = [
        {name: 'second', is_pk: true, primary_key_position: 2},
        {name: 'first', is_pk: true, primary_key_position: 1},
    ];
    // When
    const keys = orderedServicePrimaryKey(columns);
    // Then
    expect(keys).toEqual(['first', 'second']);
});

it('복합 PK 위치가 없거나 중복이면 쓰기 identity를 추측하지 않는다', async () => {
    // Given
    const {orderedServicePrimaryKey} = await import('../../src/utils/serviceQuery');
    const scenario = () => [
        orderedServicePrimaryKey([
            {name: 'a', is_pk: true},
            {name: 'b', is_pk: true},
        ]),
        orderedServicePrimaryKey([
            {name: 'a', is_pk: true, primary_key_position: 1},
            {name: 'b', is_pk: true, primary_key_position: 1},
        ]),
    ];
    // When
    const result = scenario();
    // Then
    expect(result).toEqual([null, null]);
});

it('다른 연결이 활성 상태여도 실행 대상 driver의 schema 인용과 파라미터 문법을 사용한다', () => {
    // Given
    const postgres: DriverCapabilities = {
        schemas: true,
        views: true,
        routines: true,
        file_based: false,
        folder_based: false,
        identifier_quote: '"',
        alter_primary_key: true,
        sql_dialect: 'postgres',
    };
    const mysql: DriverCapabilities = {
        ...postgres,
        schemas: false,
        identifier_quote: '`',
        sql_dialect: 'mysql',
    };
    const drivers = [
        {id: 'postgresql', capabilities: postgres},
        {id: 'mysql', capabilities: mysql},
    ];
    const target: Tab = {
        id: 'tab-a',
        connectionId: 'conn-a',
        type: 'table',
        title: 'orders',
        activeTable: 'orders',
        schema: 'public',
        query: '',
        result: null,
        error: '',
        executionTime: null,
        page: 1,
        pkColumns: null,
    };
    const scenario = () => {
        const captured = captureQueryCapabilities(drivers, 'postgresql', 'conn-a', 'conn-b', mysql);
        return {
            query: reconstructTableQuery(target, captured, {
                filterOverride: 'id = :actual',
                sortOverride: 'id',
            }),
            params: extractQueryParams('SELECT $$:hidden$$, :actual', captured.sql_dialect),
            text: interpolateQueryParams(
                'SELECT $$:hidden$$, :actual',
                {actual: '9'},
                captured.sql_dialect,
            ),
        };
    };
    // When
    const result = scenario();
    // Then
    expect(result).toEqual({
        query: 'SELECT * FROM "public"."orders" WHERE id = :actual ORDER BY id',
        params: ['actual'],
        text: 'SELECT $$:hidden$$, 9',
    });
});

it('연결별 capability override는 같은 연결일 때만 사용한다', () => {
    // Given
    const manifest: DriverCapabilities = {
        schemas: true,
        views: true,
        routines: false,
        file_based: false,
        folder_based: false,
        identifier_quote: '"',
        alter_primary_key: false,
        sql_dialect: 'postgres',
    };
    const override: DriverCapabilities = {...manifest, schemas: false};
    // When
    const captured = captureQueryCapabilities(
        [{id: 'postgresql', capabilities: manifest}],
        'postgresql',
        'conn-a',
        'conn-a',
        override,
    );
    // Then
    expect(captured).toBe(override);
});

it('저장된 driver를 찾지 못하면 활성 연결의 문법을 빌리지 않고 명시적으로 실패한다', () => {
    // Given
    const scenario = () => captureQueryCapabilities([], 'unavailable', 'conn-a', 'conn-b', null);
    // When
    const error = (() => {
        try {
            scenario();
        } catch (error) {
            return error;
        }
    })();
    // Then
    expect(error).toEqual(new Error('The saved connection driver is unavailable.'));
});
