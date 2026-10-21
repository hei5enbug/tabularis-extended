import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
export const SDK_REVISION = 'a10ba47979320766f0cb706afd48e6cecd7331e8';
const prefix = 'build-support/sdk';
const generated = new Set(['node_modules', 'dist']);

function fail(code) { const error = new Error(code); error.code = code; throw error; }
function relative(value) {
  if (typeof value !== 'string' || !value || value.includes('\\') || value.includes(':')
      || value.split('/').some(part => !part || part === '.' || part === '..')) fail('INVALID_SNAPSHOT');
  return value;
}
function regular(directory, name) {
  let current = directory;
  const parts = relative(name).split('/');
  for (let index = 0; index < parts.length; index += 1) {
    current = path.join(current, parts[index]);
    const stat = fs.lstatSync(current);
    if (stat.isSymbolicLink() || (index === parts.length - 1 ? !stat.isFile() : !stat.isDirectory())) fail('SOURCE_MISMATCH');
  }
  return current;
}
export function readProvenance(source = root) {
  const directory = path.join(source, prefix);
  const stat = fs.lstatSync(directory);
  if (!stat.isDirectory() || stat.isSymbolicLink()) fail('SOURCE_MISMATCH');
  const provenance = JSON.parse(fs.readFileSync(regular(directory, 'provenance.json'), 'utf8'));
  if (provenance.version !== 1 || provenance.canonical_host_commit !== SDK_REVISION
      || provenance.api_version !== '0.2.0' || provenance.service_protocol !== 1
      || !Array.isArray(provenance.files) || provenance.files.length === 0 || provenance.files.length > 256) fail('INVALID_SNAPSHOT');
  const names = new Set();
  for (const file of provenance.files) {
    const name = relative(file.path);
    if ((name !== 'LICENSE' && !name.startsWith('packages/plugin-api/') && !name.startsWith('packages/service-contracts/'))
        || names.has(name) || !/^[a-f0-9]{64}$/.test(file.sha256) || !/^[a-f0-9]{64}$/.test(file.origin_sha256)
        || !Number.isSafeInteger(file.size) || file.size < 0 || file.size > 4 * 1024 * 1024) fail('INVALID_SNAPSHOT');
    names.add(name);
    if (!['packages/plugin-api/package.json', 'packages/service-contracts/package.json'].includes(name)
        && file.sha256 !== file.origin_sha256) fail('INVALID_SNAPSHOT');
  }
  for (const required of ['LICENSE', 'packages/plugin-api/package.json', 'packages/plugin-api/LICENSE',
    'packages/service-contracts/package.json', 'packages/service-contracts/LICENSE']) {
    if (!names.has(required)) fail('INVALID_SNAPSHOT');
  }
  return provenance;
}
export function verifyFiles(directory, records) {
  try {
    const stat = fs.lstatSync(directory);
    if (!stat.isDirectory() || stat.isSymbolicLink()) fail('SOURCE_MISMATCH');
    const expected = new Set(records.map(file => file.path));
    const scan = (current, relativeDirectory = '') => {
      for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
        if (generated.has(entry.name) && ['packages/plugin-api', 'packages/service-contracts'].includes(relativeDirectory)) continue;
        const name = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name;
        if (name === 'provenance.json') continue;
        if (entry.isSymbolicLink()) fail('SOURCE_MISMATCH');
        if (entry.isDirectory()) scan(path.join(current, entry.name), name);
        else if (!entry.isFile() || !expected.has(name)) fail('SOURCE_MISMATCH');
      }
    };
    scan(directory);
    for (const file of records) {
      const filename = regular(directory, file.path);
      if (fs.statSync(filename).size !== file.size) fail('SOURCE_MISMATCH');
      const bytes = fs.readFileSync(filename);
      if (createHash('sha256').update(bytes).digest('hex') !== file.sha256) fail('SOURCE_MISMATCH');
    }
  } catch { fail('SOURCE_MISMATCH'); }
}
export function verifySdk(source = root) {
  const provenance = readProvenance(source);
  const directory = path.join(source, prefix);
  verifyFiles(directory, provenance.files);
  return { directory, provenance };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const { provenance } = verifySdk();
    process.stdout.write(`${JSON.stringify({ sdk_revision: SDK_REVISION, files: provenance.files.length })}\n`);
  } catch (error) {
    process.stderr.write(`${['SOURCE_MISMATCH', 'INVALID_SNAPSHOT'].includes(error.code) ? error.code : 'SDK_VERIFICATION_FAILED'}\n`);
    process.exitCode = 1;
  }
}
