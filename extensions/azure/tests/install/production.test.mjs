import test from 'node:test';
import assert from 'node:assert/strict';
import { productionOptions, productionSmoke } from './production-harness.mjs';
import { selectTargets } from './platform-options.mjs';

for (const { platform, arch } of selectTargets(process.platform, process.arch)) {
  const title = platform === 'darwin' && process.arch === 'arm64' && arch === 'x64' ? 'Rosetta' : `${platform} ${arch}`;
  test(`${title} 실제 production ZIP은 빈 PATH에서 원본 드라이버와 고정 Node를 실행한다`, { timeout: 300_000 }, async () => {
    // given
    const options = productionOptions(arch);
    // when
    const actual = await productionSmoke(options);
    // then
    assert.equal(actual.metadata.platform, platform);
    assert.equal(actual.metadata.arch, arch);
    assert.equal(actual.metadata.node, '24.21.0');
    assert.equal(actual.metadata.sdk, '4.10.1');
    assert.equal(actual.checksum, actual.originalHash);
    assert.equal(actual.metadata.sha256, actual.originalHash);
    assert.equal(actual.preservedHash, actual.originalHash);
    assert.equal(actual.duplicate.status, 1);
    assert.equal(actual.duplicate.stdout, '');
    assert.equal(actual.duplicate.stderr.trim(), 'OUTPUT_EXISTS');
    assert.equal(actual.rootMode, process.platform === 'win32' ? null : 0o700);
    assert.equal(actual.outputMode, process.platform === 'win32' ? null : 0o600);
    assert.equal(actual.outputRegular, true);
    assert.equal(actual.rootDirectory, true);
    assert.equal(actual.audit.symlinks, 0);
    assert.equal(actual.audit.workspace_paths, 0);
    assert.equal(actual.audit.recorded_files, actual.audit.files - 1);
    assert.ok(actual.audit.files > 100);
    assert.equal(actual.audit.sdk, '4.10.1');
    assert.equal(actual.audit.contracts, '@tabularis/service-contracts');
    assert.equal(actual.smoke.path, '');
    assert.equal(actual.smoke.isolatedProfile, true);
    assert.equal(actual.smoke.poisonCount, 8);
    assert.equal(actual.smoke.cleaned, true);
    assert.equal(actual.smoke.hostArch, process.arch);
    assert.equal(actual.smoke.version.status, 0);
    assert.equal(actual.smoke.version.stdout, platform === 'win32' ? 'v24.21.0\r\n' : 'v24.21.0\n');
    assert.equal(actual.smoke.version.stderr, '');
    assert.equal(actual.smoke.identity.status, 0);
    assert.deepEqual(actual.smoke.identity.data, { version: 'v24.21.0', arch, platform });
    assert.equal(actual.smoke.identity.stderr, '');
    assert.equal(actual.smoke.eof.code, 0);
    assert.equal(actual.smoke.eof.signal, null);
    assert.equal(actual.smoke.eof.stderr, '');
    assert.equal(actual.smoke.eof.canaryPresent, false);
    assert.equal(actual.smoke.eof.responses.length, 3);
    assert.ok(actual.smoke.eof.responses.every(response => response.jsonrpc === '2.0'));
    assert.deepEqual(Object.keys(actual.smoke.eof.byId).sort(), ['native', 'unknown', 'wrong']);
    assert.deepEqual(actual.smoke.eof.byId.native.result.service_capabilities, { protocol_version: 1, service_protocol: 1, documents_v1: true, query_page_v1: true, cancel_v1: true, sessions_v1: false });
    assert.equal(actual.smoke.eof.byId.wrong.error.data.code, 'PROTOCOL_MISMATCH');
    assert.equal(actual.smoke.eof.byId.unknown.error.code, -32601);
    assert.equal(actual.smoke.eof.byId.unknown.error.data.code, 'UNSUPPORTED_OPERATION');
    assert.equal(actual.smoke.shutdown.code, 0);
    assert.equal(actual.smoke.shutdown.signal, null);
    assert.equal(actual.smoke.shutdown.stderr, '');
    assert.equal(actual.smoke.shutdown.canaryPresent, false);
    assert.equal(actual.smoke.shutdown.responses.length, 4);
    assert.ok(actual.smoke.shutdown.responses.every(response => response.jsonrpc === '2.0'));
    assert.deepEqual(Object.keys(actual.smoke.shutdown.byId).sort(), ['native', 'shutdown', 'unknown', 'wrong']);
    assert.deepEqual(actual.smoke.shutdown.byId.native.result.service_capabilities, { protocol_version: 1, service_protocol: 1, documents_v1: true, query_page_v1: true, cancel_v1: true, sessions_v1: false });
    assert.equal(actual.smoke.shutdown.byId.wrong.error.data.code, 'PROTOCOL_MISMATCH');
    assert.equal(actual.smoke.shutdown.byId.unknown.error.data.code, 'UNSUPPORTED_OPERATION');
    assert.equal(actual.smoke.shutdown.byId.shutdown.result, null);
  });
}
