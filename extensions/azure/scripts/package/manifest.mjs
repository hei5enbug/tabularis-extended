import fs from 'node:fs';
import { createHash } from 'node:crypto';
import { boundedLimits, fail } from './errors.mjs';
import { readJSON, regularWithin } from './files.mjs';
import { MIN_HOST_VERSION } from './pins.mjs';

const UI_SLOTS = new Set(['connection-modal.extra_fields', 'data-grid.toolbar.actions', 'row-edit-modal.footer.before', 'row-editor-sidebar.header.actions', 'settings.plugin.actions']);
const FALSE_CAPABILITIES = ['schemas', 'views', 'routines', 'file_based', 'alter_primary_key', 'manage_tables', 'explain', 'readonly'];
const TRUE_CAPABILITIES = ['documents_v1', 'query_page_v1', 'cancel_v1', 'metadata_discovery'];

function closed(value, keys) {
  return value !== null && typeof value === 'object' && !Array.isArray(value) && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

export function validatedManifest(source, platform) {
  const manifest = readJSON(regularWithin(source, 'manifest.json'));
  if (!closed(manifest, ['id', 'name', 'kind', 'engine', 'version', 'description', 'executable', 'connection_metadata', 'service_protocol', 'min_runtime_version', 'paradigms', 'capabilities', 'data_types', 'settings', 'ui_extensions', 'ui_assets']) || manifest.id !== 'cosmos-nosql' || manifest.name !== 'cosmos-nosql' || manifest.kind !== 'driver' || manifest.engine !== 'cosmos-nosql' || manifest.executable !== 'cosmos-nosql' || manifest.connection_metadata !== true || manifest.service_protocol !== 1 || manifest.min_runtime_version !== MIN_HOST_VERSION || typeof manifest.version !== 'string' || !manifest.version.trim() || typeof manifest.description !== 'string' || !manifest.description.trim() || /bootstrap|unavailable/i.test(manifest.description)) fail('MANIFEST_NOT_READY');
  if (!closed(manifest.capabilities, [...FALSE_CAPABILITIES, ...TRUE_CAPABILITIES, 'identifier_quote']) || !FALSE_CAPABILITIES.every(key => manifest.capabilities[key] === false) || !TRUE_CAPABILITIES.every(key => manifest.capabilities[key] === true) || manifest.capabilities.identifier_quote !== '"') fail('MANIFEST_NOT_READY');
  if (!Array.isArray(manifest.paradigms) || !manifest.paradigms.includes('document') || !manifest.paradigms.every(value => typeof value === 'string' && value.trim()) || !Array.isArray(manifest.settings) || !Array.isArray(manifest.data_types) || manifest.data_types.length !== 1 || !closed(manifest.data_types[0], ['name', 'category', 'requires_length', 'requires_precision', 'default_length']) || manifest.data_types[0].name !== 'JSON' || manifest.data_types[0].category !== 'json' || manifest.data_types[0].requires_length !== false || manifest.data_types[0].requires_precision !== false || manifest.data_types[0].default_length !== null) fail('MANIFEST_NOT_READY');
  if (!Array.isArray(manifest.ui_extensions) || manifest.ui_extensions.length !== UI_SLOTS.size || !manifest.ui_extensions.every(extension => closed(extension, extension?.slot === 'settings.plugin.actions' ? ['slot', 'module'] : ['slot', 'module', 'driver']) && UI_SLOTS.has(extension.slot) && extension.module === 'ui/dist/index.js' && (extension.slot === 'settings.plugin.actions' || extension.driver === 'cosmos-nosql')) || new Set(manifest.ui_extensions.map(extension => extension.slot)).size !== UI_SLOTS.size) fail('MANIFEST_NOT_READY');
  if (!Array.isArray(manifest.ui_assets) || manifest.ui_assets.length !== 1 || !closed(manifest.ui_assets[0], ['path', 'mime']) || manifest.ui_assets[0].path !== 'ui/dist/style.css' || manifest.ui_assets[0].mime !== 'text/css') fail('MANIFEST_NOT_READY');
  if (platform === 'win32') manifest.executable = 'cosmos-nosql.exe';
  return manifest;
}

function styleExists(source, relative) {
  try { fs.lstatSync(`${source}/${relative}`); return true; }
  catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

function openStyle(source, relative) {
  const file = regularWithin(source, relative);
  const before = fs.lstatSync(file);
  const descriptor = fs.openSync(file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
  try {
    const stat = fs.fstatSync(descriptor);
    if (!stat.isFile() || before.isSymbolicLink() || stat.dev !== before.dev || stat.ino !== before.ino) fail('INVALID_FILE');
    if (!Number.isSafeInteger(stat.size) || stat.size > boundedLimits().fileBytes) fail('PACKAGE_TOO_LARGE');
    return { file, descriptor, stat };
  } catch (error) {
    fs.closeSync(descriptor);
    throw error;
  }
}

function styleHash(input) {
  const hash = createHash('sha256');
  const buffer = Buffer.alloc(Math.min(1024 * 1024, input.stat.size + 1));
  let bytes = 0;
  let count;
  while ((count = fs.readSync(input.descriptor, buffer, 0, Math.min(buffer.length, input.stat.size - bytes + 1), null)) !== 0) {
    bytes += count;
    if (bytes > input.stat.size) fail('PACKAGE_TOO_LARGE');
    hash.update(buffer.subarray(0, count));
  }
  const current = fs.lstatSync(input.file);
  if (bytes !== input.stat.size || fs.fstatSync(input.descriptor).size !== input.stat.size) fail('PACKAGE_TOO_LARGE');
  if (current.isSymbolicLink() || current.dev !== input.stat.dev || current.ino !== input.stat.ino) fail('INVALID_FILE');
  return hash.digest('hex');
}

export function uiStyle(source) {
  const relatives = ['ui/dist/style.css', 'ui/dist/azure-ui.css'].filter(relative => styleExists(source, relative));
  if (relatives.length === 0) fail('ASSET_MISSING');
  const inputs = [];
  try {
    for (const relative of relatives) inputs.push(openStyle(source, relative));
    if (inputs.length === 2 && (inputs[0].stat.size !== inputs[1].stat.size || styleHash(inputs[0]) !== styleHash(inputs[1]))) fail('ASSET_CONFLICT');
    return inputs[0].file;
  } finally {
    for (const input of inputs) fs.closeSync(input.descriptor);
  }
}

export function requiredAsset(source, relative) {
  if (!fs.existsSync(`${source}/${relative}`)) fail('ASSET_MISSING');
  return regularWithin(source, relative);
}
