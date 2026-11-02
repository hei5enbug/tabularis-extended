import {spawnSync} from 'node:child_process';
import {existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {afterEach, describe, expect, it, vi} from 'vitest';

vi.setConfig({testTimeout: 60_000});

const script = resolve('scripts/sync-extension-repos.mjs');
const folder = 'extensions/demo';
const sourceRepo = 'owner/source';
const directories: string[] = [];

const env = {
    ...process.env,
    GIT_CONFIG_GLOBAL: '/dev/null',
    GIT_CONFIG_NOSYSTEM: '1',
    GIT_AUTHOR_NAME: 'Sync Bot',
    GIT_AUTHOR_EMAIL: 'bot@example.invalid',
    GIT_COMMITTER_NAME: 'Sync Bot',
    GIT_COMMITTER_EMAIL: 'bot@example.invalid',
};

type Files = Record<string, string | null>;
type Author = {name: string; email: string; date: string};

const alice: Author = {
    name: 'Alice',
    email: 'alice@example.invalid',
    date: '2026-01-02T03:04:05+09:00',
};
const bob: Author = {name: 'Bob', email: 'bob@example.invalid', date: '2026-02-03T04:05:06+00:00'};

function run(cwd: string, command: string, args: string[], extraEnv: Record<string, string> = {}) {
    const result = spawnSync(command, args, {cwd, encoding: 'utf8', env: {...env, ...extraEnv}});
    return {status: result.status, out: `${result.stdout}${result.stderr}`, stdout: result.stdout};
}

function git(cwd: string, ...args: string[]): string {
    const result = run(cwd, 'git', args);
    if (result.status !== 0) throw new Error(`git ${args.join(' ')}: ${result.out}`);
    return result.stdout.trim();
}

function write(root: string, files: Files) {
    for (const [file, content] of Object.entries(files)) {
        const path = join(root, file);
        if (content === null) {
            rmSync(path);
            continue;
        }
        mkdirSync(dirname(path), {recursive: true});
        writeFileSync(path, content);
    }
}

function commitAll(root: string, message: string, author: Author = alice): string {
    git(root, 'add', '-A');
    const result = run(root, 'git', ['commit', '-q', '-m', message], {
        GIT_AUTHOR_NAME: author.name,
        GIT_AUTHOR_EMAIL: author.email,
        GIT_AUTHOR_DATE: author.date,
    });
    if (result.status !== 0) throw new Error(result.out);
    return git(root, 'rev-parse', 'HEAD');
}

function repository(name: string): string {
    const root = mkdtempSync(join(tmpdir(), `sync-extension-${name}-`));
    directories.push(root);
    git(root, 'init', '-q', '-b', 'main');
    return root;
}

function sync(mode: 'catch-up' | 'replay', source: string, target: string, head = 'HEAD') {
    return run(source, 'node', [
        script,
        mode,
        '--folder',
        folder,
        '--target',
        target,
        '--source-repo',
        sourceRepo,
        '--head',
        head,
    ]);
}

function targetLog(target: string): string[] {
    return git(target, 'log', '--format=%an|%ae|%aI|%B%x00')
        .split('\0')
        .map((entry) => entry.trim())
        .filter(Boolean);
}

function setup() {
    const source = repository('source');
    write(source, {
        'README.md': 'host\n',
        [`${folder}/a.txt`]: 'a1\n',
        [`${folder}/gone.txt`]: 'old\n',
    });
    const base = commitAll(source, 'feat: import demo');
    const target = repository('target');
    write(target, {
        'a.txt': 'a1\n',
        'gone.txt': 'old\n',
        '.github/workflows/bootstrap.yml': 'own ci\n',
    });
    commitAll(target, 'initial standalone state', bob);
    const caughtUp = sync('catch-up', source, target);
    if (caughtUp.status !== 0) throw new Error(caughtUp.out);
    return {source, target, base};
}

afterEach(() => {
    for (const directory of directories.splice(0))
        rmSync(directory, {recursive: true, force: true});
});

describe('sync-extension-repos replay', () => {
    it('폴더를 바꾼 소스 커밋만 순서대로 원래 메시지와 작성자, Synced-From 트레일러로 재생한다', () => {
        // given
        const {source, target} = setup();
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        const first = commitAll(
            source,
            'fix: change a\n\nBody line.\n\nCo-Authored-By: Carol <carol@example.invalid>',
            alice,
        );
        write(source, {'README.md': 'host only\n'});
        commitAll(source, 'docs: host change');
        write(source, {[`${folder}/b.bin`]: '\u0000\u0001binary', [`${folder}/gone.txt`]: null});
        const second = commitAll(source, 'feat: add b and drop gone', bob);

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        const [newest, older] = targetLog(target);
        expect(older).toBe(
            `Alice|alice@example.invalid|2026-01-02T03:04:05+09:00|fix: change a\n\nBody line.\n\n` +
                `Co-Authored-By: Carol <carol@example.invalid>\nSynced-From: ${sourceRepo}@${first}`,
        );
        expect(newest).toBe(
            `Bob|bob@example.invalid|2026-02-03T04:05:06Z|feat: add b and drop gone\n\nSynced-From: ${sourceRepo}@${second}`,
        );
        expect(git(target, 'ls-files').split('\n').sort()).toEqual([
            '.github/workflows/bootstrap.yml',
            'a.txt',
            'b.bin',
        ]);
        expect(readFileSync(join(target, 'b.bin'), 'utf8')).toBe('\u0000\u0001binary');
        expect(readFileSync(join(target, '.github/workflows/bootstrap.yml'), 'utf8')).toBe(
            'own ci\n',
        );
    });

    it('이미 재생한 커밋 뒤에 새 커밋이 없으면 다시 실행해도 대상에 커밋을 만들지 않는다', () => {
        // given
        const {source, target} = setup();
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        commitAll(source, 'fix: change a');
        sync('replay', source, target);
        write(source, {'README.md': 'host only\n'});
        commitAll(source, 'docs: host change');
        const before = git(target, 'rev-parse', 'HEAD');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(result.out).toContain('Nothing to replay');
        expect(git(target, 'rev-parse', 'HEAD')).toBe(before);
    });

    it('이미 동기화한 지점보다 오래된 소스 커밋을 지정하면 대상에 커밋하지 않고 성공한다', () => {
        // given
        const {source, target, base} = setup();
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        commitAll(source, 'fix: change a');
        sync('replay', source, target);
        const before = git(target, 'rev-parse', 'HEAD');

        // when
        const result = sync('replay', source, target, base);

        // then
        expect(result.status, result.out).toBe(0);
        expect(result.out).toContain('already synced');
        expect(git(target, 'rev-parse', 'HEAD')).toBe(before);
    });

    it('main에 병합한 기능 브랜치는 병합 커밋 하나로 재생하고 폴더를 바꾸지 않은 병합은 건너뛴다', () => {
        // given
        const {source, target} = setup();
        git(source, 'checkout', '-q', '-b', 'feature');
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        commitAll(source, 'wip: one');
        write(source, {[`${folder}/c.txt`]: 'c\n'});
        commitAll(source, 'wip: two');
        git(source, 'checkout', '-q', 'main');
        git(source, 'merge', '-q', '--no-ff', '-m', 'feat: merge feature', 'feature');
        git(source, 'checkout', '-q', '-b', 'host');
        write(source, {'README.md': 'host 2\n'});
        commitAll(source, 'chore: host only');
        git(source, 'checkout', '-q', 'main');
        git(source, 'merge', '-q', '--no-ff', '-m', 'chore: merge host', 'host');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(git(target, 'log', '-1', '--format=%s')).toBe('feat: merge feature');
        expect(git(target, 'log', '-1', '--skip=1', '--format=%s')).toMatch(
            /^chore: sync from source@/,
        );
        expect(readFileSync(join(target, 'c.txt'), 'utf8')).toBe('c\n');
    });

    it('대상 전용 파일과 겹치는 소스 커밋은 덮어쓰지 않고 이유와 함께 실패한다', () => {
        // given
        const {source, target} = setup();
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        commitAll(source, 'fix: change a');
        write(source, {[`${folder}/.github/workflows/bootstrap.yml`]: 'source ci\n'});
        const clash = commitAll(source, 'ci: add bootstrap');
        const before = git(target, 'rev-parse', 'HEAD');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status).toBe(1);
        expect(result.out).toContain(`Source commit ${clash} does not apply cleanly`);
        expect(readFileSync(join(target, '.github/workflows/bootstrap.yml'), 'utf8')).toBe(
            'own ci\n',
        );
        expect(git(target, 'rev-list', '--count', `${before}..HEAD`)).toBe('1');
    });

    it('대상에 Synced-From 트레일러가 없으면 catch-up을 먼저 실행하라고 안내하며 실패한다', () => {
        // given
        const source = repository('source');
        write(source, {[`${folder}/a.txt`]: 'a\n'});
        commitAll(source, 'feat: import demo');
        const target = repository('target');
        write(target, {'a.txt': 'a\n'});
        commitAll(target, 'initial');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status).toBe(1);
        expect(result.out).toContain('Run catch-up once first');
    });

    it('소스 커밋 메시지에 이미 Synced-From 트레일러가 있어도 다시 실행하면 같은 커밋을 재생하지 않는다', () => {
        // given
        const {source, target, base} = setup();
        write(source, {[`${folder}/a.txt`]: 'a2\n'});
        commitAll(source, `fix: change a\n\nSynced-From: ${sourceRepo}@${base}`);
        sync('replay', source, target);
        write(source, {'README.md': 'host only\n'});
        commitAll(source, 'docs: host change');
        const before = git(target, 'rev-parse', 'HEAD');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(result.out).toContain('Nothing to replay');
        expect(git(target, 'rev-parse', 'HEAD')).toBe(before);
    });

    it('폴더 전체를 지운 소스 커밋을 재생하면 대상 전용 파일만 남긴다', () => {
        // given
        const {source, target} = setup();
        git(source, 'rm', '-q', '-r', folder);
        const removal = commitAll(source, 'chore: remove demo');

        // when
        const result = sync('replay', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(git(target, 'log', '-1', '--format=%B')).toContain(
            `Synced-From: ${sourceRepo}@${removal}`,
        );
        expect(git(target, 'ls-files')).toBe('.github/workflows/bootstrap.yml');
    });
});

