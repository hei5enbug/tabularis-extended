import test from 'node:test';
import assert from 'node:assert/strict';
import {performance} from 'node:perf_hooks';
import {
    NativeHarnessError,
    createNativeFixture,
    explicitNativeEnvironment,
    loadNativeInputs,
    connectFixture,
    request,
    queryInput,
    querySource,
    succeeded,
    requireCondition,
    pollUntil,
} from '../support/native-process.mjs';

function requireAck(response) {
    succeeded(response);
    requireCondition(
        response.data.gui_applied === true &&
            Number.isSafeInteger(response.data.state_version) &&
            response.data.rendered_version === response.data.state_version &&
            response.data.map.version === response.data.state_version &&
            Number.isSafeInteger(response.data.generation),
        'REAL_RENDER_ACK_REQUIRED',
    );
    return response;
}

async function webviewScenario(fixture) {
    const evidence = {
        scenario: 'native_tauri_webview',
        native_execution: true,
        database_writes: 0,
        renderer_evidence: 'production_frontend_idle_ack',
        worker_instrumentation: 'not_observed',
    };
    let actual;
    try {
        const cli = fixture.start('cli');
        const connectionId = await connectFixture(fixture, cli);
        const context = {connection_id: connectionId};
        const query = succeeded(await cli.call(request('query.execute', queryInput(), context)));
        const spatial = succeeded(
            await cli.call(
                request(
                    'spatial.query_result',
                    {
                        result_id: query.result_id,
                        result_set_index: 0,
                        column_index: 2,
                        source_srid: 4326,
                        skip_invalid: false,
                    },
                    context,
                ),
            ),
        );
        const created = succeeded(
            await cli.call(request('map.create', {name: 'Native WebView fixture', basemap: null})),
        );
        const mapId = created.data.map.map_id;
        const mapContext = {map_id: mapId};
        const unavailable = await cli.call(
            request('map.open', {gui_instance_id: 'native-verification-unavailable'}, mapContext),
        );
        requireCondition(
            unavailable.error?.code === 'GUI_UNAVAILABLE' &&
                unavailable.error.details?.instances?.length === 0,
            'OWN_GUI_ISOLATION_REQUIRED',
        );
        const gui = fixture.start('gui');
        const instance = await pollUntil(
            async () => {
                requireCondition(
                    gui.child.pid && !gui.exited && !gui.failed,
                    'OWN_GUI_EXITED_BEFORE_READY',
                );
                const response = await cli.call(
                    request(
                        'map.open',
                        {gui_instance_id: 'native-verification-unavailable'},
                        {...mapContext, deadline_ms: 3000},
                    ),
                );
                requireCondition(
                    response.error?.code === 'GUI_UNAVAILABLE' &&
                        Array.isArray(response.error.details?.instances),
                    'GUI_DISCOVERY_RESPONSE_INVALID',
                );
                const own = response.error.details.instances.filter(
                    (entry) => entry.kind === 'gui' && entry.process_id === gui.child.pid,
                );
                requireCondition(
                    response.error.details.instances.length <= 1 && own.length <= 1,
                    'FOREIGN_GUI_INSTANCE_REFUSED',
                );
                return own[0] ?? null;
            },
            Math.min(fixture.deadline, performance.now() + 60000),
        );
        const guiInput = {gui_instance_id: instance.instance_id};
        const style = {
            point: {color: '#2563eb', opacity: 1, radius: 5},
            line: {color: '#2563eb', opacity: 1, width: 2},
            polygon: {color: '#2563eb', opacity: 0.5},
        };
        const added = succeeded(
            await cli.call(
                request(
                    'map.layer.add',
                    {source: querySource(query.result_id), style},
                    {...mapContext, ...context, expected_version: 0},
                ),
            ),
        );
        const layerId = added.data.map.layers[0].layer_id;
        const opened = requireAck(await cli.call(request('map.open', guiInput, mapContext)));
        const mcp = fixture.start('mcp');
        await mcp.rpc('initialize', {
            protocolVersion: '2025-06-18',
            capabilities: {},
            clientInfo: {name: 'native-map-verification', version: '1'},
        });
        const viewport = requireAck(
            await mcp.call(
                request(
                    'map.viewport.set',
                    {
                        viewport: {west: 5, east: 15, south: 15, north: 25, world: false},
                        await_gui: true,
                        ...guiInput,
                    },
                    {...mapContext, expected_version: opened.data.state_version},
                ),
            ),
        );
        const hidden = requireAck(
            await cli.call(
                request(
                    'map.layer.update',
                    {
                        layer_id: layerId,
                        visible: false,
                        await_gui: true,
                        ...guiInput,
                    },
                    {...mapContext, expected_version: viewport.data.state_version},
                ),
            ),
        );
        const visible = requireAck(
            await cli.call(
                request(
                    'map.layer.update',
                    {
                        layer_id: layerId,
                        visible: true,
                        await_gui: true,
                        ...guiInput,
                    },
                    {...mapContext, expected_version: hidden.data.state_version},
                ),
            ),
        );
        const selected = requireAck(
            await cli.call(
                request(
                    'map.selection.set',
                    {
                        feature_refs: [
                            {layer_id: layerId, feature_id: spatial.data.features[0].id},
                        ],
                        await_gui: true,
                        ...guiInput,
                    },
                    {...mapContext, expected_version: visible.data.state_version},
                ),
            ),
        );
        const conflict = await cli.call(
            request(
                'map.viewport.set',
                {
                    viewport: {west: -180, east: 180, south: -90, north: 90, world: true},
                    await_gui: true,
                    ...guiInput,
                },
                {...mapContext, expected_version: visible.data.state_version},
            ),
        );
        const afterConflict = succeeded(await cli.call(request('map.get', {}, mapContext)));
        const closed = requireAck(await cli.call(request('map.close', guiInput, mapContext)));
        const reopened = requireAck(await cli.call(request('map.open', guiInput, mapContext)));
        const closedAgain = requireAck(await cli.call(request('map.close', guiInput, mapContext)));
        const original = succeeded(
            await cli.call(
                request(
                    'result.get',
                    {result_id: query.result_id, result_set_index: 0, offset: 0, limit: 1000},
                    context,
                ),
            ),
        );
        actual = {
            query,
            spatial,
            created,
            unavailable,
            instance,
            guiPid: gui.child.pid,
            added,
            opened,
            viewport,
            hidden,
            visible,
            selected,
            conflict,
            afterConflict,
            closed,
            reopened,
            closedAgain,
            original,
            issuedQueryExecutions: fixture.requests.filter(
                (entry) => entry.operation === 'query.execute',
            ).length,
        };
        evidence.gui_instance = instance;
        evidence.gui_pid = gui.child.pid;
        evidence.render_acks = [
            opened,
            viewport,
            hidden,
            visible,
            selected,
            closed,
            reopened,
            closedAgain,
        ].map((response) => ({
            state_version: response.data.state_version,
            rendered_version: response.data.rendered_version,
            generation: response.data.generation,
            gui_applied: response.data.gui_applied,
        }));
        evidence.issued_query_executions = actual.issuedQueryExecutions;
    } catch (error) {
        evidence.failure =
            error instanceof NativeHarnessError ? error.code : 'NATIVE_WEBVIEW_FAILED';
        throw new NativeHarnessError(evidence.failure, fixture.root);
    } finally {
        const cleanup = await fixture.finish(evidence);
        if (actual) actual.cleanup = cleanup;
        console.log(
            JSON.stringify({evidence_root: cleanup.evidenceRoot, scenario: evidence.scenario}),
        );
    }
    return actual;
}

