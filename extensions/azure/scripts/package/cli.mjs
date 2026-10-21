import { pathToFileURL } from 'node:url';
import { packageBundle } from './index.mjs';
import { fail, publicError } from './errors.mjs';

export function parseArguments(args) {
  const allowed = new Map([['--platform', 'platform'], ['--arch', 'arch'], ['--source', 'source'], ['--launcher', 'launcher'], ['--runtime-archive', 'runtimeArchive'], ['--output', 'output'], ['--tar', 'tar']]);
  const options = {};
  for (let index = 0; index < args.length; index += 2) {
    const name = allowed.get(args[index]);
    const value = args[index + 1];
    if (!name || name in options || typeof value !== 'string' || value.startsWith('--')) fail('INVALID_ARGUMENT');
    options[name] = value;
  }
  if (['platform', 'arch', 'source', 'launcher', 'runtimeArchive', 'output'].some(name => !(name in options))) fail('INVALID_ARGUMENT');
  return options;
}

export function main(args = process.argv.slice(2)) {
  try {
    const result = packageBundle(parseArguments(args));
    process.stdout.write(`${JSON.stringify(result)}\n`);
    return 0;
  } catch (error) {
    process.stderr.write(`${publicError(error)}\n`);
    return 1;
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
