import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

const sourceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const expectedRepositories = {
  host: ['tabularis-app-source', 'https://github.com/TabularisDB/tabularis.git'],
  postgresql: ['tabularis-postgresql-plugin', 'https://github.com/TabularisDB/tabularis-postgresql-plugin.git'],
  sqlserver: ['tabularis-sqlserver-plugin', 'https://github.com/TabularisDB/tabularis-sqlserver-plugin.git'],
};

function git(directory, args, capture = true) {
  const result = spawnSync('git', ['-C', directory, ...args], {
    encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit', timeout: 180_000,
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0' }, maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) throw new Error(`Git ${args[0]} failed; the destination was not replaced.`);
  return result.stdout?.trim() ?? '';
}

export function verifySources(root = sourceRoot) {
  const manifest = JSON.parse(fs.readFileSync(path.join(root, 'integration', 'upstreams.json'), 'utf8'));
  if (manifest.format !== 1 || !Array.isArray(manifest.repositories) || manifest.repositories.length !== 3) {
    throw new Error('Invalid source manifest.');
  }
  const seen = new Set();
  for (const entry of manifest.repositories) {
    const expected = expectedRepositories[entry.id];
    if (!expected || seen.has(entry.id) || entry.directory !== expected[0] || entry.url !== expected[1]
      || !/^[a-f0-9]{40}$/.test(entry.base_commit) || !/^[a-f0-9]{40}$/.test(entry.source_commit)
      || !/^[a-f0-9]{40}$/.test(entry.tree) || !/^[a-f0-9]{64}$/.test(entry.patch_sha256)
      || entry.patch !== `patches/${entry.id}.patch`) throw new Error('Invalid source manifest entry.');
    seen.add(entry.id);
    const patch = path.join(root, 'integration', entry.patch);
    const stat = fs.lstatSync(patch);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 32 * 1024 * 1024) throw new Error('Invalid source patch.');
    if (createHash('sha256').update(fs.readFileSync(patch)).digest('hex') !== entry.patch_sha256) {
      throw new Error(`Source patch checksum mismatch: ${entry.id}`);
    }
  }
  return manifest;
}

export function prepareSources({ root = sourceRoot, only = 'all' } = {}) {
  if (!['all', ...Object.keys(expectedRepositories)].includes(only)) throw new Error('Unknown source selection.');
  root = fs.realpathSync(root);
  if (path.basename(root) !== 'tabularis-spatial') throw new Error('Clone this repository into a folder named tabularis-spatial.');
  const manifest = verifySources(root);
  const parent = path.dirname(root);
  const selected = manifest.repositories.filter(entry => only === 'all' || entry.id === only);
  const lock = path.join(parent, '.tabularis-upstreams-bootstrap.lock');
  const lockFd = fs.openSync(lock, 'wx', 0o600);
  const lockStat = fs.fstatSync(lockFd);
  try {
  for (const entry of selected) {
    const destination = path.join(parent, entry.directory);
    if (!fs.existsSync(destination)) continue;
    const marker = path.join(destination, '.git', 'tabularis-bootstrap.json');
    if (fs.lstatSync(destination).isSymbolicLink() || !fs.existsSync(marker)) {
      throw new Error(`Existing checkout is preserved: ${destination}. Use a fresh parent directory.`);
    }
    const prior = JSON.parse(fs.readFileSync(marker, 'utf8'));
    if (prior.patch_sha256 !== entry.patch_sha256 || git(destination, ['rev-parse', 'HEAD']) !== entry.base_commit
      || git(destination, ['write-tree']) !== entry.tree) throw new Error(`Existing checkout does not match: ${destination}`);
    git(destination, ['diff', '--quiet']);
  }
  for (const entry of selected) {
    const destination = path.join(parent, entry.directory);
    if (fs.existsSync(destination)) { process.stdout.write(`Verified ${entry.directory}\n`); continue; }
    const temporary = fs.mkdtempSync(path.join(parent, '.tabularis-bootstrap-'));
    try {
      git(temporary, ['init', '--quiet']);
      git(temporary, ['config', 'core.autocrlf', 'false']);
      git(temporary, ['remote', 'add', 'origin', entry.url]);
      git(temporary, ['fetch', '--depth', '1', 'origin', entry.base_commit], false);
      git(temporary, ['checkout', '--quiet', '--detach', 'FETCH_HEAD']);
      if (git(temporary, ['rev-parse', 'HEAD']) !== entry.base_commit) throw new Error('Upstream revision mismatch.');
      const patch = path.join(root, 'integration', entry.patch);
      git(temporary, ['apply', '--check', '--index', patch]);
      git(temporary, ['apply', '--index', patch]);
      if (git(temporary, ['write-tree']) !== entry.tree) throw new Error('Patched source tree mismatch.');
      fs.writeFileSync(path.join(temporary, '.git', 'tabularis-bootstrap.json'), `${JSON.stringify(entry, null, 2)}\n`, { flag: 'wx' });
      if (fs.existsSync(destination)) throw new Error('Destination appeared during preparation.');
      fs.renameSync(temporary, destination);
      process.stdout.write(`Prepared ${entry.directory} (${entry.source_commit})\n`);
    } finally {
      if (fs.existsSync(temporary)) fs.rmSync(temporary, { recursive: true });
    }
  }
  } finally {
    fs.closeSync(lockFd);
    const current = fs.lstatSync(lock);
    if (current.dev === lockStat.dev && current.ino === lockStat.ino) fs.unlinkSync(lock);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const args = process.argv.slice(2);
    if (args.length === 1 && args[0] === '--verify') verifySources();
    else if (args.length === 0) prepareSources();
    else if (args.length === 2 && args[0] === '--only') prepareSources({ only: args[1] });
    else throw new Error('Usage: node scripts/bootstrap.mjs [--verify | --only host|postgresql|sqlserver]');
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : 'Source preparation failed.'}\n`);
    process.exitCode = 1;
  }
}
