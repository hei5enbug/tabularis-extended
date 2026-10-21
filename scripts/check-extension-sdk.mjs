#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../', import.meta.url));
const extensions = ['spatial', 'azure'];
const packages = ['plugin-api', 'service-contracts'];
const generated = new Set(['node_modules', 'dist', 'target']);
const rootOnly = new Set(['scripts', 'rust', 'tests']);
const adapted = new Set(['package.json']);

const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

function list(directory, { skipRootOnly }) {
  const found = new Map();
  const visit = (current, prefix) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      if (!prefix && (generated.has(entry.name) || (skipRootOnly && rootOnly.has(entry.name)))) continue;
      const name = prefix ? `${prefix}/${entry.name}` : entry.name;
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) visit(full, name);
      else if (entry.isFile()) found.set(name, full);
    }
  };
  if (fs.existsSync(directory)) visit(directory, '');
  return found;
}

const problems = [];
const record = message => problems.push(message);

function read(file) {
  try { return fs.readFileSync(file); } catch { return null; }
}

let compared = 0;
for (const extension of extensions) {
  const snapshotRoot = path.join(repo, 'extensions', extension, 'build-support/sdk');
  const label = `extensions/${extension}/build-support/sdk`;
  const provenanceFile = path.join(snapshotRoot, 'provenance.json');
  let provenance;
  try { provenance = JSON.parse(fs.readFileSync(provenanceFile, 'utf8')); } catch { record(`${label}/provenance.json: missing or unreadable`); }
  const records = new Map((provenance?.files ?? []).map(file => [file.path, file]));

  const licenseSource = read(path.join(repo, 'LICENSE'));
  const licenseSnapshot = read(path.join(snapshotRoot, 'LICENSE'));
  compared += 1;
  if (!licenseSnapshot) record(`${label}/LICENSE: missing in snapshot`);
  else if (!licenseSource || !licenseSource.equals(licenseSnapshot)) record(`${label}/LICENSE: differs from LICENSE`);

  for (const name of packages) {
    const source = list(path.join(repo, 'packages', name), { skipRootOnly: true });
    const snapshot = list(path.join(snapshotRoot, 'packages', name), { skipRootOnly: false });
    for (const [file, sourceFile] of source) {
      const snapshotPath = `packages/${name}/${file}`;
      const target = snapshot.get(file);
      compared += 1;
      if (!target) { record(`${label}/${snapshotPath}: missing in snapshot (present in packages/${name}/${file})`); continue; }
      const sourceBytes = fs.readFileSync(sourceFile);
      if (adapted.has(file)) {
        const entry = records.get(snapshotPath);
        if (!entry || entry.origin_sha256 !== sha256(sourceBytes)) {
          record(`${label}/${snapshotPath}: origin_sha256 in provenance.json does not match packages/${name}/${file}`);
        }
      } else if (!sourceBytes.equals(fs.readFileSync(target))) {
        record(`${label}/${snapshotPath}: differs from packages/${name}/${file}`);
      }
    }
    for (const file of snapshot.keys()) {
      if (!source.has(file)) record(`${label}/packages/${name}/${file}: not present in packages/${name}`);
    }
  }

  for (const [file, entry] of records) {
    const bytes = read(path.join(snapshotRoot, file));
    if (!bytes) record(`${label}/${file}: listed in provenance.json but missing`);
    else if (bytes.length !== entry.size || sha256(bytes) !== entry.sha256) record(`${label}/${file}: size or sha256 does not match provenance.json`);
  }
}

if (problems.length) {
  process.stderr.write(`Extension SDK snapshots are out of sync with packages/ (${problems.length} problem${problems.length === 1 ? '' : 's'}):\n`);
  for (const problem of problems) process.stderr.write(`  - ${problem}\n`);
  process.stderr.write('Copy the changed packages/plugin-api and packages/service-contracts files into each snapshot and refresh provenance.json.\n');
  process.exit(1);
}
process.stdout.write(`Extension SDK snapshots match packages/ (${compared} files checked across ${extensions.length} extensions).\n`);
