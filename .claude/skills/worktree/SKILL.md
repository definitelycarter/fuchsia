---
name: worktree
description: Use when starting (or cleaning up) an isolated line of work — typically implementing an RFC — in a git worktree under .claude/worktrees/. Covers creating the worktree and branch, the naming convention, building/testing inside it, folding the branch back to main, and removing it.
---

# Worktrees in fuchsia

A non-trivial effort — usually implementing an accepted [[rfc]] — is built in its
own **git worktree**: a second working tree of this repo, on its own branch,
checked out in a separate directory. `main` stays clean, several efforts can be in
flight at once, and each has an isolated build. This mirrors the cadence in the
sibling `slate` repo.

Worktrees are an isolation tool, not a requirement for every change. A quick fix on
`main` (or a normal feature branch) doesn't need one. Reach for a worktree when the
work is long-running, parallel to other work, or the implementation arm of an RFC.

## Where they live

- `.claude/worktrees/<name>/` — one directory per effort.
- This path is **gitignored** (`/.claude/worktrees/` in `.gitignore`), so the
  checkout never gets committed into `main`. The `.claude/skills/` tree *is*
  tracked — only `worktrees/` is ignored.
- `<name>` matches the effort: for an RFC, use its slug
  (`.claude/worktrees/per-actor-retry-policy/`).

## Create

From the repo root, branch off `main` and add the worktree in one command:

```bash
git worktree add .claude/worktrees/<name> -b <name>
```

- Branch name = directory name = the RFC/effort slug. Keep them identical so the
  link between a branch, its worktree, and its RFC is obvious. (Slate also uses a
  `worktree-<slug>` / `test/<slug>` prefix on occasion — plain `<slug>` is the
  default here.)
- To branch from somewhere other than `main`, append the start point:
  `git worktree add .claude/worktrees/<name> -b <name> <start-point>`.
- `git worktree list` shows every checkout and the branch it's on.

## Work inside it

Treat `.claude/worktrees/<name>/` as a full, independent checkout:

```bash
cd .claude/worktrees/<name>
cargo build --workspace
cargo test --workspace
```

- It has its **own `target/`** — builds don't share artifacts with the root
  checkout, so the first build is cold. That's the cost of isolation.
- Commit on the branch as normal (the [[commit]] skill applies unchanged: same
  gates, same forbidden-pattern audit, no auto-commit without approval).
- The same `AGENTS.md` rules apply — it's the same repo.

## Fold back and remove

When the branch is ready, integrate it into `main` from the **root** checkout, then
tear the worktree down:

```bash
# from the repo root (not inside the worktree)
git merge <name>          # or rebase the branch onto main, however you integrate
git worktree remove .claude/worktrees/<name>
git branch -d <name>      # delete the now-merged branch
```

- `git worktree remove` refuses if the tree has uncommitted changes — commit or
  stash first (or `--force` if you mean to discard).
- A worktree whose directory was deleted by hand leaves a stale registration; run
  `git worktree prune` to clean it up.
- Update the RFC's status callout and the roadmap as part of landing the work — see
  [[rfc]]. The worktree is disposable; the RFC and the merged history are the record.

## Checklist

- [ ] Worktree under `.claude/worktrees/<slug>/`, branch named the same slug.
- [ ] `/.claude/worktrees/` is in `.gitignore` (it is — don't commit the checkout).
- [ ] Built and tested inside the worktree before integrating.
- [ ] Branch merged/rebased to `main`, worktree removed, branch deleted.
- [ ] RFC callout + roadmap updated as the work landed.
