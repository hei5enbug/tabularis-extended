import { fail } from './errors.mjs';

export const HOST_FLOOR = '0.26.0';
export const MAPLIBRE_VERSION = '6.11.2';
const keys = (value, expected) => value && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === expected.length && expected.every(key => Object.hasOwn(value, key));

export function validatedManifest(value) {
  if (!keys(value, ['id', 'name', 'kind', 'version', 'description', 'service_protocol', 'min_runtime_version', 'ui_extensions', 'ui_assets']) || value.id !== 'spatial' || value.name !== 'spatial' || value.kind !== 'driver' || value.version !== '0.1.0' || value.service_protocol !== 1 || value.min_runtime_version !== HOST_FLOOR || typeof value.description !== 'string' || !value.description.trim() || /bootstrap|unavailable/i.test(value.description)) fail('MANIFEST_NOT_READY');
  if (!Array.isArray(value.ui_assets) || value.ui_assets.length !== 2 || !keys(value.ui_assets[0], ['path', 'mime']) || value.ui_assets[0].path !== 'ui/dist/style.css' || value.ui_assets[0].mime !== 'text/css' || !keys(value.ui_assets[1], ['path', 'mime']) || value.ui_assets[1].path !== 'ui/dist/maplibre-worker.js' || value.ui_assets[1].mime !== 'text/javascript') fail('MANIFEST_NOT_READY');
  if (!Array.isArray(value.ui_extensions) || value.ui_extensions.length !== 2) fail('MANIFEST_NOT_READY');
  const toolbar = value.ui_extensions.find(item => item?.slot === 'data-grid.toolbar.actions');
  const renderer = value.ui_extensions.find(item => item?.slot === 'app.map.renderer');
  if (!keys(toolbar, ['slot', 'module']) || toolbar.module !== 'ui/dist/index.js' || !keys(renderer, ['slot', 'module']) || renderer.module !== 'ui/dist/index.js') fail('MANIFEST_NOT_READY');
  return value;
}
