import type { Plugin } from "vite";

export const MAPLIBRE_VERSION: "6.11.2";
export const MAPLIBRE_SHA256: "3f55566295583644617fe17d008a36c580414b8c71dd2e1fcff1309de6fdee5d";
export const RTL_MARKER: string;
export const RTL_REPLACEMENT: string;
export function rewriteRtlDispatcher(code: string): string;
export function transformMapLibreLifecycle(code: string, id: string, installed?: { moduleId: string; version: string }): string;
export function mapLibreLifecyclePlugin(): Plugin;
