import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { packageBundle } from '../package/index.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
if (process.argv.length !== 3 || process.argv[2] !== 'prepare') throw new Error('Expected prepare.');
const python = execFileSync(process.platform === 'win32' ? 'python' : 'python3',
  ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' }).trim();
if (!path.isAbsolute(python) || !fs.statSync(python).isFile()) throw new Error('An absolute Python executable is required.');
const artifacts = path.join(root, 'artifacts');
fs.mkdirSync(artifacts, { recursive: true });
const archive = path.join(artifacts, 'spatial-0.1.0.zip');
const result = packageBundle({ source: root, output: archive });
fs.writeFileSync(path.join(artifacts, 'package-report.json'), `${JSON.stringify({ ...result,
  platform_tested: process.platform, arch_tested: process.arch }, null, 2)}\n`, { flag: 'wx' });
if (process.env.GITHUB_ENV) {
  fs.appendFileSync(process.env.GITHUB_ENV,
    `TABULARIS_S3_TEST_PYTHON=${python}\nTABULARIS_TEST_SPATIAL_ZIP=${archive}\n`);
}
process.stdout.write(`${JSON.stringify(result)}\n`);
