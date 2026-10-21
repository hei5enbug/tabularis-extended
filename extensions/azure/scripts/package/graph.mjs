import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { fail } from './errors.mjs';
import { boundedDirectoryEntries, readBoundedRegular, readJSON, regularWithin, sha256 } from './files.mjs';
import { copyEmbeddedLicense } from './embedded-licenses.mjs';

function packageName(name) {
  if (typeof name !== 'string' || !/^(?:@[a-zA-Z0-9_.-]+\/)?[a-zA-Z0-9_.-]+$/.test(name) || name.split('/').some(part => part === '.' || part === '..')) fail('INVALID_DEPENDENCY');
  return name;
}

function findNamedRoot(file, name) {
  let current = path.dirname(fs.realpathSync(file));
  while (true) {
    const manifest = path.join(current, 'package.json');
    if (fs.existsSync(manifest) && readJSON(manifest).name === name) return fs.realpathSync(current);
    const parent = path.dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

export function resolveInstalled(importer, name, optional = false) {
  packageName(name);
  let ancestor = fs.realpathSync(importer);
  let installed;
  while (true) {
    const manifest = path.join(ancestor, 'node_modules', name, 'package.json');
    if (fs.existsSync(manifest) && readJSON(manifest).name === name) {
      installed = fs.realpathSync(path.dirname(manifest));
      break;
    }
    const parent = path.dirname(ancestor);
    if (parent === ancestor) break;
    ancestor = parent;
  }
  if (!installed) {
    if (optional) return null;
    fail('DEPENDENCY_MISSING');
  }
  const require = createRequire(path.join(importer, 'package.json'));
  for (const request of [`${name}/package.json`, name]) {
    try {
      const root = findNamedRoot(require.resolve(request), name);
      if (root === installed) return root;
    } catch (error) {
      if (error?.code && !['MODULE_NOT_FOUND', 'ERR_PACKAGE_PATH_NOT_EXPORTED'].includes(error.code)) throw error;
    }
  }
  return installed;
}

const excludedDirectories = new Set(['node_modules', '.git', '.hg', '.svn', 'test', 'tests', '__tests__', 'coverage', '.nyc_output', '.cache']);
const credentialCodePins = Object.freeze({
  'dist/esm/auth/credentials.js': { bytes: 813, sha256: '4636551f56877938f92f4ae6860d12a8a72a58a6a25ec7f2278868883e7e8e6e' },
  'dist/esm/auth/credentials.js.map': { bytes: 3606, sha256: 'a042ed718b7d6a665cc4797debead0ead99e3268beb543a6890f208eb18c9815' },
  'dist/esm/auth/credentials.d.ts': { bytes: 2781, sha256: 'ea60df4c0522b71ee3ff8669949adcc258530fc21c63263d11d5f3575ba9a6eb' },
  'dist/commonjs/auth/credentials.js': { bytes: 1699, sha256: '726137b320ef6bc4ac70e029679b0c3ddea84dce428ba64c3f448544c415e59c' },
  'dist/commonjs/auth/credentials.js.map': { bytes: 3442, sha256: 'df783562632396393f518a0810e3abd326b27dac3da8dba8062c14a915085132' },
  'dist/commonjs/auth/credentials.d.ts': { bytes: 2781, sha256: 'ea60df4c0522b71ee3ff8669949adcc258530fc21c63263d11d5f3575ba9a6eb' },
});
function excluded(relative, name) {
  if (excludedDirectories.has(name)) return true;
  if (/^(?:\.env(?:\..*)?|\.npmrc|\.yarnrc(?:\..*)?|\.pnpmfile.*|.*\.(?:pem|key|pfx|p12)|credentials?(?:\..*)?|secrets?(?:\..*)?|.*\.log)$/i.test(name)) return true;
  return relative.split('/').some(part => excludedDirectories.has(part));
}

function packageExclusions(root, manifest) {
  return (relative, name) => {
    if (manifest.name === '@typespec/ts-http-runtime' && manifest.version === '0.3.9' && Object.hasOwn(credentialCodePins, relative)) {
      const pin = credentialCodePins[relative];
      try {
        const data = readBoundedRegular(regularWithin(root, relative), pin.bytes, 'INVALID_DEPENDENCY');
        if (data.length !== pin.bytes || sha256(data) !== pin.sha256) fail('INVALID_DEPENDENCY');
      } catch { fail('INVALID_DEPENDENCY'); }
      return false;
    }
    return excluded(relative, name);
  };
}

function dependencies(manifest) {
  const result = new Map();
  for (const name of Object.keys(manifest.dependencies ?? {})) result.set(packageName(name), false);
  for (const name of Object.keys(manifest.optionalDependencies ?? {})) result.set(packageName(name), true);
  for (const name of Object.keys(manifest.peerDependencies ?? {})) {
    if (!manifest.peerDependenciesMeta?.[name]?.optional && !result.has(name)) result.set(packageName(name), false);
  }
  return [...result.entries()].sort(([a], [b]) => a.localeCompare(b, 'en'));
}

function copyLicenses(stage, root, manifest, notices) {
  if (typeof manifest.version !== 'string' || !manifest.version || typeof manifest.name !== 'string') fail('INVALID_DEPENDENCY');
  const candidates = boundedDirectoryEntries(root, stage.limits.files).filter(name => /^(?:licen[cs]e|copying)(?:[._-].*)?$/i.test(name));
  if (candidates.length === 0) return copyEmbeddedLicense(stage, root, manifest, notices);
  if (stage.entries.size + candidates.length > stage.limits.files) fail('PACKAGE_TOO_LARGE');
  let total = 0;
  const licenses = candidates.map(name => {
    const file = regularWithin(root, name);
    const stat = fs.lstatSync(file);
    if (stat.size === 0) fail('LICENSE_MISSING');
    if (!Number.isSafeInteger(stat.size) || stat.size > stage.limits.fileBytes) fail('PACKAGE_TOO_LARGE');
    total += stat.size;
    if (!Number.isSafeInteger(total) || total > stage.limits.bytes - stage.bytes) fail('PACKAGE_TOO_LARGE');
    return { name, file, stat };
  });
  const identityHash = createHash('sha256');
  const buffer = Buffer.alloc(1024 * 1024);
  for (const license of licenses) {
    identityHash.update(license.name).update(Buffer.from([0]));
    const descriptor = fs.openSync(license.file, fs.constants.O_RDONLY | (fs.constants.O_NOFOLLOW ?? 0));
    try {
      const stat = fs.fstatSync(descriptor);
      if (!stat.isFile() || stat.size !== license.stat.size || stat.dev !== license.stat.dev || stat.ino !== license.stat.ino) fail('PACKAGE_TOO_LARGE');
      let bytes = 0;
      let count;
      while ((count = fs.readSync(descriptor, buffer, 0, Math.min(buffer.length, stat.size - bytes + 1), null)) !== 0) {
        bytes += count;
        if (bytes > stat.size) fail('PACKAGE_TOO_LARGE');
        identityHash.update(buffer.subarray(0, count));
      }
      if (bytes !== stat.size || fs.fstatSync(descriptor).size !== stat.size) fail('PACKAGE_TOO_LARGE');
    } finally { fs.closeSync(descriptor); }
    identityHash.update(Buffer.from([0]));
  }
  const hash = identityHash.digest('hex');
  const identity = `${manifest.name}@${manifest.version}:${hash}`;
  if (!notices.has(identity)) {
    const prefix = `licenses/packages/${manifest.name}/${manifest.version}/${hash}`;
    for (const item of licenses) stage.copy(item.file, `${prefix}/${item.name}`);
    notices.set(identity, { name: manifest.name, version: manifest.version, sha256: hash });
  }
}

export function materializeProductionGraph(stage, source) {
  const sourceRoot = fs.realpathSync(source);
  const rootManifest = readJSON(regularWithin(sourceRoot, 'package.json'));
  const nodes = new Map();
  const rootsByName = new Map();
  const topBindings = new Map();
  for (const [name, optional] of dependencies(rootManifest)) {
    const root = resolveInstalled(sourceRoot, name, optional);
    if (root) topBindings.set(name, root);
  }
  function discover(root, name, depth) {
    if (nodes.has(root)) {
      if (nodes.get(root).manifest.name !== name) fail('INVALID_DEPENDENCY');
      return;
    }
    if (depth > stage.limits.depth || nodes.size >= stage.limits.packages) fail('GRAPH_TOO_LARGE');
    const manifest = readJSON(regularWithin(root, 'package.json'));
    packageName(manifest.name);
    if (manifest.name !== name || typeof manifest.version !== 'string' || !manifest.version) fail('INVALID_DEPENDENCY');
    const node = { manifest, edges: new Map() };
    nodes.set(root, node);
    if (!rootsByName.has(name)) rootsByName.set(name, new Set());
    rootsByName.get(name).add(root);
    for (const [childName, optional] of dependencies(manifest)) {
      const child = resolveInstalled(root, childName, optional);
      if (child) node.edges.set(childName, child);
    }
    for (const [childName, child] of node.edges) discover(child, childName, depth + 1);
  }
  for (const [name, root] of topBindings) discover(root, name, 1);
  for (const [name, roots] of rootsByName) {
    if (!topBindings.has(name) && roots.size === 1) topBindings.set(name, roots.values().next().value);
  }
  const destinations = new Map();
  const copies = [];
  function reserve(destination, root) {
    if (destinations.has(destination)) {
      if (destinations.get(destination) !== root) fail('INVALID_DEPENDENCY');
      return;
    }
    if (destinations.size >= stage.limits.packages) fail('GRAPH_TOO_LARGE');
    destinations.set(destination, root);
  }
  const sortedTop = [...topBindings].sort(([a], [b]) => a.localeCompare(b, 'en'));
  for (const [name, root] of sortedTop) reserve(`node_modules/${name}`, root);
  function place(root, destination, ancestors, depth) {
    if (depth > stage.limits.depth) fail('GRAPH_TOO_LARGE');
    const localBindings = new Map();
    const children = [];
    const node = nodes.get(root);
    for (const [name, child] of node.edges) {
      const nearest = ancestors.find(bindings => bindings.has(name));
      if (nearest?.get(name) === child) continue;
      const childDestination = `${destination}/node_modules/${name}`;
      reserve(childDestination, child);
      localBindings.set(name, child);
      children.push({ root: child, destination: childDestination });
    }
    copies.push({ root, destination, manifest: node.manifest });
    const lineage = [localBindings, ...ancestors];
    for (const child of children) place(child.root, child.destination, lineage, depth + 1);
  }
  for (const [, root] of sortedTop) {
    const name = nodes.get(root).manifest.name;
    place(root, `node_modules/${name}`, [topBindings], 1);
  }
  const notices = new Map();
  const packages = [];
  for (const { root, destination, manifest } of copies) {
    copyLicenses(stage, root, manifest, notices);
    stage.copyTree(root, destination, packageExclusions(root, manifest));
    packages.push({ name: manifest.name, version: manifest.version, path: destination });
  }
  return { notices: [...notices.values()].sort((a, b) => `${a.name}@${a.version}:${a.sha256}`.localeCompare(`${b.name}@${b.version}:${b.sha256}`, 'en')), packages };
}
