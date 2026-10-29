import {beforeEach, describe, expect, it, vi} from 'vitest';
import {renderHook} from '@testing-library/react';
import {invoke} from '@tauri-apps/api/core';
import {listen, type EventCallback} from '@tauri-apps/api/event';
import type {PluginMapApplyAck, PluginMapApplyRequest} from '../../packages/plugin-api/src/types';
import {usePluginService} from '../../src/hooks/usePluginService';

vi.mock('@tauri-apps/api/event', () => ({listen: vi.fn()}));

const request: PluginMapApplyRequest = {
    map_id: 'map-a',
    state_version: 4,
    generation: 7,
    state: {action: 'open', map: {id: 'map-a', version: 4}},
};
const ack: PluginMapApplyAck = {
    state_version: 4,
    rendered_version: 4,
    generation: 7,
    gui_applied: true,
};
let listener: EventCallback<PluginMapApplyRequest>;
let acknowledged: Promise<void>;
const unlisten = vi.fn();
const emit = () => listener({event: 'service-map-apply', id: 1, payload: request});
const ready = (value: boolean) =>
    ['service_map_renderer', {subscriptionId: expect.any(String), ready: value}] as const;
const acks = () =>
    vi.mocked(invoke).mock.calls.filter(([command]) => command === 'service_map_ack');
const publish = async () => {
    emit();
    await acknowledged;
};

describe('plugin map subscription', () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
        unlisten.mockReset();
        vi.mocked(listen).mockReset();
        vi.mocked(listen).mockImplementation(async (_event, handler) => {
            listener = handler as EventCallback<PluginMapApplyRequest>;
            return unlisten;
        });
        acknowledged = new Promise<void>((resolve) => {
            vi.mocked(invoke).mockImplementation(async (command) => {
                if (command === 'service_map_ack') resolve();
            });
        });
    });

    it('지도구독은고정host이벤트에등록한다', async () => {
        // Given
        const {result} = renderHook(usePluginService);
        // When
        const unsubscribe = await result.current.subscribeMap(async () => ack);
        // Then
        expect(listen).toHaveBeenCalledExactlyOnceWith('service-map-apply', expect.any(Function));
        expect(unsubscribe).toBeTypeOf('function');
        expect(invoke).toHaveBeenCalledExactlyOnceWith(...ready(true));
        expect(acks()).toHaveLength(0);
    });

    it('렌더콜백이완료된후동일지도버전과generation을한번만확인한다', async () => {
        // Given
        let complete: (value: PluginMapApplyAck) => void = () => undefined;
        const rendering = new Promise<PluginMapApplyAck>((resolve) => {
            complete = resolve;
        });
        const handler = vi.fn(async () => rendering);
        const {result} = renderHook(usePluginService);
        await result.current.subscribeMap(handler);
        const renderScenario = async () => {
            emit();
            await Promise.resolve();
            const earlyCalls = acks().length;
            complete(ack);
            await acknowledged;
            return earlyCalls;
        };
        // When
        const earlyCalls = await renderScenario();
        // Then
        expect(earlyCalls).toBe(0);
        expect(handler).toHaveBeenCalledExactlyOnceWith(request);
        expect(acks()).toEqual([['service_map_ack', {mapId: 'map-a', ack}]]);
    });

    it('콜백오류는원문오류없이미적용확인을전송한다', async () => {
        // Given
        const {result} = renderHook(usePluginService);
        await result.current.subscribeMap(async () => {
            throw new Error('private-render-error');
        });
        // When
        await publish();
        // Then
        expect(acks()).toEqual([
            [
                'service_map_ack',
                {
                    mapId: 'map-a',
                    ack: {
                        state_version: 4,
                        rendered_version: null,
                        generation: 7,
                        gui_applied: false,
                    },
                },
            ],
        ]);
        expect(JSON.stringify(vi.mocked(invoke).mock.calls)).not.toContain('private-render-error');
    });

    it('이전generation의콜백결과를현재지도에적용했다고확인하지않는다', async () => {
        // Given
        const {result} = renderHook(usePluginService);
        await result.current.subscribeMap(async () => ({...ack, generation: 6}));
        // When
        await publish();
        // Then
        expect(acks()).toEqual([
            [
                'service_map_ack',
                {
                    mapId: 'map-a',
                    ack: {
                        state_version: 4,
                        rendered_version: null,
                        generation: 7,
                        gui_applied: false,
                    },
                },
            ],
        ]);
    });

    it('구독해제는진행중렌더를미적용으로한번확인하고후속이벤트를무시한다', async () => {
        // Given
        let complete: (value: PluginMapApplyAck) => void = () => undefined;
        const rendering = new Promise<PluginMapApplyAck>((resolve) => {
            complete = resolve;
        });
        const handler = vi.fn(async () => rendering);
        const {result} = renderHook(usePluginService);
        const unsubscribe = await result.current.subscribeMap(handler);
        const unsubscribeScenario = async () => {
            emit();
            await Promise.resolve();
            unsubscribe();
            unsubscribe();
            await acknowledged;
            complete(ack);
            await Promise.resolve();
            await Promise.resolve();
            emit();
        };
        // When
        await unsubscribeScenario();
        // Then
        expect(unlisten).toHaveBeenCalledOnce();
        expect(handler).toHaveBeenCalledOnce();
        expect(invoke).toHaveBeenCalledWith(...ready(true));
        expect(invoke).toHaveBeenLastCalledWith(...ready(false));
        expect(invoke).toHaveBeenCalledTimes(3);
        expect(acks()).toEqual([
            [
                'service_map_ack',
                {
                    mapId: 'map-a',
                    ack: {
                        state_version: 4,
                        rendered_version: null,
                        generation: 7,
                        gui_applied: false,
                    },
                },
            ],
        ]);
    });
});
