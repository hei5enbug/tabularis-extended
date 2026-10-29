#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
import {existsSync, readFileSync, writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {
    FORK_VERSION,
    STABLE_TAG,
    compareTags,
    git,
    newestMergedStableTag,
    versionSources,
} from './check-fork-identity.mjs';

const UPSTREAM_URL = process.env.UPSTREAM_URL || 'https://github.com/TabularisDB/tabularis.git';
const MAX_COMMITS = 100;
const USAGE = `Usage:
  node scripts/upstream-sync.mjs status [--json]   show the newest upstream release and a conflict preview
  node scripts/upstream-sync.mjs sync [<tag>]      merge an upstream release on a new sync/upstream-<tag> branch
  node scripts/upstream-sync.mjs finish            set the fork version and commit it after a resolved merge`;

function fail(message) {
    throw new Error(message);
}

function ensureUpstream(root) {
    if (git(root, ['remote', 'get-url', 'upstream'], {allowFail: true}).status !== 0) {
        git(root, ['remote', 'add', 'upstream', UPSTREAM_URL]);
    }
}

function fetchUpstreamTags(root) {
    const result = git(root, ['fetch', '--no-tags', 'upstream', 'refs/tags/v*:refs/tags/v*'], {
        allowFail: true,
    });
    if (result.status === 0) return;
    if (/clobber|rejected/.test(result.stderr)) {
        fail(
            `Some local release tags point somewhere else than the same upstream tags, so the release to merge cannot be trusted.\n` +
                `Check them with 'git show-ref --tags' and 'git ls-remote --tags upstream', fix or delete the wrong local tags yourself, then run again:\n` +
                result.stderr.trim(),
        );
    }
    fail(`Cannot fetch upstream tags:\n${result.stderr.trim()}`);
}

function upstreamStableTags(root) {
    const lines = git(root, [
        'ls-remote',
        '--tags',
        '--refs',
        'upstream',
        'refs/tags/v*',
    ]).stdout.split('\n');
    const tags = lines
        .map((line) => line.split('refs/tags/')[1])
        .filter((tag) => tag && STABLE_TAG.test(tag));
    return tags.sort(compareTags);
}

function isAncestor(root, ref, of) {
    return git(root, ['merge-base', '--is-ancestor', ref, of], {allowFail: true}).status === 0;
}

const CONFLICT_HUNK =
    /^<{7}[^\r\n]*\r?\n([\s\S]*?)^\|{7}[^\r\n]*\r?\n([\s\S]*?)^={7}\r?\n([\s\S]*?)^>{7}[^\r\n]*\r?\n/gm;

// The line that holds the application version in each version file, and what must surround it.
const APP_VERSION_LINES = {
    'package.json': {line: /^ {2}"version": "/},
    'src-tauri/tauri.conf.json': {line: /^ {2}"version": "/},
    'packages/web-ui/src/version.ts': {line: /APP_VERSION\s*=/},
    'src-tauri/Cargo.toml': {line: /^version = "/, owner: /^\[(.+)\]\s*$/, expected: ['package']},
    'src-tauri/Cargo.lock': {
        line: /^version = "/,
        owner: /^name = "(.+)"\s*$/,
        expected: ['tabularis', 'tabularis-extended'],
    },
};

function isAppVersionLine(file, before, line) {
    const rule = APP_VERSION_LINES[file];
    if (!rule || !rule.line.test(line)) return false;
    if (!rule.owner) return true;
    const owner = before
        .split('\n')
        .reverse()
        .map((text) => rule.owner.exec(text))
        .find(Boolean);
    return Boolean(owner) && rule.expected.includes(owner[1]);
}

// A hunk is safe to settle only when every line upstream changed there is the application version line, moved
// from its old release to the new one (for example 0.27.0 to 0.28.0): keep the fork side, and finish writes the
// new fork version. Anything else, such as a dependency version, stays a conflict.
function resolveVersionHunks(text, fromCore, toCore, file) {
    const onlyReleaseBump = (base, theirs, offset) => {
        if (!fromCore || !toCore || base === theirs) return false;
        const baseLines = base.split('\n');
        const theirLines = theirs.split('\n');
        if (baseLines.length !== theirLines.length) return false;
        return baseLines.every((line, index) => {
            if (line === theirLines[index]) return true;
            const before = `${text.slice(0, offset)}${baseLines.slice(0, index).join('\n')}`;
            return (
                theirLines[index] === line.split(fromCore).join(toCore) &&
                line.includes(fromCore) &&
                isAppVersionLine(file, before, line)
            );
        });
    };
    const resolved = text.replace(CONFLICT_HUNK, (hunk, ours, base, theirs, offset) =>
        onlyReleaseBump(base, theirs, offset) ? ours : hunk,
    );
    return {
        text: resolved,
        changed: resolved !== text,
        remaining: (resolved.match(/^<{7}( |\r?$)/gm) ?? []).length,
    };
}

const versionFiles = new Set(versionSources.map((source) => source.file));
// Files the fork writes itself: an upstream change to them never matters here, so the fork side always wins.
const forkOwnedFiles = new Set(['README.md']);

function previewMerge(root, tag, base) {
    const result = git(
        root,
        [
            '-c',
            'merge.conflictStyle=diff3',
            'merge-tree',
            '--write-tree',
            '--name-only',
            '--no-messages',
            'HEAD',
            tag,
        ],
        {allowFail: true},
    );
    if (result.status !== 0 && result.status !== 1)
        fail(`Cannot preview the merge:\n${result.stderr.trim()}`);
    if (result.status === 0) return {conflicts: [], autoResolved: []};
    const [tree, ...rest] = result.stdout.split('\n');
    const end = rest.indexOf('');
    const files = [...new Set(rest.slice(0, end < 0 ? rest.length : end).filter(Boolean))];
    const conflicts = [];
    const autoResolved = [];
    for (const file of files) {
        if (forkOwnedFiles.has(file)) {
            autoResolved.push(file);
            continue;
        }
        const blob = versionFiles.has(file)
            ? git(root, ['show', `${tree}:${file}`], {allowFail: true})
            : null;
        if (
            blob &&
            blob.status === 0 &&
            resolveVersionHunks(blob.stdout, base?.slice(1), tag.slice(1), file).remaining === 0
        )
            autoResolved.push(file);
        else conflicts.push(file);
    }
    return {conflicts, autoResolved};
}

function collectStatus(root) {
    ensureUpstream(root);
    fetchUpstreamTags(root);
    const tags = upstreamStableTags(root);
    if (tags.length === 0) fail('The upstream repository has no stable vX.Y.Z tags.');
    const latest = tags.at(-1);
    const base = newestMergedStableTag(root, new Set(tags));
    const merged = isAncestor(root, latest, 'HEAD');
    const status = {
        upstreamUrl: git(root, ['remote', 'get-url', 'upstream']).stdout.trim(),
        branch: git(root, ['branch', '--show-current']).stdout.trim(),
        latest,
        base,
        merged,
        behind: 0,
        commits: [],
        conflicts: [],
        autoResolved: [],
    };
    if (merged) return status;
    const range = `HEAD..${latest}`;
    status.behind = Number(git(root, ['rev-list', '--count', '--no-merges', range]).stdout.trim());
    status.commits = git(root, [
        'log',
        '--no-merges',
        `--max-count=${MAX_COMMITS}`,
        '--format=%h%x09%s',
        range,
    ])
        .stdout.split('\n')
        .filter(Boolean)
        .map((line) => {
            const [hash, ...subject] = line.split('\t');
            return {hash, subject: subject.join('\t')};
        });
    Object.assign(status, previewMerge(root, latest, base));
    return status;
}

function printStatus(status) {
    console.log(`Upstream:        ${status.upstreamUrl}`);
    console.log(`Newest release:  ${status.latest}`);
    console.log(`Merged base:     ${status.base ?? 'none found'}`);
    if (status.merged) {
        console.log(
            `Up to date: ${status.latest} is already merged into ${status.branch || 'HEAD'}.`,
        );
        return;
    }
    console.log(`Commits behind:  ${status.behind}`);
    if (status.autoResolved.length > 0) {
        console.log(
            `Conflicts that "sync" settles by itself (version lines and fork-owned files):\n${status.autoResolved.map((file) => `  ${file}`).join('\n')}`,
        );
    }
    if (status.conflicts.length === 0)
        console.log('Predicted conflicts: none. Run: node scripts/upstream-sync.mjs sync');
    else {
        console.log(
            `Predicted conflicts (${status.conflicts.length}):\n${status.conflicts.map((file) => `  ${file}`).join('\n')}`,
        );
        console.log('Run: node scripts/upstream-sync.mjs sync');
    }
}

function inProgress(root) {
    if (git(root, ['rev-parse', '-q', '--verify', 'MERGE_HEAD'], {allowFail: true}).status === 0)
        return 'merge';
    for (const name of ['rebase-merge', 'rebase-apply', 'CHERRY_PICK_HEAD', 'REVERT_HEAD']) {
        const path = git(root, ['rev-parse', '--git-path', name]).stdout.trim();
        if (existsSync(join(root, path))) return name;
    }
    return null;
}

function assertReady(root, {untracked}) {
    const running = inProgress(root);
    if (running === 'merge')
        fail(
            "A merge is already in progress. Run 'git merge --continue' or 'git merge --abort' first.",
        );
    if (running) fail(`A git operation is in progress (${running}). Finish or abort it first.`);
    const dirty = git(root, [
        'status',
        '--porcelain',
        `--untracked-files=${untracked ? 'normal' : 'no'}`,
    ])
        .stdout.split('\n')
        .filter(Boolean);
    if (dirty.length > 0) {
        const shown = dirty.slice(0, 10).join('\n  ');
        fail(
            `The work tree is not clean. Commit or stash first:\n  ${shown}${dirty.length > 10 ? '\n  ...' : ''}`,
        );
    }
}

function unmergedFiles(root) {
    return git(root, ['diff', '--name-only', '--diff-filter=U']).stdout.split('\n').filter(Boolean);
}

function settleForkOwnedConflicts(root) {
    for (const file of unmergedFiles(root).filter((name) => forkOwnedFiles.has(name))) {
        git(root, ['checkout', '--ours', '--', file]);
        git(root, ['add', '--', file]);
    }
}

function settleVersionConflicts(root, fromCore, toCore) {
    for (const file of unmergedFiles(root).filter((name) => versionFiles.has(name))) {
        const path = join(root, file);
        if (!existsSync(path)) continue;
        const result = resolveVersionHunks(readFileSync(path, 'utf8'), fromCore, toCore, file);
        if (!result.changed) continue;
        writeFileSync(path, result.text);
        if (result.remaining === 0) git(root, ['add', '--', file]);
    }
}

function finish(root) {
    assertReady(root, {untracked: false});
    const base = newestMergedStableTag(root);
    if (!base)
        fail(
            'No upstream release tag (vX.Y.Z) is merged into HEAD. Run the status command first to fetch the tags.',
        );
    const core = base.slice(1);
    const texts = versionSources.map((source) => readFileSync(join(root, source.file), 'utf8'));
    const current = versionSources.map((source, index) => source.read(texts[index]));
    const same = current.every((version) => version === current[0]);
    const keep =
        same && FORK_VERSION.test(current[0] ?? '') && current[0].startsWith(`${core}+extended.`);
    const version = keep ? current[0] : `${core}+extended.1`;
    if (!keep) {
        versionSources.forEach((source, index) =>
            writeFileSync(join(root, source.file), source.write(texts[index], version)),
        );
    }

    const check = spawnSync(
        process.execPath,
        [fileURLToPath(new URL('./check-fork-identity.mjs', import.meta.url))],
        {
            cwd: root,
            stdio: 'inherit',
        },
    );
    if (check.status !== 0) {
        fail(
            `The identity check failed.${keep ? '' : ' The version files were edited but not committed.'} Fix the problems above and run finish again.`,
        );
    }
    if (keep) console.log(`Version is already ${version}. Nothing to commit.`);
    else {
        const files = versionSources.map((source) => source.file);
        git(root, ['add', '--', ...files]);
        git(root, [
            'commit',
            '-m',
            `chore: set Tabularis Extended version to ${version}`,
            '--',
            ...files,
        ]);
        console.log(`Committed version ${version}.`);
    }
    console.log(`\nNext, verify the merge:
  cd src-tauri && cargo test --lib
  pnpm exec vitest run --config packages/web-ui/vitest.config.ts
  pnpm typecheck
Then push the branch yourself and open a pull request.`);
}

function sync(root, requested) {
    assertReady(root, {untracked: true});
    ensureUpstream(root);
    fetchUpstreamTags(root);
    const tags = upstreamStableTags(root);
    const tag = requested ? (requested.startsWith('v') ? requested : `v${requested}`) : tags.at(-1);
    if (!tag || !tags.includes(tag))
        fail(
            `${requested ?? '(none)'} is not a stable upstream release tag. Known newest: ${tags.at(-1) ?? 'none'}.`,
        );
    if (isAncestor(root, tag, 'HEAD')) {
        console.log(`${tag} is already merged into HEAD. Nothing to do.`);
        return;
    }

    const base = newestMergedStableTag(root, new Set(tags));
    const branch = `sync/upstream-${tag}`;
    if (
        git(root, ['show-ref', '--verify', '--quiet', `refs/heads/${branch}`], {allowFail: true})
            .status === 0
    ) {
        fail(
            `Branch ${branch} already exists. Switch to it, or remove it yourself, then run sync again.`,
        );
    }
    const origin = git(root, ['branch', '--show-current']).stdout.trim();
    git(root, ['switch', '-c', branch]);
    git(root, ['config', 'rerere.enabled', 'true']);
    git(root, ['config', 'rerere.autoupdate', 'true']);
    console.log(`Created ${branch}${origin ? ` from ${origin}` : ''}. Merging ${tag}...`);

    const message = `chore: merge upstream Tabularis ${tag}`;
    const merge = git(
        root,
        ['-c', 'merge.conflictStyle=diff3', 'merge', '--no-ff', '--no-edit', '-m', message, tag],
        {allowFail: true},
    );
    if (merge.status === 0) {
        finish(root);
        return;
    }
    if (inProgress(root) !== 'merge') {
        fail(
            `The merge could not start:\n${(merge.stderr || merge.stdout).trim()}\nYou are on ${branch}. Go back with: git switch ${origin || '-'}`,
        );
    }

    settleForkOwnedConflicts(root);
    settleVersionConflicts(root, base?.slice(1), tag.slice(1));
    const left = unmergedFiles(root);
    if (left.length === 0) {
        git(root, ['commit', '--no-edit', '--cleanup=strip']);
        console.log('Only version and fork-owned file conflicts were found and they are settled.');
        finish(root);
        return;
    }
    console.error(
        `Merge stopped with ${left.length} conflicted file(s):\n${left.map((file) => `  ${file}`).join('\n')}`,
    );
    console.error(`
The merge is still in progress on ${branch}. Next:
  1. Edit each file above and remove the conflict markers.
  2. git add <files>
  3. git merge --continue
  4. node scripts/upstream-sync.mjs finish
To give up: git merge --abort`);
    process.exitCode = 1;
}

function main() {
    const [command, ...args] = process.argv.slice(2);
    if (!['status', 'sync', 'finish'].includes(command ?? '')) {
        console.error(USAGE);
        process.exit(command === 'help' || command === '--help' ? 0 : 2);
    }
    const root = git(process.cwd(), ['rev-parse', '--show-toplevel']).stdout.trim();
    if (command === 'status') {
        const status = collectStatus(root);
        if (args.includes('--json')) console.log(JSON.stringify(status, null, 2));
        else printStatus(status);
    } else if (command === 'sync') sync(root, args[0]);
    else finish(root);
}

try {
    main();
} catch (error) {
    console.error(error.message);
    process.exitCode = 1;
}
