import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { fingerprintSource } from "../../scripts/companion-source.mjs";

const scriptPath = fileURLToPath(new URL("../../scripts/companion-source.mjs", import.meta.url));

async function createRepository() {
  const root = await mkdtemp(path.join(os.tmpdir(), "tabularis-companion-source-"));
  execFileSync("git", ["init", "--quiet"], { cwd: root, stdio: "ignore" });
  return root;
}

function git(root, args) {
  execFileSync("git", args, { cwd: root, stdio: "ignore" });
}

function runCli(args) {
  return spawnSync(process.execPath, [scriptPath, ...args], {
    encoding: "utf8",
    maxBuffer: 1024 * 1024,
  });
}

test("추적 파일과 untracked 파일의 내용이 digest를 바꿉니다.", async () => {
  // given
  const root = await createRepository();
  try {
    const trackedPath = path.join(root, "tracked.txt");
    await writeFile(trackedPath, "first content");
    git(root, ["add", "tracked.txt"]);

    // when
    const first = await fingerprintSource(root);
    await writeFile(trackedPath, "changed content");
    const changedTracked = await fingerprintSource(root);
    await writeFile(path.join(root, "untracked.txt"), "new untracked content");
    const changedUntracked = await fingerprintSource(root);

    // then
    assert.notEqual(changedTracked, first);
    assert.notEqual(changedUntracked, changedTracked);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("untracked에서 tracked 또는 commit 상태로 바뀌어도 같은 내용을 유지하면 digest가 같습니다.", async () => {
  // given
  const root = await createRepository();
  try {
    await writeFile(path.join(root, "same.txt"), "same content");

    // when
    const untracked = await fingerprintSource(root);
    git(root, ["add", "same.txt"]);
    const staged = await fingerprintSource(root);
    git(root, ["-c", "user.name=Fingerprint Test", "-c", "user.email=fingerprint@example.test", "commit", "--quiet", "-m", "fixture"]);
    const committed = await fingerprintSource(root);

    // then
    assert.equal(staged, untracked);
    assert.equal(committed, untracked);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("gitignore 대상 build output은 fingerprint에서 제외됩니다.", async () => {
  // given
  const root = await createRepository();
  try {
    await writeFile(path.join(root, ".gitignore"), "dist/\n");
    git(root, ["add", ".gitignore"]);
    const before = await fingerprintSource(root);
    await mkdir(path.join(root, "dist"));
    await writeFile(path.join(root, "dist", "bundle.js"), "ignored output");

    // when
    const after = await fingerprintSource(root);

    // then
    assert.equal(after, before);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("삭제된 tracked 파일은 조용히 건너뛰지 않고 실패합니다.", async () => {
  // given
  const root = await createRepository();
  try {
    const trackedPath = path.join(root, "missing.txt");
    await writeFile(trackedPath, "tracked before deletion");
    git(root, ["add", "missing.txt"]);
    await rm(trackedPath);

    // when
    const result = fingerprintSource(root);

    // then
    await assert.rejects(result, /tracked or unignored source file is missing/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("symlink 파일과 symlink 상위 경로를 거부합니다.", async () => {
  // given
  const root = await createRepository();
  const outside = await mkdtemp(path.join(os.tmpdir(), "tabularis-companion-outside-"));
  try {
    await writeFile(path.join(outside, "external.txt"), "outside");
    await symlink(path.join(outside, "external.txt"), path.join(root, "external-link.txt"));

    // when
    const fileSymlinkResult = fingerprintSource(root);

    // then
    await assert.rejects(fileSymlinkResult, /Symbolic links are not allowed/);
    await rm(path.join(root, "external-link.txt"));

    await mkdir(path.join(root, "linked-parent", "nested"), { recursive: true });
    await writeFile(path.join(root, "linked-parent", "nested", "file.txt"), "tracked");
    git(root, ["add", "linked-parent/nested/file.txt"]);
    await rm(path.join(root, "linked-parent"), { recursive: true });
    await symlink(outside, path.join(root, "linked-parent"), "dir");

    // when
    const parentSymlinkResult = fingerprintSource(root);

    // then
    await assert.rejects(parentSymlinkResult, /Symbolic links are not allowed/);
  } finally {
    await rm(root, { recursive: true, force: true });
    await rm(outside, { recursive: true, force: true });
  }
});

test("중첩 디렉터리를 Git worktree root로 잘못 지정하면 거부합니다.", async () => {
  // given
  const root = await createRepository();
  const nested = path.join(root, "nested");
  try {
    await mkdir(nested);

    // when
    const result = fingerprintSource(nested);

    // then
    await assert.rejects(result, /not the Git worktree root/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("Unicode와 공백이 있는 상대 파일명은 생성 순서와 무관하게 같은 digest를 냅니다.", async () => {
  // given
  const firstRoot = await createRepository();
  const secondRoot = await createRepository();
  const contents = new Map([
    ["space name.txt", "space"],
    ["한글 파일.md", "unicode"],
    ["e\u0301.txt", "decomposed"],
  ]);
  try {
    for (const name of contents.keys()) await writeFile(path.join(firstRoot, name), contents.get(name));
    for (const name of [...contents.keys()].reverse()) await writeFile(path.join(secondRoot, name), contents.get(name));

    // when
    const first = await fingerprintSource(firstRoot);
    const second = await fingerprintSource(secondRoot);

    // then
    assert.equal(second, first);
  } finally {
    await rm(firstRoot, { recursive: true, force: true });
    await rm(secondRoot, { recursive: true, force: true });
  }
});

test("CLI fingerprint digest를 verify하고 불일치 시 민감한 값을 출력하지 않습니다.", async () => {
  // given
  const root = await createRepository();
  try {
    await writeFile(path.join(root, "source.txt"), "release input");
    const digest = await fingerprintSource(root);

    // when
    const fingerprint = runCli(["fingerprint", "--root", root]);
    const verified = runCli(["verify", "--root", root, "--expected", digest]);
    const mismatch = runCli(["verify", "--root", root, "--expected", "0".repeat(64)]);

    // then
    assert.equal(fingerprint.status, 0, fingerprint.stderr);
    assert.equal(fingerprint.stdout, `${digest}\n`);
    assert.equal(verified.status, 0, verified.stderr);
    assert.equal(verified.stdout, "Source fingerprint verified.\n");
    assert.equal(mismatch.status, 1);
    assert.equal(mismatch.stdout, "");
    assert.equal(mismatch.stderr, "Source fingerprint verification failed.\n");
    assert.equal(mismatch.stderr.includes(root), false);
    assert.equal(mismatch.stderr.includes(digest), false);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
