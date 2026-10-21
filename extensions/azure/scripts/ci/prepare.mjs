import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { prepareArchives } from './runtime.mjs';
import { NODE_PINS } from '../package/pins.mjs';

export function prepareInputs({ platform = process.platform, arch = process.arch, launcher, python, cache, envFile, systemRoot }) {
  if (!Object.hasOwn(NODE_PINS, `${platform}-${arch}`) || arch !== 'x64') throw new Error('CAPABILITY_UNAVAILABLE');
  const paths = platform === 'win32' ? path.win32 : path.posix;
  for (const file of [launcher, python, cache, envFile]) if (typeof file !== 'string' || !paths.isAbsolute(file) || /[\r\n\0]/.test(file)) throw new Error('INVALID_PATH');
  if (platform === 'win32' && (typeof systemRoot !== 'string' || !paths.isAbsolute(systemRoot))) throw new Error('INVALID_PATH');
  return { platform, arch, launcher, python, cache, envFile, systemRoot };
}
export async function prepareCI(input, { archives = prepareArchives, execute = spawnSync } = {}) {
  const options = prepareInputs(input);
  options.python = fs.realpathSync(options.python);
  for (const file of [options.launcher, options.python]) if (!fs.lstatSync(file).isFile() || fs.lstatSync(file).isSymbolicLink()) throw new Error('INVALID_TOOLCHAIN');
  const check = execute(options.python, ['-c', 'import sys;print(sys.executable)'], { shell: false, encoding: 'utf8', timeout: 15_000 });
  if (check.status !== 0 || !path.isAbsolute(check.stdout?.trim() ?? '')) throw new Error('INVALID_TOOLCHAIN');
  const files = await archives(options.cache);
  const values = {
    TABULARIS_C3B_TEST_LAUNCHER: options.launcher,
    TABULARIS_C3B_TEST_NODE_ARCHIVE: files[`${options.platform}-${options.arch}`],
    TABULARIS_C3B_TEST_PYTHON: options.python,
    TABULARIS_C3B_TEST_NODE_CACHE: options.cache,
  };
  fs.appendFileSync(options.envFile, `${Object.entries(values).map(([key, value]) => `${key}=${value}`).join('\n')}\n`);
  return values;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  try {
    if (args.length !== 8 || args.some((value, i) => i % 2 === 0 && value !== ['--launcher', '--python', '--cache', '--env-file'][i / 2])) throw new Error('INVALID_ARGUMENT');
    const result = await prepareCI({ launcher: args[1], python: args[3], cache: args[5], envFile: args[7], systemRoot: process.platform === 'win32' ? process.env.SystemRoot : undefined });
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch { process.stderr.write('CI_INPUT_FAILED\n'); process.exitCode = 1; }
}
