import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { SDK_REVISION, verifySdk } from './sdk.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
export const NODE_VERSION = '24.21.0';
export const PNPM_VERSION = '10.30.3';
function fail(code) { const error = new Error(code); error.code = code; throw error; }

export function commandFor(args, platform = process.platform, systemRoot) {
  if (args.some(arg => !/^[a-zA-Z0-9@/*:.=-]+$/.test(arg))) fail('INVALID_COMMAND');
  if (platform === 'win32') {
    if (typeof systemRoot !== 'string' || !path.win32.isAbsolute(systemRoot)) fail('INVALID_TOOLCHAIN');
    return { executable: path.win32.join(systemRoot, 'System32', 'cmd.exe'), args: ['/d', '/s', '/c', `corepack pnpm ${args.join(' ')}`] };
  }
  return { executable: 'corepack', args: ['pnpm', ...args] };
}
export function toolEnvironment() {
  const env = {};
  for (const key of ['PATH', 'HOME', 'USERPROFILE', 'TMPDIR', 'TEMP', 'TMP', 'SystemRoot', 'LOCALAPPDATA', 'APPDATA', 'PNPM_HOME']) {
    if (process.env[key] !== undefined) env[key] = process.env[key];
  }
  return env;
}
export function runPnpm(args, cwd, { platform = process.platform, env, capture = false } = {}) {
  const command = commandFor(args, platform, env?.SystemRoot);
  const result = spawnSync(command.executable, command.args, { cwd, env, shell: false, stdio: capture ? 'pipe' : 'inherit', encoding: 'utf8' });
  if (result.error || result.status !== 0) fail('COMMAND_FAILED');
  return result.stdout?.trim();
}
export function bootstrap({ source = root, nodeVersion = process.versions.node, run = runPnpm, env = toolEnvironment() } = {}) {
  if (nodeVersion !== NODE_VERSION) fail('NODE_VERSION_MISMATCH');
  if (run(['--version'], source, { env, capture: true }) !== PNPM_VERSION) fail('PNPM_VERSION_MISMATCH');
  const { directory } = verifySdk(source);
  const metadata = JSON.parse(fs.readFileSync(path.join(source, 'package.json'), 'utf8'));
  if (!['@tabularis/azure', '@tabularis/spatial'].includes(metadata.name)) fail('INVALID_SNAPSHOT');
  const lock = path.join(source, '.sdk-bootstrap.lock');
  let descriptor;
  try { descriptor = fs.openSync(lock, 'wx', 0o600); } catch { fail('BOOTSTRAP_LOCKED'); }
  const owner = fs.fstatSync(descriptor);
  try {
    const commands = [
      ['install', '--frozen-lockfile', '--ignore-scripts'],
      ['--filter', '@tabularis/service-contracts', 'build'],
      ['--filter', '@tabularis/plugin-api', 'build'],
      ...(metadata.name === '@tabularis/azure' ? [['build:driver']] : []),
      ['--dir', 'ui', 'build'],
    ];
    for (const args of commands) run(args, source, { env });
    verifySdk(source);
    return { sdk_revision: SDK_REVISION, sdk: directory, node: NODE_VERSION, pnpm: PNPM_VERSION };
  } finally {
    fs.closeSync(descriptor);
    try {
      const current = fs.lstatSync(lock);
      if (current.dev === owner.dev && current.ino === owner.ino) fs.unlinkSync(lock);
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv.length !== 2) fail('INVALID_ARGUMENT');
    process.stdout.write(`${JSON.stringify(bootstrap())}\n`);
  } catch (error) {
    process.stderr.write(`${['NODE_VERSION_MISMATCH', 'PNPM_VERSION_MISMATCH', 'SOURCE_MISMATCH', 'INVALID_SNAPSHOT', 'BOOTSTRAP_LOCKED', 'COMMAND_FAILED', 'INVALID_TOOLCHAIN', 'INVALID_ARGUMENT'].includes(error.code) ? error.code : 'BOOTSTRAP_FAILED'}\n`);
    process.exitCode = 1;
  }
}
