import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { createRequire } from 'node:module';
import { fail } from './errors.mjs';
import { entriesBounded, readBounded, readJSON, regularWithin, sha256 } from './files.mjs';
import { MAPLIBRE_VERSION } from './manifest.mjs';

const README_EXCEPTION = Object.freeze({
  name: 'murmurhash-js', version: '1.0.0',
  packageBytes: 635, packageHash: 'e4b3531abcc7da48f732a058112686b44af66004c38d98a8014619542e767cb4',
  readmeBytes: 1941, readmeHash: 'e137ced8967fc334ec9b5fc5c8500992f9e49d2e9cc0f6e2439e46af2f2481a4',
  sourceURL: 'https://github.com/mikolalysenko/murmurhash-js/tree/72aabce3f52cb8f16245692a69fd35951e165af0',
});

function packageName(value) {
  if (typeof value !== 'string' || !/^(?:@[A-Za-z0-9_-][A-Za-z0-9_.-]*\/)?[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(value) || value.split('/').some(part => part === '.' || part === '..')) fail('INVALID_PACKAGE');
  return value;
}

function packageVersion(value) {
  if (typeof value !== 'string' || !/^[0-9A-Za-z][0-9A-Za-z.+_-]{0,127}$/.test(value) || value.includes('..')) fail('INVALID_PACKAGE');
  return value;
}

export function resolveInstalled(importer, name, limits) {
  packageName(name);
  const resolver = createRequire(path.join(importer, 'package.json'));
  const ancestors = new Set();
  for (let current = importer;; current = path.dirname(current)) {
    ancestors.add(path.join(current, 'node_modules'));
    if (current === path.dirname(current)) break;
  }
  for (const modules of resolver.resolve.paths(name) ?? []) {
    if (!ancestors.has(modules)) continue;
    const candidate = path.join(modules, ...name.split('/'));
    let stat;
    try { stat = fs.lstatSync(candidate); } catch (error) {
      if (error.code === 'ENOENT' || error.code === 'ENOTDIR') continue;
      throw error;
    }
    if (!stat.isDirectory() && !stat.isSymbolicLink()) fail('INVALID_PACKAGE');
    const root = fs.realpathSync(candidate);
    if (!fs.statSync(root).isDirectory()) fail('INVALID_PACKAGE');
    const manifest = readJSON(root, 'package.json', limits);
    if (manifest.value.name !== name) fail('DEPENDENCY_MISMATCH');
    packageVersion(manifest.value.version);
    const { name: packageNameValue, version, dependencies: direct, optionalDependencies, peerDependencies, peerDependenciesMeta } = manifest.value;
    return {
      root, packageBytes: manifest.bytes.length, packageHash: sha256(manifest.bytes),
      value: { name: packageNameValue, version, dependencies: direct, optionalDependencies, peerDependencies, peerDependenciesMeta },
    };
  }
  return null;
}

function dependencies(manifest) {
  const result = new Map();
  for (const key of ['dependencies', 'optionalDependencies', 'peerDependencies']) {
    const values = manifest[key] ?? {};
    if (!values || typeof values !== 'object' || Array.isArray(values)) fail('INVALID_PACKAGE');
    for (const [name, range] of Object.entries(values)) {
      packageName(name);
      if (typeof range !== 'string' || !range.trim()) fail('INVALID_PACKAGE');
      const optional = key === 'optionalDependencies' || (key === 'peerDependencies' && manifest.peerDependenciesMeta?.[name]?.optional === true);
      if (!result.has(name) || !optional) result.set(name, optional);
    }
  }
  for (const name of Object.keys(manifest.optionalDependencies ?? {})) result.set(name, true);
  return [...result].sort(([a], [b]) => a.localeCompare(b, 'en'));
}

export function discoverLicenses(source, limits) {
  const ui = path.join(source, 'ui');
  const manifest = readJSON(ui, 'package.json', limits).value;
  if ((manifest.dependencies?.['maplibre-gl'] ?? manifest.devDependencies?.['maplibre-gl']) !== MAPLIBRE_VERSION) fail('MAPLIBRE_VERSION_MISMATCH');
  const initial = resolveInstalled(ui, 'maplibre-gl', limits);
  if (!initial || initial.value.version !== MAPLIBRE_VERSION) fail('MAPLIBRE_VERSION_MISMATCH');
  const visited = new Map();
  function visit(node, depth) {
    if (visited.has(node.root)) return;
    if (depth > limits.depth || visited.size >= limits.dependencyRoots) fail('PACKAGE_TOO_LARGE');
    packageName(node.value.name);
    packageVersion(node.value.version);
    visited.set(node.root, node);
    for (const [name, optional] of dependencies(node.value)) {
      const dependency = resolveInstalled(node.root, name, limits);
      if (!dependency) {
        if (!optional) fail('DEPENDENCY_MISSING');
      } else visit(dependency, depth + 1);
    }
  }
  visit(initial, 1);
  return [...visited.values()];
}

function licenseFiles(node, limits) {
  const names = entriesBounded(node.root, limits.files).filter(entry => /^(?:licen[cs]e|copying)(?:[._-].*)?$/i.test(entry.name)).map(entry => ({ source: entry.name, destination: entry.name }));
  if (names.length) return { names, source: 'license_file' };
  if (node.value.name !== README_EXCEPTION.name || node.value.version !== README_EXCEPTION.version || node.packageBytes !== README_EXCEPTION.packageBytes || node.packageHash !== README_EXCEPTION.packageHash) fail('LICENSE_MISSING');
  let readme;
  try { readme = readBounded(regularWithin(node.root, 'README.md'), Math.min(limits.licenseBytes, limits.fileBytes)); } catch { fail('LICENSE_MISSING'); }
  if (readme.length !== README_EXCEPTION.readmeBytes || sha256(readme) !== README_EXCEPTION.readmeHash) fail('LICENSE_MISSING');
  return {
    names: [{ source: 'README.md', destination: 'UPSTREAM-README.md' }, { source: 'package.json', destination: 'UPSTREAM-package.json' }],
    source: 'embedded_readme', sourceURL: README_EXCEPTION.sourceURL,
  };
}

export function copyLicenses(nodes, stage) {
  const notices = new Map();
  for (const node of nodes) {
    const selection = licenseFiles(node, stage.limits);
    const files = selection.names.map(item => {
      const file = regularWithin(node.root, item.source);
      const size = fs.lstatSync(file).size;
      if (!Number.isSafeInteger(size) || size < 1 || size > Math.min(stage.limits.licenseBytes, stage.limits.fileBytes)) fail('PACKAGE_TOO_LARGE');
      return { ...item, file, size };
    });
    const total = files.reduce((sum, item) => sum + item.size, 0);
    if (!Number.isSafeInteger(total) || total > stage.limits.bytes - stage.bytes || files.length + stage.entries.size + 2 > stage.limits.files) fail('PACKAGE_TOO_LARGE');
    const hash = crypto.createHash('sha256');
    for (const file of files) {
      const bytes = readBounded(file.file, Math.min(stage.limits.licenseBytes, stage.limits.fileBytes));
      if (bytes.length !== file.size) fail('SOURCE_CHANGED');
      file.sha256 = sha256(bytes);
      hash.update(file.destination).update('\0').update(bytes).update('\0');
    }
    const identity = hash.digest('hex');
    const key = `${node.value.name}@${node.value.version}:${identity}`;
    if (notices.has(key)) continue;
    const prefix = `licenses/packages/${node.value.name}/${node.value.version}/${identity}`;
    const copied = files.map(file => {
      const bytes = readBounded(file.file, Math.min(stage.limits.licenseBytes, stage.limits.fileBytes));
      if (bytes.length !== file.size || sha256(bytes) !== file.sha256) fail('SOURCE_CHANGED');
      stage.track(node.root, file.source, bytes, Math.min(stage.limits.licenseBytes, stage.limits.fileBytes));
      return stage.add(`${prefix}/${file.destination}`, bytes);
    });
    notices.set(key, {
      name: node.value.name, version: node.value.version, identity_sha256: identity,
      license_source: selection.source, ...(selection.sourceURL ? { source_url: selection.sourceURL } : {}), files: copied,
    });
  }
  return [...notices.values()].sort((a, b) => Buffer.compare(Buffer.from(`${a.name}/${a.version}/${a.identity_sha256}`), Buffer.from(`${b.name}/${b.version}/${b.identity_sha256}`)));
}
