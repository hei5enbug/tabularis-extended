import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { fail } from './errors.mjs';
import { boundedDirectoryEntries, readBoundedRegular, regularWithin, sha256 } from './files.mjs';

const TEMPLATE_SOURCE = 'https://raw.githubusercontent.com/spdx/license-list-data/main/text/MIT.txt';
const TEMPLATE_HASH = 'b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5';
const TEMPLATE_BYTES = 1078;
const TEMPLATE_NOTE = 'The copyright placeholders belong to the SPDX template. They are not upstream attribution; no year or copyright holder has been substituted.';
const MODULE_ROOT = fileURLToPath(new URL('.', import.meta.url));
const PINS = Object.freeze({
  priorityqueuejs: Object.freeze({
    name: 'priorityqueuejs', version: '2.0.0', repository: 'janogonzalez/priorityqueuejs', gitHead: '5fc8ac2ea0482277ee8110182e1d743e31cef1aa',
    author: 'Jano González <info@janogonzalez.com>', heading: 'Licence',
    packageBytes: 581, packageHash: '115a363e8b9038f9defdd1e2a06def3309f3e8ff7fd65951d38940a5a069f66e',
    readmeBytes: 1904, readmeHash: 'd8b642d2eec713715981b38b7bf0fe7686f7e182464747f959d06352d47a05da',
    provenanceBytes: 1450, provenanceHash: '83e1c7cc879a3d1057d82c616207bb9108ee6447ef377e4b98f6107c350aaa04',
  }),
  semaphore: Object.freeze({
    name: 'semaphore', version: '1.1.0', repository: 'abrkn/semaphore.js', gitHead: '88a33875b168cc7b5943d7fe987c36d08321d252',
    author: null, heading: 'License',
    packageBytes: 443, packageHash: '5e9f2a33e61b07906ddab0abd05ae0aff382954774ede7aeee30e6964ae1a37a',
    readmeBytes: 1187, readmeHash: 'be2a8a71f8055b65f5b5d2724418aef1642c5d4ba243f3206eebd915d35ba228',
    provenanceBytes: 1380, provenanceHash: '2ae22ccd64a4b66405ce7b68df02473414e66eefffc221e289bc6c4fc15b81de',
  }),
});

function expectedProvenance(pin) {
  return {
    name: pin.name, version: pin.version,
    upstream_files: {
      'package.json': { file: 'UPSTREAM-PACKAGE.json', bytes: pin.packageBytes, sha256: pin.packageHash },
      'README.md': { file: 'UPSTREAM-README.md', bytes: pin.readmeBytes, sha256: pin.readmeHash },
    },
    published_source: {
      registry_metadata_url: `https://registry.npmjs.org/${pin.name}/${pin.version}`,
      tarball_url: `https://registry.npmjs.org/${pin.name}/-/${pin.name}-${pin.version}.tgz`,
      repository_url: `https://github.com/${pin.repository}`, git_head: pin.gitHead,
    },
    license_declaration: { source: 'UPSTREAM-README.md', heading: pin.heading, text: 'MIT' },
    known_author: { source: 'UPSTREAM-PACKAGE.json', value: pin.author }, original_license_file_present: false,
    template: { file: 'MIT-TEMPLATE.txt', source: TEMPLATE_SOURCE, bytes: TEMPLATE_BYTES, sha256: TEMPLATE_HASH, is_template: true, note: TEMPLATE_NOTE },
  };
}

function closedEqual(value, expected) {
  if (expected === null || typeof expected !== 'object') return value === expected;
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const keys = Object.keys(expected);
  return Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key) && closedEqual(value[key], expected[key]));
}

function checkedFile(root, relative, bytes, hash) {
  const file = regularWithin(root, relative);
  const value = readBoundedRegular(file, bytes, 'LICENSE_MISSING');
  if (value.length !== bytes || sha256(value) !== hash) fail('LICENSE_MISSING');
  return value;
}

export function privateStageDirectory(stat, platform = process.platform) {
  return stat.isDirectory() && !stat.isSymbolicLink() && (platform === 'win32' || (stat.mode & 0o077) === 0);
}

export function copyEmbeddedLicense(stage, root, manifest, notices) {
  if (!Object.hasOwn(PINS, manifest.name)) fail('LICENSE_MISSING');
  const pin = PINS[manifest.name];
  if (manifest.version !== pin.version) fail('LICENSE_MISSING');
  const evidence = [
    { name: 'UPSTREAM-PACKAGE.json', bytes: pin.packageBytes, hash: pin.packageHash },
    { name: 'UPSTREAM-README.md', bytes: pin.readmeBytes, hash: pin.readmeHash },
    { name: 'MIT-TEMPLATE.txt', bytes: TEMPLATE_BYTES, hash: TEMPLATE_HASH },
    { name: 'provenance.json', bytes: pin.provenanceBytes, hash: pin.provenanceHash },
  ];
  const total = evidence.reduce((bytes, item) => bytes + item.bytes, 0);
  if (stage.entries.size + evidence.length > stage.limits.files || evidence.some(item => item.bytes > stage.limits.fileBytes)
    || !Number.isSafeInteger(stage.bytes + total) || stage.bytes + total > stage.limits.bytes) fail('PACKAGE_TOO_LARGE');
  let verified;
  try {
    const privateStage = fs.lstatSync(stage.root);
    if (!privateStageDirectory(privateStage)) fail('LICENSE_MISSING');
    if (boundedDirectoryEntries(root, stage.limits.files).some(name => /^(?:licen[cs]e|copying)(?:[._-].*)?$/i.test(name))) fail('LICENSE_MISSING');
    const installedPackage = checkedFile(root, 'package.json', pin.packageBytes, pin.packageHash);
    const installedReadme = checkedFile(root, 'README.md', pin.readmeBytes, pin.readmeHash);
    verified = evidence.map(item => ({ ...item, data: checkedFile(MODULE_ROOT, `licenses/${pin.name}-${pin.version}/${item.name}`, item.bytes, item.hash) }));
    if (!installedPackage.equals(verified[0].data) || !installedReadme.equals(verified[1].data)
      || !closedEqual(JSON.parse(verified[3].data.toString('utf8')), expectedProvenance(pin))) fail('LICENSE_MISSING');
  } catch { fail('LICENSE_MISSING'); }
  const hash = createHash('sha256');
  for (const item of verified) hash.update(item.name).update(Buffer.from([0])).update(item.data).update(Buffer.from([0]));
  const digest = hash.digest('hex');
  const identity = `${pin.name}@${pin.version}:${digest}`;
  if (!notices.has(identity)) {
    const prefix = `licenses/packages/${pin.name}/${pin.version}/${digest}`;
    for (const item of verified) stage.add(`${prefix}/${item.name}`, item.data);
    notices.set(identity, { name: pin.name, version: pin.version, sha256: digest, license: 'MIT', license_source: 'embedded_readme', template_source: TEMPLATE_SOURCE });
  }
}
