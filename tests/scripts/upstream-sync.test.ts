import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const syncScript = resolve("scripts/upstream-sync.mjs");
const identityScript = resolve("scripts/check-fork-identity.mjs");
const forbiddenName = ["Data", "Grip"].join("");
const directories: string[] = [];

type Files = Record<string, string>;

const env = {
  ...process.env,
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_AUTHOR_NAME: "Tester",
  GIT_AUTHOR_EMAIL: "tester@example.invalid",
  GIT_COMMITTER_NAME: "Tester",
  GIT_COMMITTER_EMAIL: "tester@example.invalid",
};

function run(cwd: string, command: string, args: string[], extraEnv: Record<string, string> = {}) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", env: { ...env, ...extraEnv } });
  return { status: result.status, out: `${result.stdout}${result.stderr}`, stdout: result.stdout };
}

function git(cwd: string, ...args: string[]): string {
  const result = run(cwd, "git", args);
  if (result.status !== 0) throw new Error(`git ${args.join(" ")}: ${result.out}`);
  return result.stdout.trim();
}

function write(root: string, files: Files) {
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(dirname(join(root, file)), { recursive: true });
    writeFileSync(join(root, file), content);
  }
}

function read(root: string, file: string): string {
  return readFileSync(join(root, file), "utf8");
}

function tempDirectory(): string {
  const directory = mkdtempSync(join(tmpdir(), "upstream-sync-"));
  directories.push(directory);
  return directory;
}

