import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {
    explicitNativeEnvironment,
    loadNativeInputs,
    NativeHarnessError,
} from '../tests/service/support/native-process.mjs';

async function main() {
    const args = process.argv.slice(2);
    if (args.length !== 1 || !['parity', 'webview'].includes(args[0]))
        throw new NativeHarnessError('USAGE: parity|webview');
    const inputs = loadNativeInputs(explicitNativeEnvironment());
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tabularis-service-native-'));
    fs.chmodSync(root, 0o700);
    const output = fs.openSync(path.join(root, 'runner.log'), 'wx', 0o600);
    const filename = fileURLToPath(
        new URL(`../tests/service/native/${args[0]}.native.mjs`, import.meta.url),
    );
    const child = spawn(process.execPath, ['--test', '--test-concurrency=1', filename], {
        cwd: root,
        env: {...inputs.environment, PATH: '/usr/bin:/bin:/usr/sbin:/sbin', TMPDIR: root},
        shell: false,
        stdio: ['ignore', 'pipe', 'pipe'],
        detached: process.platform !== 'win32',
    });
    let total = 0;
    let limit = false;
    let escalation;
    const terminate = () => {
        try {
            if (process.platform === 'win32') child.kill('SIGTERM');
            else if (child.pid) process.kill(-child.pid, 'SIGTERM');
        } catch (error) {
            if (error.code !== 'ESRCH') limit = true;
        }
        escalation ??= setTimeout(() => {
            try {
                if (process.platform === 'win32') child.kill('SIGKILL');
                else if (child.pid) process.kill(-child.pid, 'SIGKILL');
            } catch (error) {
                if (error.code !== 'ESRCH') limit = true;
            }
        }, 5000);
    };
    const capture = (bytes) => {
        total += bytes.length;
        if (total > 1024 * 1024) {
            limit = true;
            terminate();
            return;
        }
        fs.writeSync(output, bytes);
        process.stdout.write(bytes);
    };
    child.stdout.on('data', capture);
    child.stderr.on('data', capture);
    const watchdog = setTimeout(terminate, 10 * 60 * 1000 + 10000);
    const interrupt = () => terminate();
    process.once('SIGINT', interrupt);
    process.once('SIGTERM', interrupt);
    try {
        const code = await new Promise((resolve, reject) => {
            child.once('error', () => reject(new NativeHarnessError('NATIVE_TEST_RUNNER_FAILED')));
            child.once('close', (status, signal) => resolve(signal || limit ? 1 : (status ?? 1)));
        });
        fs.fsyncSync(output);
        console.log(JSON.stringify({mode: args[0], runner_evidence: root, exit_code: code}));
        process.exitCode = code;
    } finally {
        clearTimeout(watchdog);
        clearTimeout(escalation);
        process.removeListener('SIGINT', interrupt);
        process.removeListener('SIGTERM', interrupt);
        fs.closeSync(output);
    }
}

main().catch((error) => {
    console.error(
        JSON.stringify({
            error:
                error instanceof NativeHarnessError ? error.code : 'NATIVE_HARNESS_INPUT_INVALID',
        }),
    );
    process.exitCode = 1;
});
