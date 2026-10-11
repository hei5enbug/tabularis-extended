import { spawn } from "node:child_process";
import { chmod, mkdir, mkdtemp, realpath, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const temporaryParent = process.platform === "darwin" ? "/private/tmp" : await realpath(os.tmpdir());
const temporary = await mkdtemp(path.join(temporaryParent, "tabularis-extended-rust-tests-"));
await chmod(temporary, 0o700);
const profile = path.join(temporary, "tabularis-extended");
const plugins = path.join(temporary, "plugins");
await mkdir(profile);
await mkdir(plugins);
try {
  process.exitCode = await new Promise((resolve, reject) => {
    const child = spawn("cargo", ["test", "--manifest-path", path.join(root, "src-tauri/Cargo.toml"), "--locked", "--lib", ...process.argv.slice(2)], {
      cwd: root, stdio: "inherit", env: { ...process.env, TMPDIR: temporaryParent,
        TABULARIS_EXTENDED_DATA_DIR: profile, TABULARIS_EXTENDED_PLUGIN_DIR: plugins },
    });
    child.once("error", reject);
    child.once("exit", code => resolve(code ?? 1));
  });
} finally {
  await rm(temporary, { recursive: true, force: true });
}
