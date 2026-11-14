import * as React from 'react';
import * as ReactJSXRuntime from 'react/jsx-runtime';
import * as explainApi from '@tabularis/explain';

import type {TabularisClient} from '@/api/client';
import * as pluginApi from '@/pluginApi';
import {API_VERSION} from '@/pluginApiVersion';
import type {SlotComponentProps} from '@/types/pluginSlots';

/**
 * Fork-owned extensions whose UI uses the fork host hooks (`usePluginService`,
 * `usePluginAssets`) and Web Workers, which the sandboxed guest frame does not
 * provide. Only these run in the main window; every other plugin UI stays in
 * the sandboxed frame.
 */
const IN_PROCESS_PLUGIN_IDS: ReadonlySet<string> = new Set(['spatial', 'cosmos-nosql']);

export function runsInProcess(pluginId: string): boolean {
    return IN_PROCESS_PLUGIN_IDS.has(pluginId);
}

let globalsExposed = false;

/**
 * Exposes React and the host plugin API so an in-process IIFE bundle, and the
 * workers it starts, can reach them without bundling their own copies.
 */
function exposePluginGlobals(): void {
    if (globalsExposed) return;
    globalsExposed = true;
    const host = window as unknown as Record<string, unknown>;
    host.React = React;
    host.ReactJSXRuntime = ReactJSXRuntime;
    host.__TABULARIS_API__ = pluginApi;
    host.__TABULARIS_API_VERSION__ = API_VERSION;
    host.__TABULARIS_EXPLAIN__ = explainApi;
}

/** Accepts `Component`, `{default: Component}`, or `{default: {component: Component}}`. */
export function pluginComponentFrom(raw: unknown): React.ComponentType<SlotComponentProps> | null {
    const candidate =
        typeof raw === 'function' ? raw : ((raw as {default?: unknown} | null)?.default ?? null);
    const component =
        typeof candidate === 'object' && candidate !== null && 'component' in candidate
            ? (candidate as {component: unknown}).component
            : candidate;
    return typeof component === 'function'
        ? (component as React.ComponentType<SlotComponentProps>)
        : null;
}

/**
 * Evaluates a fork-owned plugin's IIFE bundle in the main window and returns its
 * slot component, or `null` when the bundle exports none.
 */
export async function loadInProcessComponent(
    client: TabularisClient,
    pluginId: string,
    modulePath: string,
): Promise<React.ComponentType<SlotComponentProps> | null> {
    exposePluginGlobals();
    const source = await client.readPluginAsset(pluginId, modulePath);
    const evaluate = new Function(
        'React',
        'ReactJSXRuntime',
        '__TABULARIS_API__',
        source +
            "\n return typeof __tabularis_plugin__ !== 'undefined' ? __tabularis_plugin__ : null;",
    );
    return pluginComponentFrom(evaluate(React, ReactJSXRuntime, pluginApi) as unknown);
}
