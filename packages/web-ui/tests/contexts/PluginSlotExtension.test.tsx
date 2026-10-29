import {describe, expect, it, vi} from 'vitest';
import {render, waitFor} from '@testing-library/react';
import {useContext} from 'react';
import {invoke} from '@tauri-apps/api/core';
import {PluginSlotProvider} from '../../src/contexts/PluginSlotProvider';
import {PluginSlotContext} from '../../src/contexts/PluginSlotContext';
import {SettingsContext, DEFAULT_SETTINGS} from '../../src/contexts/SettingsContext';
import type {PluginSlotRegistryType} from '../../src/contexts/PluginSlotContext';

vi.mock('@/hooks/useTabularisClient', () => import('../support/tauriBackedHooks'));
vi.mock('i18next', () => ({
    default: {language: 'en', hasResourceBundle: () => false, addResourceBundle: vi.fn()},
}));

function Consumer({receive}: {receive(value: PluginSlotRegistryType): void}) {
    const context = useContext(PluginSlotContext);
    if (context) receive(context);
    return null;
}

describe('명시적 UI extension', () => {
    it('앱지도렌더러는활성연결없이슬롯을로드한다', async () => {
        // Given
        vi.mocked(invoke).mockReset();
        vi.mocked(invoke).mockImplementation(async (command, args) => {
            if (command === 'get_plugin_manifest')
                return {
                    id: 'spatial',
                    name: 'Spatial',
                    version: '1.0.0',
                    description: 'fixture',
                    capabilities: {},
                    ui_extensions: [{slot: 'app.map.renderer', module: 'ui/map.js'}],
                };
            if ((args as {filePath?: string}).filePath === 'ui/map.js')
                return "var __tabularis_plugin__ = { default: __TABULARIS_API__.defineSlot('app.map.renderer', function SpatialMap() { return null; }) };";
            throw new Error('No optional locale fixture');
        });
        let registry: PluginSlotRegistryType | undefined;
        const settings = {
            settings: {...DEFAULT_SETTINGS, activeExternalDrivers: ['spatial']},
            updateSetting: vi.fn(),
            isLoading: false,
        };
        const load = async () => {
            render(
                <SettingsContext.Provider value={settings}>
                    <PluginSlotProvider>
                        <Consumer
                            receive={(value) => {
                                registry = value;
                            }}
                        />
                    </PluginSlotProvider>
                </SettingsContext.Provider>,
            );
            await waitFor(() => {
                if (registry?.contributions.length !== 1)
                    throw new Error('Waiting for app map renderer');
            });
            return registry?.getSlotContributions('app.map.renderer', {});
        };
        // When
        const contributions = await load();
        // Then
        expect(contributions).toHaveLength(1);
        expect(contributions?.[0].slot).toBe('app.map.renderer');
        expect(contributions?.[0].when).toBeUndefined();
    });
    it('driverregistry조회없이디스크manifest의슬롯을로드한다', async () => {
        // Given
        vi.mocked(invoke).mockReset();
        vi.mocked(invoke).mockImplementation(async (command, args) => {
            if (command === 'get_plugin_manifest')
                return {
                    id: 'spatial',
                    name: 'Spatial',
                    version: '1.0.0',
                    description: 'fixture',
                    capabilities: {},
                    ui_extensions: [{slot: 'data-grid.toolbar.actions', module: 'ui/index.js'}],
                };
            if ((args as {filePath?: string}).filePath === 'ui/index.js')
                return "var __tabularis_plugin__ = { default: __TABULARIS_API__.defineSlot('data-grid.toolbar.actions', function Spatial() { return null; }) };";
            throw new Error('No optional locale fixture');
        });
        let registry: PluginSlotRegistryType | undefined;
        const settings = {
            settings: {...DEFAULT_SETTINGS, activeExternalDrivers: ['spatial']},
            updateSetting: vi.fn(),
            isLoading: false,
        };
        const load = async () => {
            render(
                <SettingsContext.Provider value={settings}>
                    <PluginSlotProvider>
                        <Consumer
                            receive={(value) => {
                                registry = value;
                            }}
                        />
                    </PluginSlotProvider>
                </SettingsContext.Provider>,
            );
            await waitFor(() => {
                if (registry?.contributions.length !== 1)
                    throw new Error('Waiting for extension slot');
            });
        };
        // When
        await load();
        // Then
        expect(registry?.contributions[0].pluginId).toBe('spatial');
        expect(registry?.contributions[0].slot).toBe('data-grid.toolbar.actions');
        expect(invoke).toHaveBeenCalledWith('get_plugin_manifest', {pluginId: 'spatial'});
        expect(invoke).not.toHaveBeenCalledWith('get_registered_drivers');
    });
});
