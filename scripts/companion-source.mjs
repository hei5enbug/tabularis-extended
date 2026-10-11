import { execFileSync } from "node:child_process";
import { constants } from "node:fs";
import { lstat, open, readlink, realpath, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_GIT_OUTPUT_BYTES = 16 * 1024 * 1024;
const MAX_FILE_COUNT = 10_000;
const MAX_FILE_BYTES = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES = 128 * 1024 * 1024;
const READ_CHUNK_BYTES = 64 * 1024;
const UTF8 = new TextDecoder("utf-8", { fatal: true });

function fail(message) {
  throw new Error(message);
}

function runGit(root, args) {
  try {
    return execFileSync("git", args, {
      cwd: root,
      encoding: "buffer",
      maxBuffer: MAX_GIT_OUTPUT_BYTES,
      stdio: ["ignore", "pipe", "ignore"],
      windowsHide: true,
    });
  } catch {
    fail("Git metadata could not be read.");
  }
}

function decodeGitPathList(output) {
  if (output.length === 0) return [];
  if (output[output.length - 1] !== 0) fail("Git returned an incomplete file list.");

  const paths = [];
  let start = 0;
  for (let offset = 0; offset < output.length; offset += 1) {
    if (output[offset] !== 0) continue;
    if (offset === start) fail("Git returned an empty file path.");
    try {
      paths.push(UTF8.decode(output.subarray(start, offset)));
    } catch {
      fail("Git returned a file path that is not valid UTF-8.");
    }
    start = offset + 1;
  }
  if (paths.length > MAX_FILE_COUNT) fail("Source contains too many files to fingerprint.");
  return paths;
}

function validateRelativePath(relativePath) {
  if (
    relativePath.length === 0 ||
    relativePath.includes("\\") ||
    path.posix.isAbsolute(relativePath) ||
    path.win32.isAbsolute(relativePath)
  ) {
    fail("Source contains an invalid relative file path.");
  }

  const segments = relativePath.split("/");
  if (
    segments.some(segment => segment.length === 0 || segment === "." || segment === "..") ||
    path.posix.normalize(relativePath) !== relativePath
  ) {
    fail("Source contains a path that escapes or ambiguously resolves within the worktree.");
  }
  return segments;
}

function assertInsideRoot(root, candidate) {
  const relative = path.relative(root, candidate);
  if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
    fail("A source path resolves outside the worktree.");
  }
}

async function resolveRegularFile(root, relativePath) {
  const segments = validateRelativePath(relativePath);
  const fullPath = path.resolve(root, ...segments);
  assertInsideRoot(root, fullPath);

  let current = root;
  let leafStat;
  for (const [index, segment] of segments.entries()) {
    current = path.join(current, segment);
    try {
      leafStat = await lstat(current);
    } catch {
      fail("A tracked or unignored source file is missing.");
    }
    if (leafStat.isSymbolicLink()) fail("Symbolic links are not allowed in fingerprinted source.");
    if (index < segments.length - 1 && !leafStat.isDirectory()) {
      fail("A source path parent is not a directory.");
    }
  }

  if (!leafStat?.isFile()) fail("Only regular source files can be fingerprinted.");
  if (leafStat.size > MAX_FILE_BYTES) fail("A source file exceeds the per-file fingerprint size limit.");

  let physicalPath;
  try {
    physicalPath = await realpath(fullPath);
  } catch {
    fail("A source file could not be resolved.");
  }
  assertInsideRoot(root, physicalPath);
  return { fullPath, physicalPath, leafStat };
}

function lengthPrefix(length) {
  const prefix = Buffer.alloc(8);
  prefix.writeBigUInt64BE(BigInt(length));
  return prefix;
}

async function readBoundedFile(root, relativePath, totalBytesRead) {
  const { fullPath, physicalPath, leafStat } = await resolveRegularFile(root, relativePath);
  const noFollow = constants.O_NOFOLLOW ?? 0;
  let handle;
  try {
    handle = await open(fullPath, constants.O_RDONLY | noFollow);
    const before = await handle.stat();
    if (!before.isFile() || before.dev !== leafStat.dev || before.ino !== leafStat.ino) {
      fail("A source file changed while it was being opened.");
    }
    const physicalStat = await stat(physicalPath);
    if (physicalStat.dev !== before.dev || physicalStat.ino !== before.ino) {
      fail("A source path changed while it was being opened.");
    }
    if (before.size > MAX_FILE_BYTES) fail("A source file exceeds the per-file fingerprint size limit.");

    const chunks = [];
    let fileBytesRead = 0;
    const buffer = Buffer.allocUnsafe(READ_CHUNK_BYTES);
    while (true) {
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      fileBytesRead += bytesRead;
      if (fileBytesRead > MAX_FILE_BYTES) fail("A source file exceeds the per-file fingerprint size limit.");
      if (totalBytesRead + fileBytesRead > MAX_TOTAL_BYTES) {
        fail("Source exceeds the total fingerprint size limit.");
      }
      chunks.push(Buffer.from(buffer.subarray(0, bytesRead)));
    }

    const after = await handle.stat();
    if (
      after.dev !== before.dev ||
      after.ino !== before.ino ||
      after.size !== before.size ||
      after.mtimeMs !== before.mtimeMs ||
      after.ctimeMs !== before.ctimeMs
    ) {
      fail("A source file changed while it was being fingerprinted.");
    }
    return Buffer.concat(chunks, fileBytesRead);
  } finally {
    await handle?.close();
  }
}