describe('sync-extension-repos catch-up', () => {
    it('export-ignore 속성이 붙은 파일도 소스 내용 그대로 대상에 복사한다', () => {
        // given
        const source = repository('source');
        write(source, {
            [`${folder}/.gitattributes`]: 'a.txt export-ignore\nb.txt export-subst\n',
            [`${folder}/a.txt`]: 'new\n',
            [`${folder}/b.txt`]: '$Format:%H$\n',
        });
        commitAll(source, 'feat: import demo');
        const target = repository('target');
        write(target, {'a.txt': 'stale\n'});
        commitAll(target, 'initial');

        // when
        const result = sync('catch-up', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(git(target, 'show', 'HEAD:a.txt')).toBe('new');
        expect(git(target, 'show', 'HEAD:b.txt')).toBe('$Format:%H$');
        expect(readFileSync(join(target, 'a.txt'), 'utf8')).toBe('new\n');
        expect(git(target, 'status', '--porcelain')).toBe('');
    });

    it('폴더 내용을 대상에 덮어쓰고 대상 전용 파일은 남기며 시작 지점 트레일러를 남긴다', () => {
        // given
        const source = repository('source');
        write(source, {[`${folder}/a.txt`]: 'new\n', [`${folder}/nested/new.txt`]: 'n\n'});
        const head = commitAll(source, 'feat: import demo');
        const target = repository('target');
        write(target, {'a.txt': 'stale\n', 'integration/run.sh': 'own\n'});
        commitAll(target, 'initial');

        // when
        const result = sync('catch-up', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(git(target, 'log', '-1', '--format=%B')).toBe(
            `chore: sync from source@${head.slice(0, 12)}\n\nSynced-From: ${sourceRepo}@${head}`,
        );
        expect(readFileSync(join(target, 'a.txt'), 'utf8')).toBe('new\n');
        expect(readFileSync(join(target, 'nested/new.txt'), 'utf8')).toBe('n\n');
        expect(readFileSync(join(target, 'integration/run.sh'), 'utf8')).toBe('own\n');
        expect(result.out).toContain('integration/run.sh');
    });

    it('이전 동기화 이후 폴더에서 지운 파일을 대상에서도 지우고 같은 지점에서는 다시 커밋하지 않는다', () => {
        // given
        const {source, target} = setup();
        write(source, {[`${folder}/gone.txt`]: null, [`${folder}/a.txt`]: 'a3\n'});
        commitAll(source, 'chore: drop gone');
        sync('catch-up', source, target);
        const before = git(target, 'rev-parse', 'HEAD');

        // when
        const result = sync('catch-up', source, target);

        // then
        expect(result.status, result.out).toBe(0);
        expect(result.out).toContain('already synced');
        expect(git(target, 'rev-parse', 'HEAD')).toBe(before);
        expect(existsSync(join(target, 'gone.txt'))).toBe(false);
        expect(readFileSync(join(target, 'a.txt'), 'utf8')).toBe('a3\n');
        expect(existsSync(join(target, '.github/workflows/bootstrap.yml'))).toBe(true);
    });
});