test(
    '격리된 실제 앱은 원본 결과 지도를 렌더하고 버전 충돌을 보존하며 닫은 뒤 다시 연다',
    {timeout: 10 * 60 * 1000},
    async () => {
        // given
        const inputs = loadNativeInputs(explicitNativeEnvironment());
        const fixture = createNativeFixture(inputs);
        // when
        const actual = await webviewScenario(fixture);
        // then
        assert.equal(actual.instance.kind, 'gui');
        assert.equal(actual.instance.process_id, actual.guiPid);
        assert.equal(actual.unavailable.error.code, 'GUI_UNAVAILABLE');
        assert.deepEqual(actual.unavailable.error.details.instances, []);
        assert.equal(actual.added.data.map.layers[0].connection_id, actual.query.connection_id);
        assert.equal(actual.added.data.map.layers[0].source.result_id, actual.query.result_id);
        assert.ok(
            [
                actual.opened,
                actual.viewport,
                actual.hidden,
                actual.visible,
                actual.selected,
                actual.closed,
                actual.reopened,
                actual.closedAgain,
            ].every(
                (response) =>
                    response.data.gui_applied === true &&
                    response.data.rendered_version === response.data.state_version,
            ),
        );
        assert.equal(
            actual.viewport.data.map.layers[0].generation,
            actual.added.data.map.layers[0].generation,
        );
        assert.equal(actual.hidden.data.map.layers[0].visible, false);
        assert.equal(actual.visible.data.map.layers[0].visible, true);
        assert.deepEqual(actual.selected.data.map.selected_feature_refs, [
            {
                layer_id: actual.added.data.map.layers[0].layer_id,
                feature_id: actual.spatial.data.features[0].id,
            },
        ]);
        assert.equal(actual.conflict.error.code, 'VERSION_CONFLICT');
        assert.equal(
            actual.conflict.error.details.current_version,
            actual.selected.data.state_version,
        );
        assert.deepEqual(actual.afterConflict.data.map, actual.selected.data.map);
        assert.equal(actual.afterConflict.data.generation, actual.selected.data.generation);
        assert.ok(
            actual.closed.data.generation < actual.reopened.data.generation &&
                actual.reopened.data.generation < actual.closedAgain.data.generation,
        );
        assert.equal(actual.closedAgain.data.state_version, actual.selected.data.state_version);
        assert.deepEqual(actual.original.data.rows[0], actual.query.data.rows[0]);
        assert.equal(actual.original.data.rows.length, 3);
        assert.equal(actual.issuedQueryExecutions, 1);
        assert.deepEqual(actual.cleanup.cleanupErrors, []);
    },
);