async function canonicalWorktreeRoot(root) {
  if (typeof root !== "string" || !path.isAbsolute(root)) {
    fail("The worktree root must be an absolute path.");
  }

  let physicalRoot;
  try {
    physicalRoot = await realpath(root);
  } catch {
    fail("The worktree root does not exist.");
  }
  let rootStat;
  try {
    rootStat = await lstat(physicalRoot);
  } catch {
    fail("The worktree root could not be inspected.");
  }
  if (!rootStat.isDirectory()) fail("The worktree root must be a directory.");

  const reportedRoot = runGit(physicalRoot, ["rev-parse", "--show-toplevel"]);
  if (reportedRoot.length < 2 || reportedRoot[reportedRoot.length - 1] !== 0x0a) {
    fail("Git did not report a valid worktree root.");
  }
  const rootBytes = reportedRoot.subarray(0, reportedRoot.length - 1);
  if (rootBytes.includes(0x0a) || rootBytes.includes(0x0d)) {
    fail("Git reported an ambiguous worktree root.");
  }

  let gitRoot;
  try {
    gitRoot = await realpath(UTF8.decode(rootBytes));
  } catch {
    fail("Git worktree root could not be resolved.");
  }
  if (gitRoot !== physicalRoot) fail("The supplied path is not the Git worktree root.");
  return physicalRoot;
}

async function readExpectedSymlink(root, relativePath, expectedTarget) {
  const segments = validateRelativePath(relativePath);
  let current = root;
  for (const segment of segments.slice(0, -1)) {
    current = path.join(current, segment);
    const entry = await lstat(current);
    if (!entry.isDirectory() || entry.isSymbolicLink()) fail("A source link parent is not a regular directory.");
  }
  const fullPath = path.join(current, segments.at(-1));
  if (!(await lstat(fullPath)).isSymbolicLink() || await readlink(fullPath) !== expectedTarget) {
    fail("A source link does not match the allowed target.");
  }
  assertInsideRoot(root, await realpath(fullPath));
  return Buffer.from(`symlink\0${expectedTarget}`, "utf8");
}

export async function fingerprintSource(root, { allowedSymlinks = {} } = {}) {
  const physicalRoot = await canonicalWorktreeRoot(root);
  const paths = decodeGitPathList(runGit(physicalRoot, [
    "ls-files",
    "--cached",
    "--others",
    "--exclude-standard",
    "-z",
  ]));
  const seen = new Set();
  for (const relativePath of paths) {
    if (seen.has(relativePath)) fail("Git returned a duplicate source path.");
    seen.add(relativePath);
  }
  paths.sort((left, right) => Buffer.compare(Buffer.from(left, "utf8"), Buffer.from(right, "utf8")));

  const hash = createHash("sha256");
  let totalBytesRead = 0;
  for (const relativePath of paths) {
    const pathBytes = Buffer.from(relativePath, "utf8");
    const content = Object.hasOwn(allowedSymlinks, relativePath)
      ? await readExpectedSymlink(physicalRoot, relativePath, allowedSymlinks[relativePath])
      : await readBoundedFile(physicalRoot, relativePath, totalBytesRead);
    totalBytesRead += content.length;
    hash.update(lengthPrefix(pathBytes.length));
    hash.update(pathBytes);
    hash.update(lengthPrefix(content.length));
    hash.update(content);
  }
  return hash.digest("hex");
}

function parseArguments(argv) {
  const [command, ...args] = argv;
  const options = new Map();
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index];
    if (flag !== "--root" && flag !== "--expected") fail("Unsupported command arguments.");
    const value = args[index + 1];
    if (!value || value.startsWith("--") || options.has(flag)) fail("Unsupported command arguments.");
    options.set(flag, value);
    index += 1;
  }

  const root = options.get("--root");
  if (!root || !path.isAbsolute(root)) fail("The worktree root must be an absolute path.");
  if (command === "fingerprint" && options.size === 1) return { command, root };
  const expected = options.get("--expected");
  if (
    command === "verify" &&
    options.size === 2 &&
    expected &&
    /^[a-f0-9]{64}$/i.test(expected)
  ) {
    return { command, root, expected: expected.toLowerCase() };
  }
  fail("Unsupported fingerprint command.");
}

async function main(argv) {
  const command = argv[0];
  try {
    const options = parseArguments(argv);
    const digest = await fingerprintSource(options.root);
    if (options.command === "fingerprint") {
      process.stdout.write(`${digest}\n`);
      return;
    }
    if (digest !== options.expected) fail("Source fingerprint mismatch.");
    process.stdout.write("Source fingerprint verified.\n");
  } catch {
    process.stderr.write(command === "verify"
      ? "Source fingerprint verification failed.\n"
      : "Source fingerprint failed.\n");
    process.exitCode = 1;
  }
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}
