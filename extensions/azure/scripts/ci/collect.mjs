import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { pathToFileURL } from 'node:url';

export function collectArtifacts(logFile, output) {
  if (![logFile, output].every(file => path.isAbsolute(file))) throw new Error('INVALID_PATH');
  const log = fs.readFileSync(logFile, 'utf8');
  const matches = [...log.matchAll(/Production artifact (arm64|x64): ([^\r\n]+\.zip) SHA256=([a-f0-9]{64})/g)];
  if (matches.length !== 1 || matches[0][1] !== 'x64') throw new Error('MISSING_NATIVE_ARTIFACT');
  const [, arch, bundle, expected] = matches[0];
  const bytes = fs.readFileSync(bundle);
  const sha256 = createHash('sha256').update(bytes).digest('hex');
  if (sha256 !== expected) throw new Error('ARTIFACT_HASH_MISMATCH');
  fs.mkdirSync(output, { recursive: true });
  for (const name of [path.basename(bundle), `${path.basename(bundle)}.sha256`, 'release.json', 'production-report.json']) {
    const source = name.startsWith(path.basename(bundle)) ? path.join(path.dirname(bundle), name) : path.join(path.dirname(bundle), name);
    if (!fs.lstatSync(source).isFile() || fs.lstatSync(source).isSymbolicLink()) throw new Error('INVALID_ARTIFACT');
    fs.copyFileSync(source, path.join(output, name), fs.constants.COPYFILE_EXCL);
  }
  const result = { platform: process.platform, arch, bytes: bytes.length, sha256, evidence_kind: 'native_ci_package_and_fixture_extraction', azure_executed: false, native_host_installer_executed: false };
  fs.writeFileSync(path.join(output, 'ci-report.json'), `${JSON.stringify(result, null, 2)}\n`, { flag: 'wx' });
  return result;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv.length !== 4) throw new Error('INVALID_ARGUMENT');
    process.stdout.write(`${JSON.stringify(collectArtifacts(process.argv[2], process.argv[3]))}\n`);
  } catch { process.stderr.write('CI_ARTIFACT_FAILED\n'); process.exitCode = 1; }
}
