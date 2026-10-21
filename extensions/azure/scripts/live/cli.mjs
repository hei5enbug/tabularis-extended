import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { parseArguments, readConfig, LiveError, absoluteFile } from './config.mjs';
import { readCredentialFD, releaseCredential } from './credentials.mjs';
import { runLiveScenario } from './run.mjs';
import { assertOwnerAcl } from './windows-acl.mjs';

export function reportParent(file, { platform = process.platform, ownerAclChecker = assertOwnerAcl } = {}) {
  absoluteFile(file);
  try {
    const parent = path.dirname(file);
    const stat = fs.lstatSync(parent);
    if (!stat.isDirectory() || stat.isSymbolicLink() || (platform !== 'win32' && ((stat.mode & 0o077) !== 0 || stat.uid !== process.getuid()))) throw new Error();
    ownerAclChecker(parent, { platform });
    if (fs.existsSync(file)) throw new Error();
    return parent;
  } catch { throw new LiveError('INVALID_REPORT_PATH'); }
}
export function writeReport(file, report) {
  const parent = reportParent(file);
  const temporary = path.join(parent, `.tabularis-live-${randomUUID()}.tmp`);
  let descriptor;
  try {
    descriptor = fs.openSync(temporary, fs.constants.O_CREAT | fs.constants.O_EXCL | fs.constants.O_WRONLY, 0o600);
    try { assertOwnerAcl(temporary); } catch { throw new LiveError('INVALID_REPORT_PATH'); }
    fs.writeFileSync(descriptor, `${JSON.stringify(report)}\n`); fs.fsyncSync(descriptor); fs.closeSync(descriptor); descriptor = undefined;
    reportParent(file);
    fs.linkSync(temporary, file);
  } catch (failure) {
    if (failure instanceof LiveError && failure.code === 'INVALID_REPORT_PATH') throw failure;
    throw new LiveError('REPORT_WRITE_FAILED');
  }
  finally { if (descriptor !== undefined) fs.closeSync(descriptor); fs.rmSync(temporary, { force: true }); }
}
export async function main(args, dependencies = {}) {
  let owner; let readonly; let controller; let listener;
  const output = dependencies.output ?? (value => process.stdout.write(`${JSON.stringify(value)}\n`));
  try {
    const options = parseArguments(args);
    const platform = dependencies.platform ?? process.platform;
    const ownerAclChecker = dependencies.ownerAclChecker ?? assertOwnerAcl;
    if (platform === 'win32') {
      try { ownerAclChecker(options.config, { platform }); } catch { throw new LiveError('INVALID_CONFIG'); }
    }
    if (options.report) reportParent(options.report, { platform, ownerAclChecker });
    const config = (dependencies.configReader ?? readConfig)(options.config);
    owner = (dependencies.credentialReader ?? readCredentialFD)(options.credentialFd, config);
    readonly = (dependencies.credentialReader ?? readCredentialFD)(options.readonlyCredentialFd, config);
    controller = new AbortController(); listener = () => controller.abort();
    if (!dependencies.noSignals) process.once('SIGINT', listener);
    const result = await (dependencies.scenario ?? runLiveScenario)(config, { owner, readonly }, { driver: options.driver, signal: controller.signal });
    if (options.report) writeReport(options.report, result.report); else output(result.report);
    return result.exitCode;
  } catch (failure) {
    const safe = failure instanceof LiveError ? failure.code : 'HARNESS_FAILURE';
    output({ version: 1, status: 'failed', code: safe, evidence_kind: 'no_live_execution' });
    return controller?.signal.aborted ? 130 : 2;
  } finally {
    if (listener) process.removeListener('SIGINT', listener);
    releaseCredential(owner); releaseCredential(readonly); owner = undefined; readonly = undefined;
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exitCode = await main(process.argv.slice(2));