const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`;

function upstreamFiles(version: string): Files {
  return {
    "package.json": json({ name: "tabularis", private: true, version }),
    "src-tauri/tauri.conf.json": json({
      productName: "Tabularis",
      version,
      identifier: "io.github.debba.tabularis",
      bundle: { createUpdaterArtifacts: true },
      plugins: {
        updater: { endpoints: ["https://example.invalid/latest.json"], pubkey: "key" },
        "deep-link": { desktop: { schemes: ["tabularis"] } },
      },
    }),
    "src-tauri/Cargo.toml": `[package]\nname = "tabularis"\nversion = "${version}"\nedition = "2021"\n\n[dependencies]\nserde = "1"\n`,
    "src-tauri/Cargo.lock": `[[package]]\nname = "serde"\nversion = "1.0.0"\n\n[[package]]\nname = "tabularis"\nversion = "${version}"\n`,
    "src/version.ts": `export const APP_VERSION = "${version}";\n`,
    "src-tauri/src/paths.rs": 'pub const APP_DIR_NAME: &str = "tabularis";\n',
    "src-tauri/src/keychain_utils.rs": 'pub const SERVICE_NAME: &str = "tabularis";\n',
    "src-tauri/src/plugins/deep_link.rs": 'pub const DEEP_LINK_SCHEME: &str = "tabularis";\n',
    "src-tauri/src/mcp/install.rs": 'pub(crate) const MCP_SERVER_KEY: &str = "tabularis";\n',
    "src-tauri/src/updater.rs": "const OFFICIAL_UPDATE_FEED_ENABLED: bool = true;\n",
    "src-tauri/linux/tabularis.desktop": "[Desktop Entry]\nName=Tabularis\n",
    "src/app.ts": "export const greeting = 'hello';\n",
    "README.md": "# Tabularis\n",
  };
}

function forkFiles(): Files {
  const files = upstreamFiles("1.0.0");
  const version = "1.0.0+extended.1";
  const { "src-tauri/linux/tabularis.desktop": _renamed, ...shared } = upstreamFiles(version);
  void _renamed;
  return {
    ...shared,
    "package.json": json({ name: "tabularis-extended", private: true, version }),
    "src-tauri/tauri.conf.json": json({
      productName: "Tabularis Extended",
      version,
      identifier: "io.github.hei5enbug.tabularis.extended",
      bundle: { createUpdaterArtifacts: false },
      plugins: { updater: { endpoints: [], pubkey: "" }, "deep-link": { desktop: { schemes: ["tabularis-extended"] } } },
    }),
    "src-tauri/Cargo.toml": files["src-tauri/Cargo.toml"].replace('name = "tabularis"', 'name = "tabularis-extended"').replace("1.0.0", version),
    "src-tauri/Cargo.lock": files["src-tauri/Cargo.lock"].replace('name = "tabularis"', 'name = "tabularis-extended"').replace(/"1\.0\.0"\n$/, `"${version}"\n`),
    "src-tauri/src/paths.rs": 'pub const APP_DIR_NAME: &str = "tabularis-extended";\n',
    "src-tauri/src/keychain_utils.rs": 'pub const SERVICE_NAME: &str = "tabularis-extended";\n',
    "src-tauri/src/plugins/deep_link.rs": 'pub const DEEP_LINK_SCHEME: &str = "tabularis-extended";\n',
    "src-tauri/src/mcp/install.rs": 'pub(crate) const MCP_SERVER_KEY: &str = "tabularis-extended";\n',
    "src-tauri/src/updater.rs": "const OFFICIAL_UPDATE_FEED_ENABLED: bool = false;\n",
    "src-tauri/linux/tabularis-extended.desktop": "[Desktop Entry]\nName=Tabularis Extended\n",
    "README.md": "# Tabularis Extended\n",
  };
}

function commitAll(root: string, message: string) {
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", message);
}

function makeUpstream(conflictingChange: boolean, lockBump = false, readmeChange = false): string {
  const root = tempDirectory();
  git(root, "init", "-q", "-b", "main");
  write(root, upstreamFiles("1.0.0"));
  commitAll(root, "release 1.0.0");
  git(root, "tag", "v1.0.0");
  write(root, { ...upstreamFiles("1.1.0"), "src/feature.ts": "export const feature = true;\n" });
  if (conflictingChange) write(root, { "src/app.ts": "export const greeting = 'hello from upstream';\n" });
  if (readmeChange) write(root, { "README.md": "# Tabularis 1.1.0\n\nNew upstream text.\n" });
  if (lockBump) {
    write(root, { "src-tauri/Cargo.lock": read(root, "src-tauri/Cargo.lock").replace('version = "1.0.0"', 'version = "1.0.5"') });
  }
  commitAll(root, "release 1.1.0");
  git(root, "tag", "v1.1.0");
  git(root, "tag", "nightly-20250101-abc1234");
  git(root, "tag", "v1.2.0-rc1");
  return root;
}

function makeFork(upstream: string, forkAppChange = false): string {
  const root = tempDirectory();
  git(root, "init", "-q", "-b", "main");
  git(root, "fetch", "-q", "--no-tags", upstream, "+refs/tags/v1.0.0:refs/tags/v1.0.0");
  git(root, "checkout", "-q", "-b", "main", "v1.0.0");
  git(root, "mv", "src-tauri/linux/tabularis.desktop", "src-tauri/linux/tabularis-extended.desktop");
  write(root, { ...forkFiles(), "src/fork-only.ts": "export const fork = true;\n" });
  if (forkAppChange) write(root, { "src/app.ts": "export const greeting = 'hello from the fork';\n" });
  commitAll(root, "feat: fork identity");
  return root;
}

function sync(root: string, upstream: string, ...args: string[]) {
  return run(root, process.execPath, [syncScript, ...args], { UPSTREAM_URL: upstream });
}

afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

describe("upstream sync status", () => {
  it("새 업스트림 릴리스와 병합된 기준 태그, 예상 충돌을 작업 트리 변경 없이 알려 준다", () => {
    // given
    const upstream = makeUpstream(true);
    const fork = makeFork(upstream, true);
    const head = git(fork, "rev-parse", "HEAD");

    // when
    const result = sync(fork, upstream, "status", "--json");

    // then
    expect(result.status).toBe(0);
    const status = JSON.parse(result.stdout);
    expect(status).toMatchObject({ latest: "v1.1.0", base: "v1.0.0", merged: false, behind: 1, conflicts: ["src/app.ts"] });
    expect(status.autoResolved.length).toBeGreaterThan(0);
    expect(git(fork, "rev-parse", "HEAD")).toBe(head);
    expect(git(fork, "status", "--porcelain")).toBe("");
    expect(git(fork, "branch", "--list", "sync/*")).toBe("");
    expect(git(fork, "remote", "get-url", "upstream")).toBe(upstream);
  });

  it("업스트림이 이미 병합된 릴리스라면 최신 상태라고 알려 준다", () => {
    // given
    const upstream = makeUpstream(false);
    const fork = makeFork(upstream);
    git(fork, "fetch", "-q", "--no-tags", upstream, "refs/tags/v1.1.0:refs/tags/v1.1.0");
    git(fork, "merge", "-q", "--no-edit", "-X", "ours", "v1.1.0");

    // when
    const status = JSON.parse(sync(fork, upstream, "status", "--json").stdout);

    // then
    expect(status).toMatchObject({ latest: "v1.1.0", base: "v1.1.0", merged: true, behind: 0 });
  });
});

describe("upstream sync merge", () => {
  it("작업 트리가 더럽다면 병합을 거부하고 브랜치를 만들지 않는다", () => {
    // given
    const upstream = makeUpstream(false);
    const fork = makeFork(upstream);
    write(fork, { "src/fork-only.ts": "export const fork = false;\n" });

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status).not.toBe(0);
    expect(result.out).toContain("not clean");
    expect(git(fork, "branch", "--show-current")).toBe("main");
    expect(git(fork, "branch", "--list", "sync/*")).toBe("");
  });

  it("충돌이 없다면 새 브랜치에서 병합하고 모든 버전 파일을 1.1.0+extended.1로 맞춘다", () => {
    // given
    const upstream = makeUpstream(false);
    const fork = makeFork(upstream);

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status, result.out).toBe(0);
    expect(git(fork, "branch", "--show-current")).toBe("sync/upstream-v1.1.0");
    expect(git(fork, "status", "--porcelain")).toBe("");
    expect(git(fork, "merge-base", "--is-ancestor", "v1.1.0", "HEAD")).toBe("");
    expect(git(fork, "log", "-2", "--format=%s")).toBe(
      "chore: set Tabularis Extended version to 1.1.0+extended.1\nchore: merge upstream Tabularis v1.1.0",
    );
    expect(JSON.parse(read(fork, "package.json")).version).toBe("1.1.0+extended.1");
    expect(JSON.parse(read(fork, "src-tauri/tauri.conf.json")).version).toBe("1.1.0+extended.1");
    expect(read(fork, "src-tauri/Cargo.toml")).toContain('name = "tabularis-extended"\nversion = "1.1.0+extended.1"');
    expect(read(fork, "src-tauri/Cargo.lock")).toContain('name = "tabularis-extended"\nversion = "1.1.0+extended.1"');
    expect(read(fork, "src-tauri/Cargo.lock")).toContain('name = "serde"\nversion = "1.0.0"');
    expect(read(fork, "src/version.ts")).toBe('export const APP_VERSION = "1.1.0+extended.1";\n');
    expect(read(fork, "src/feature.ts")).toContain("feature");
    expect(run(fork, process.execPath, [identityScript]).status).toBe(0);
  });

  it("충돌이 나면 병합을 진행 중인 채로 멈추고 충돌 파일과 다음 명령을 알려 준다", () => {
    // given
    const upstream = makeUpstream(true);
    const fork = makeFork(upstream, true);

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status).not.toBe(0);
    expect(result.out).toContain("src/app.ts");
    expect(result.out).toContain("git merge --continue");
    expect(result.out).toContain("upstream-sync.mjs finish");
    expect(git(fork, "rev-parse", "--verify", "MERGE_HEAD")).toBe(git(upstream, "rev-parse", "v1.1.0^{commit}"));
    expect(git(fork, "diff", "--name-only", "--diff-filter=U")).toBe("src/app.ts");
    expect(git(fork, "config", "rerere.enabled")).toBe("true");
    expect(git(fork, "config", "rerere.autoupdate")).toBe("true");
  });

  it("업스트림이 버전 파일에서 앱 릴리스 버전이 아닌 의존성 버전을 바꾼 충돌은 자동으로 풀지 않는다", () => {
    // given
    const upstream = makeUpstream(false, true);
    const fork = makeFork(upstream);
    write(fork, { "src-tauri/Cargo.lock": read(fork, "src-tauri/Cargo.lock").replace('version = "1.0.0"', 'version = "1.0.2"') });
    commitAll(fork, "chore: bump serde in the fork");

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status).not.toBe(0);
    expect(git(fork, "diff", "--name-only", "--diff-filter=U")).toBe("src-tauri/Cargo.lock");
    expect(read(fork, "src-tauri/Cargo.lock")).toContain('version = "1.0.5"');
  });

  it("앱 버전과 같은 값이라도 의존성 버전 충돌은 자동으로 풀지 않는다", () => {
    // given
    const upstream = tempDirectory();
    git(upstream, "init", "-q", "-b", "main");
    const withDependency = (files: Files, version: string, dependency: string) => ({
      ...files,
      "src-tauri/Cargo.toml": files["src-tauri/Cargo.toml"].replace('serde = "1"', `serde = "1"\nshared = "${dependency}"`),
    });
    write(upstream, withDependency(upstreamFiles("1.0.0"), "1.0.0", "1.0.0"));
    commitAll(upstream, "release 1.0.0");
    git(upstream, "tag", "v1.0.0");
    write(upstream, withDependency(upstreamFiles("1.1.0"), "1.1.0", "1.1.0"));
    commitAll(upstream, "release 1.1.0");
    git(upstream, "tag", "v1.1.0");
    const fork = makeFork(upstream);
    write(fork, { "src-tauri/Cargo.toml": read(fork, "src-tauri/Cargo.toml").replace('serde = "1"', 'serde = "1"\nshared = "1.0.1"') });
    commitAll(fork, "chore: pin shared in the fork");

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status).not.toBe(0);
    expect(git(fork, "diff", "--name-only", "--diff-filter=U")).toBe("src-tauri/Cargo.toml");
    expect(read(fork, "src-tauri/Cargo.toml")).toContain('shared = "1.1.0"');
  });

  it("업스트림이 포크가 쓴 README를 바꿔도 충돌로 멈추지 않고 포크의 README를 지킨다", () => {
    // given
    const upstream = makeUpstream(false, false, true);
    const fork = makeFork(upstream);

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status, result.out).toBe(0);
    expect(read(fork, "README.md")).toBe("# Tabularis Extended\n");
    expect(git(fork, "status", "--porcelain")).toBe("");
    expect(git(fork, "merge-base", "--is-ancestor", "v1.1.0", "HEAD")).toBe("");
  });

  it("로컬 릴리스 태그가 업스트림의 같은 태그와 다르면 병합하지 않고 멈춘다", () => {
    // given
    const upstream = makeUpstream(false);
    const fork = makeFork(upstream);
    git(fork, "tag", "v1.1.0", "HEAD");

    // when
    const result = sync(fork, upstream, "sync");

    // then
    expect(result.status).not.toBe(0);
    expect(result.out).toContain("cannot be trusted");
    expect(git(fork, "branch", "--list", "sync/*")).toBe("");
  });

  it("충돌을 직접 풀고 finish를 실행하면 버전 커밋까지 끝난다", () => {
    // given
    const upstream = makeUpstream(true);
    const fork = makeFork(upstream, true);
    expect(sync(fork, upstream, "sync").status).not.toBe(0);
    write(fork, { "src/app.ts": "export const greeting = 'hello from both';\n" });
    git(fork, "add", "src/app.ts");
    run(fork, "git", ["commit", "--no-edit", "-q"]);

    // when
    const result = run(fork, process.execPath, [syncScript, "finish"], { UPSTREAM_URL: upstream });

    // then
    expect(result.status, result.out).toBe(0);
    expect(read(fork, "src/version.ts")).toBe('export const APP_VERSION = "1.1.0+extended.1";\n');
    expect(git(fork, "status", "--porcelain")).toBe("");
  });
});

describe("fork identity check", () => {
  it("포크 정체성이 유지되면 통과한다", () => {
    // given
    const fork = makeFork(makeUpstream(false));

    // when
    const result = run(fork, process.execPath, [identityScript]);

    // then
    expect(result.status, result.out).toBe(0);
  });

  it("포크가 추가한 줄에 다른 IDE 제품명이 있으면 대소문자와 관계없이 실패한다", () => {
    // given
    const fork = makeFork(makeUpstream(false));
    write(fork, { "src/fork-only.ts": `// compatible with ${forbiddenName.toLowerCase()}\n` });
    commitAll(fork, "feat: mention");

    // when
    const result = run(fork, process.execPath, [identityScript]);

    // then
    expect(result.status).toBe(1);
    expect(result.out).toContain("src/fork-only.ts");
  });

  it("업스트림이 추가한 줄에 제품명이 있어도 허용한다", () => {
    // given
    const upstream = makeUpstream(false);
    write(upstream, { "docs.md": `Works with ${forbiddenName}.\n` });
    commitAll(upstream, "docs");
    git(upstream, "tag", "v1.1.1");
    const fork = makeFork(upstream);
    git(fork, "fetch", "-q", "--no-tags", upstream, "refs/tags/v1.1.1:refs/tags/v1.1.1");
    git(fork, "merge", "-q", "--no-edit", "-X", "ours", "v1.1.1");

    // when
    const result = run(fork, process.execPath, [identityScript]);

    // then
    expect(result.status, result.out).toBe(0);
  });

  it("번들 식별자가 바뀌면 실패한다", () => {
    // given
    const fork = makeFork(makeUpstream(false));
    const file = "src-tauri/tauri.conf.json";
    write(fork, { [file]: read(fork, file).replace("io.github.hei5enbug.tabularis.extended", "io.github.debba.tabularis") });
    commitAll(fork, "fix: identifier");

    // when
    const result = run(fork, process.execPath, [identityScript]);

    // then
    expect(result.status).toBe(1);
    expect(result.out).toContain("identifier");
  });

  it("버전 파일끼리 값이 다르면 실패한다", () => {
    // given
    const fork = makeFork(makeUpstream(false));
    write(fork, { "src/version.ts": 'export const APP_VERSION = "1.0.0+extended.2";\n' });
    commitAll(fork, "fix: version");

    // when
    const result = run(fork, process.execPath, [identityScript]);

    // then
    expect(result.status).toBe(1);
    expect(result.out).toContain("versions differ");
  });
});
