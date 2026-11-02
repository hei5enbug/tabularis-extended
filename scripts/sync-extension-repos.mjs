#!/usr/bin/env node
import {spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';

const TRAILER_KEY = 'Synced-From';
const EMPTY_TREE = '4b825dc642cb6eb9a060e54bf8d69288fbee4904';
const USAGE = `Usage:
  node scripts/sync-extension-repos.mjs catch-up --folder <path> --target <dir> [options]
  node scripts/sync-extension-repos.mjs replay   --folder <path> --target <dir> [options]

  catch-up  copy the folder at <head> over the target checkout and commit it once with a
            ${TRAILER_KEY} trailer; files that exist only in the target are kept
  replay    commit every first-parent source commit after the last ${TRAILER_KEY} trailer that
            changes the folder, one target commit each, with the original message and author

Options:
  --folder <path>        source folder relative to the source root, such as extensions/azure
  --target <dir>         checkout of the target repository; commits land on its current branch
  --source <dir>         source repository root (default: current directory)
  --source-repo <name>   owner/name written in the trailer (default: hei5enbug/tabularis-extended)
  --head <rev>           last source revision to sync (default: HEAD)

Neither mode pushes.`;

function fail(message) {
    throw new Error(message);
}

function git(cwd, args, {allowFail = false, env, input} = {}) {
    const result = spawnSync('git', args, {
        cwd,
        encoding: 'utf8',
        env: env ? {...process.env, ...env} : process.env,
        input,
        maxBuffer: 256 * 1024 * 1024,
    });
    if (result.error) fail(`Cannot run git: ${result.error.message}`);
    if (result.status !== 0 && !allowFail) {
        fail(`git ${args.join(' ')} failed in ${cwd}:\n${result.stderr.trim()}`);
    }
    return result;
}

function parseArgs(argv) {
    const [mode, ...rest] = argv;
    const options = {mode, source: '.', sourceRepo: 'hei5enbug/tabularis-extended', head: 'HEAD'};
    const names = {
        '--folder': 'folder',
        '--target': 'target',
        '--source': 'source',
        '--source-repo': 'sourceRepo',
        '--head': 'head',
    };
    for (let index = 0; index < rest.length; index += 2) {
        const key = names[rest[index]];
        const value = rest[index + 1];
        if (!key || value === undefined || value.startsWith('--')) {
            fail(`Unknown or incomplete option '${rest[index]}'.\n\n${USAGE}`);
        }
        options[key] = value;
    }
    if (mode !== 'catch-up' && mode !== 'replay') fail(USAGE);
    if (!options.folder || !options.target) fail(`--folder and --target are required.\n\n${USAGE}`);
    if (!/^[\w.-]+\/[\w.-]+$/.test(options.sourceRepo))
        fail(`--source-repo must look like owner/name.`);
    return {
        ...options,
        folder: options.folder.replace(/^\.?\/+|\/+$/g, ''),
        source: resolve(options.source),
        target: resolve(options.target),
    };
}

function resolveCommit(root, revision) {
    const result = git(root, ['rev-parse', '--verify', '--quiet', `${revision}^{commit}`], {
        allowFail: true,
    });
    if (result.status !== 0) fail(`'${revision}' is not a commit in ${root}.`);
    return result.stdout.trim();
}

function commitExists(root, sha) {
    return git(root, ['cat-file', '-e', `${sha}^{commit}`], {allowFail: true}).status === 0;
}

function isAncestor(root, ancestor, descendant) {
    return (
        git(root, ['merge-base', '--is-ancestor', ancestor, descendant], {allowFail: true})
            .status === 0
    );
}

function checkFolder(options, head) {
    const result = git(options.source, ['cat-file', '-t', `${head}:${options.folder}`], {
        allowFail: true,
    });
    if (result.stdout.trim() !== 'tree')
        fail(`'${options.folder}' is not a folder at ${head} in ${options.source}.`);
}

function checkCleanTarget(target) {
    resolveCommit(target, 'HEAD');
    if (git(target, ['status', '--porcelain']).stdout.trim()) {
        fail(
            `The target checkout ${target} has uncommitted changes. Commit or discard them first.`,
        );
    }
}

function lastSynced(options) {
    const prefix = `${options.sourceRepo}@`;
    const perCommit = git(options.target, [
        'log',
        `--format=%(trailers:key=${TRAILER_KEY},valueonly,separator=%x1F)%x1E`,
    ]).stdout.split('\x1E');
    const value = perCommit
        .map((values) => values.split('\x1F').map((line) => line.trim()))
        .map((values) => values.filter((line) => line.startsWith(prefix)).at(-1))
        .find(Boolean);
    if (!value) return null;
    const sha = value.slice(prefix.length);
    if (!/^[0-9a-f]{40}$/.test(sha))
        fail(`The target's newest ${TRAILER_KEY} trailer '${value}' has no full commit id.`);
    return sha;
}

function trailer(options, sha) {
    return `${TRAILER_KEY}: ${options.sourceRepo}@${sha}`;
}

function changedFolderCommits(options, from, head) {
    const range = git(options.source, [
        'rev-list',
        '--reverse',
        '--first-parent',
        `${from}..${head}`,
    ]).stdout;
    return range
        .split('\n')
        .filter(Boolean)
        .filter((sha) => {
            const diff = git(
                options.source,
                ['diff', '--quiet', firstParent(options.source, sha), sha, '--', options.folder],
                {
                    allowFail: true,
                },
            );
            return diff.status === 1;
        });
}

function firstParent(root, sha) {
    const result = git(root, ['rev-parse', '--verify', '--quiet', `${sha}^1`], {allowFail: true});
    return result.status === 0 ? result.stdout.trim() : EMPTY_TREE;
}

function folderPatch(options, sha) {
    return git(options.source, [
        'diff',
        '--binary',
        '--full-index',
        '--no-renames',
        '--no-ext-diff',
        '--no-textconv',
        `--relative=${options.folder}/`,
        firstParent(options.source, sha),
        sha,
        '--',
        `${options.folder}/`,
    ]).stdout;
}

function sourceCommitInfo(root, sha) {
    const [name, email, date, ...message] = git(root, [
        'log',
        '-1',
        '--format=%an%x00%ae%x00%aI%x00%B',
        sha,
    ]).stdout.split('\0');
    return {name, email, date, message: message.join('\0')};
}

function withMessageFile(message, action) {
    const directory = mkdtempSync(join(tmpdir(), 'sync-extension-repos-'));
    try {
        const file = join(directory, 'message');
        writeFileSync(file, message);
        return action(file);
    } finally {
        rmSync(directory, {recursive: true, force: true});
    }
}

function commit(target, message, trailerLine, {author, allowEmpty = false} = {}) {
    const env = author
        ? {
              GIT_AUTHOR_NAME: author.name,
              GIT_AUTHOR_EMAIL: author.email,
              GIT_AUTHOR_DATE: author.date,
          }
        : undefined;
    withMessageFile(message, (file) =>
        git(
            target,
            [
                'commit',
                '--quiet',
                '--no-verify',
                '--cleanup=whitespace',
                ...(allowEmpty ? ['--allow-empty'] : []),
                '-F',
                file,
                '--trailer',
                trailerLine,
            ],
            {env},
        ),
    );
}

function replayCommit(options, sha) {
    const patch = folderPatch(options, sha);
    const check = git(options.target, ['apply', '--check', '--index', '--binary', '-'], {
        allowFail: true,
        input: patch,
    });
    if (check.status !== 0) {
        fail(
            `Source commit ${sha} does not apply cleanly to ${options.target}; nothing after it was replayed.\n` +
                `${check.stderr.trim()}\n` +
                `The target has drifted from ${options.folder}. Fix the target by hand, or run catch-up to copy ` +
                `the folder over it again, then rerun replay.`,
        );
    }
    git(options.target, ['apply', '--index', '--binary', '-'], {input: patch});
    const info = sourceCommitInfo(options.source, sha);
    commit(options.target, info.message, trailer(options, sha), {author: info, allowEmpty: true});
    return info;
}

function replay(options) {
    const head = resolveCommit(options.source, options.head);
    checkCleanTarget(options.target);
    const last = lastSynced(options);
    if (!last) {
        fail(
            `The target has no '${TRAILER_KEY}: ${options.sourceRepo}@<sha>' trailer yet. Run catch-up once first.`,
        );
    }
    if (!commitExists(options.source, last)) {
        fail(
            `The last synced commit ${last} is not in ${options.source}. Fetch the full history, or run catch-up.`,
        );
    }
    if (isAncestor(options.source, head, last)) {
        console.log(
            `The target is already synced to ${last.slice(0, 12)}, which includes ${head.slice(0, 12)}.`,
        );
        return 0;
    }
    if (!isAncestor(options.source, last, head)) {
        fail(
            `The last synced commit ${last} is not an ancestor of ${head}. Run catch-up to start again from ${head}.`,
        );
    }
    const commits = changedFolderCommits(options, last, head);
    for (const sha of commits) {
        const info = replayCommit(options, sha);
        console.log(`replayed ${sha.slice(0, 12)} ${info.message.split('\n')[0]}`);
    }
    console.log(
        commits.length
            ? `Replayed ${commits.length} commit(s).`
            : `Nothing to replay after ${last.slice(0, 12)}.`,
    );
    return commits.length;
}

function folderEntries(options, head) {
    return git(options.source, [
        'ls-tree',
        '-r',
        '-z',
        '--full-tree',
        head,
        '--',
        `${options.folder}/`,
    ])
        .stdout.split('\0')
        .filter(Boolean)
        .map((line) => {
            const [meta, path] = [
                line.slice(0, line.indexOf('\t')),
                line.slice(line.indexOf('\t') + 1),
            ];
            const [mode, type, sha] = meta.split(' ');
            return {mode, type, sha, path: path.slice(options.folder.length + 1)};
        });
}

function copyBlob(options, sha) {
    const blob = spawnSync('git', ['cat-file', 'blob', sha], {
        cwd: options.source,
        maxBuffer: 256 * 1024 * 1024,
    });
    if (blob.error || blob.status !== 0) {
        fail(
            `Cannot read blob ${sha} from ${options.source}: ${blob.error?.message ?? blob.stderr}`,
        );
    }
    const written = git(options.target, ['hash-object', '-w', '--no-filters', '--stdin'], {
        input: blob.stdout,
    }).stdout.trim();
    if (written !== sha) fail(`Blob ${sha} was stored in the target as ${written}.`);
}

function copyFolder(options, entries) {
    const blobs = entries.filter((entry) => entry.type === 'blob');
    for (const entry of blobs) copyBlob(options, entry.sha);
    const index = entries.map(
        (entry) => `${entry.mode} ${entry.type} ${entry.sha}\t${entry.path}\0`,
    );
    git(options.target, ['update-index', '-z', '--index-info'], {input: index.join('')});
    git(options.target, ['checkout-index', '--force', '-z', '--stdin'], {
        input: blobs.map((entry) => `${entry.path}\0`).join(''),
    });
}

function deletedSince(options, last, head) {
    if (!last) return [];
    return git(options.source, [
        'diff',
        '--name-only',
        '-z',
        '--no-renames',
        '--diff-filter=D',
        `--relative=${options.folder}/`,
        last,
        head,
        '--',
        `${options.folder}/`,
    ])
        .stdout.split('\0')
        .filter(Boolean);
}

function targetOnlyFiles(options, files) {
    const kept = new Set(files);
    return git(options.target, ['ls-files', '-z'])
        .stdout.split('\0')
        .filter((file) => file && !kept.has(file));
}

function catchUp(options) {
    const head = resolveCommit(options.source, options.head);
    checkFolder(options, head);
    checkCleanTarget(options.target);
    const recorded = lastSynced(options);
    const last = recorded && commitExists(options.source, recorded) ? recorded : null;
    if (recorded && !last) {
        console.log(
            `The last synced commit ${recorded} is not in the source, so no deletions are carried over.`,
        );
    }
    if (last && isAncestor(options.source, head, last)) {
        console.log(
            `The target is already synced to ${last.slice(0, 12)}, which includes ${head.slice(0, 12)}.`,
        );
        return 0;
    }
    const entries = folderEntries(options, head);
    copyFolder(options, entries);
    const files = entries.map((entry) => entry.path);
    const deleted = deletedSince(options, last, head);
    if (deleted.length) {
        git(
            options.target,
            [
                '--literal-pathspecs',
                'rm',
                '--quiet',
                '--ignore-unmatch',
                '--pathspec-from-file=-',
                '--pathspec-file-nul',
            ],
            {
                input: `${deleted.join('\0')}\0`,
            },
        );
    }
    const name = options.sourceRepo.split('/')[1];
    commit(
        options.target,
        `chore: sync from ${name}@${head.slice(0, 12)}\n`,
        trailer(options, head),
        {allowEmpty: true},
    );
    const kept = targetOnlyFiles(options, files);
    console.log(
        `Copied ${files.length} file(s) from ${options.folder} at ${head.slice(0, 12)}; deleted ${deleted.length}.`,
    );
    console.log(
        `Kept ${kept.length} target-only file(s). Delete any that are stale copies of removed folder files:`,
    );
    for (const file of kept) console.log(`  ${file}`);
    return 1;
}

try {
    const options = parseArgs(process.argv.slice(2));
    if (options.mode === 'catch-up') catchUp(options);
    else replay(options);
} catch (error) {
    console.error(error.message);
    process.exit(1);
}
