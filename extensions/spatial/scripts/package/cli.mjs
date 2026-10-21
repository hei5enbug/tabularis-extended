import { pathToFileURL } from 'node:url';
import { packageBundle } from './index.mjs';
import { absolute } from './files.mjs';
import { fail, publicError } from './errors.mjs';

export function parseArguments(args) {
  if (!Array.isArray(args) || args.length !== 4) fail('INVALID_ARGUMENT');
  const parsed = {};
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    if (!['--source', '--output'].includes(flag) || Object.hasOwn(parsed, flag.slice(2))) fail('INVALID_ARGUMENT');
    parsed[flag.slice(2)] = absolute(args[index + 1]);
  }
  if (!parsed.source || !parsed.output) fail('INVALID_ARGUMENT');
  return parsed;
}

export function runCLI(args) {
  try { return packageBundle(parseArguments(args)); } catch (error) { return publicError(error); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const result = runCLI(process.argv.slice(2));
  process.stdout.write(`${JSON.stringify(result)}\n`);
  process.exitCode = result.ok ? 0 : 1;
}
