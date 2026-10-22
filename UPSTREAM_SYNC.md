# Syncing with upstream Tabularis

This fork is upstream `TabularisDB/tabularis` plus our commits. To get a new upstream release, we merge its
`vX.Y.Z` tag. We never rebase. The script `scripts/upstream-sync.mjs` does the repeatable parts, and
`scripts/check-fork-identity.mjs` makes sure the fork keeps its own name, identifiers, and version.

## When to sync

- After GitHub opens an issue titled `Upstream Tabularis vX.Y.Z is available` (see "Scheduled watch" below).
- Before you start a larger piece of work, so you build on current code.

Only stable `vX.Y.Z` tags count. `nightly-*` tags and pre-releases such as `v1.2.0-rc1` are ignored. Do not
give our own releases a plain `vX.Y.Z` tag, because the scripts read those tags as upstream releases.

## The three commands

Run them from the repository root, on the branch you want to update, with a clean work tree.

| Command | What it does |
| --- | --- |
| `node scripts/upstream-sync.mjs status [--json]` | Adds the `upstream` remote if it is missing and fetches the `v*` tags. Shows the newest stable tag, the upstream release already merged into `HEAD`, the commits behind, and a conflict preview. It changes nothing in your work tree or index. |
| `node scripts/upstream-sync.mjs sync [<tag>]` | Refuses a dirty tree or a merge in progress. Creates `sync/upstream-<tag>` from the current branch, turns on `rerere`, and merges the tag with `--no-ff`. The default tag is the newest stable one. A clean merge runs `finish` for you. |
| `node scripts/upstream-sync.mjs finish` | Sets the version to `<X.Y.Z>+extended.1` in `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and `src/version.ts`. Then it runs the identity check and commits only those files. |

If a local `vX.Y.Z` tag points somewhere else than the upstream tag with the same name, `status` and `sync` stop
and name the tags. Fix or delete the wrong local tag yourself; the scripts never overwrite tags.

The scripts never push, never force, and never delete branches or tags. The remote URL can be changed with the
`UPSTREAM_URL` environment variable. The `origin` remote is never touched.

If the version is already `<X.Y.Z>+extended.N`, `finish` keeps it. Raise `N` by hand for a second fork build of the
same upstream release.

## If the merge stops on conflicts

1. `sync` lists the conflicted files and leaves the merge in progress.
2. Edit each file and remove the conflict markers.
3. Run `git add <files>`, then `git merge --continue`.
4. Run `node scripts/upstream-sync.mjs finish`.

To give up, run `git merge --abort`. The `sync/upstream-<tag>` branch stays, so delete it yourself if you want.

## How the preview and rerere help

- The preview in `status` uses `git merge-tree --write-tree`. It builds the merge in memory, so your work tree and
  index stay untouched. You see the conflicting files before you start.
- Upstream bumps the version lines that the fork also edits, so those files would always conflict. When upstream
  changed nothing in a conflicting block except the application version line (for example `0.27.0` to `0.28.0`),
  `sync` keeps the fork side and `finish` sets the new version. Any other change in that block, such as a dependency
  version, stays a conflict for you to resolve. `status` shows the settled files as version-only conflicts.
- `rerere` is turned on in this repository's local git config. Git records how you resolved a conflict and reuses
  the answer when the same conflict appears again. Because `rerere.autoupdate` is on, reused answers are staged.
  Check them in `git diff --cached` before you continue.

## Conflict hotspots

These files carry the most fork changes against `v0.27.0`, so they are the most likely to conflict. The list is
based on how many lines changed, not on a tested merge.

- `src-tauri/src/commands.rs` (about 1,400 changed lines)
- `src-tauri/src/plugins/driver.rs`
- `src-tauri/src/persistence.rs`
- `src-tauri/src/ai_activity.rs`
- `src-tauri/src/mcp/mod.rs`
- `src/components/modals/NewConnectionModal.tsx` and its test
- `src/components/modals/AiApprovalModal.tsx`
- `src/pages/Editor.tsx`

Rule: keep fork logic in new files and keep edits to upstream files as small hooks. A one-line call into a new
fork file is easy to merge. A rewritten function in an upstream file conflicts on every release.

## After the merge

Run the checks that `finish` prints, then push the branch yourself and open a pull request:

```
cd src-tauri && cargo test --lib
pnpm exec vitest run
pnpm typecheck
```

`node scripts/check-fork-identity.mjs` can be run at any time. It reads local tags and does not fetch. It fails when:

- the five version files differ or are not shaped `X.Y.Z+extended.N`;
- the package name, product name, bundle identifier, updater settings, or deep-link scheme changed;
- `APP_DIR_NAME`, `SERVICE_NAME`, `DEEP_LINK_SCHEME`, or `MCP_SERVER_KEY` is not `tabularis-extended`, the official
  update feed is enabled, or the Linux desktop file is not `tabularis-extended.desktop`;
- a line the fork added since the merged upstream tag names the other IDE product. Lines added by upstream are
  allowed.

`scripts/sync-version.js` is not used here. It also rewrites README release links, and the fork keeps those
identical to upstream.

## Scheduled watch and checks

- `.github/workflows/upstream-watch.yml` runs every Monday and on demand. When a newer stable upstream tag is not
  merged, it creates or updates one open issue with the commits behind, the predicted conflicts, and the commands
  above. It never pushes, opens pull requests, or merges.
- `.github/workflows/extended-checks.yml` runs the identity check on every push and pull request.
- Both run only in `hei5enbug/tabularis-extended`.
- GitHub runs scheduled workflows only from the default branch. The weekly watch starts after this work reaches
  `main`. The manual `Run workflow` button also needs the file on the default branch. Until then, run `status`
  locally.

## The PostgreSQL extension

`extensions/postgresql` is a git subtree of `TabularisDB/tabularis-postgresql-plugin`. To take plugin changes, run
this on a clean tree, with the same options that were used for the original `git subtree add`:

```
git subtree pull --prefix extensions/postgresql https://github.com/TabularisDB/tabularis-postgresql-plugin <ref>
```

Replace `<ref>` with a plugin branch or tag. Do this on its own branch, separate from a host sync.
