---
name: commit
description: Use when the user asks to commit changes in this repo. Runs format/test gates, audits for forbidden patterns, drafts a message in repo style, and stages specific files. Never auto-commits without explicit user approval.
---

# Committing changes in fuchsia

Only commit when the user explicitly asks for it. Never amend; always create a new commit. Always include a `Co-Authored-By` trailer (see step 5). `AGENTS.md` is explicit: do not auto-commit or push without user approval.

## 1. Survey the change

Run in parallel:

- `git status` (no `-uall` flag)
- `git diff` (staged + unstaged)
- `git log --oneline -10` (to match existing message style)

Read the diff fully before drafting anything. The message describes *why*, not just *what*, so you need to understand the change.

## 2. Pre-flight gates

These must all pass before proposing a commit. Run in parallel where possible.

- `cargo fmt` — applies formatting; if it changes files, include them in the commit
- `cargo test` — all tests must pass
- New functionality must ship with tests. If the diff adds public behavior without a test, stop and surface that to the user before committing.

## 3. Production-code audit

Grep the diff (not the whole repo) for forbidden patterns in non-test code. These mirror `AGENTS.md`:

- `unwrap()`, `expect(` — propagate errors with `?` or `map_err` instead
- `.ok()` discarding errors — `sort_by` closures are the only exception
- `clone()` — must have a justification in the message or be removed

Test code (`#[cfg(test)]` blocks, `tests/` dirs, `*_test.rs`, `*_tests.rs`) is exempt. If you find a violation in production code, fix it or flag it before committing — do not commit through it.

## 4. Draft the message

Match the style of recent commits (`git log --oneline -20`). Conventions:

- **Subject**: imperative mood, under ~72 chars, no trailing period. Start with a verb: `Add`, `Fix`, `Update`, `Refactor`, `Remove`, `Rename`, `Unify`, etc.
- **Body** (only when the change needs explanation): blank line, then bullets or short paragraphs covering *why* and any non-obvious mechanics. Wrap at ~72 chars.
- Subject-only is fine for small, self-explanatory changes (see `bd1334f`, `9906642`).
- Multi-area changes get bullets per area (see `ff28f6c` for the dash-bullet pattern, `dfbacf4` for grouped sections like "New crates:" / "Removed crates:").
- **Always** end with a blank line followed by a `Co-Authored-By` trailer naming the model currently running this session. Use the model's display name as given in the environment context (e.g. `Opus 4.7 (1M context)`, `Sonnet 4.6`, `Haiku 4.5`). Format:

  ```
  Co-Authored-By: Claude <display-name> <noreply@anthropic.com>
  ```

  Do **not** hard-code a specific model — read the current one from the environment each time.

## 5. Stage and commit

Stage files by name, not `git add .` or `-A` — protects against accidentally including `.env`, scratch files, or unrelated worktrees.

```bash
git add path/to/file1.rs path/to/file2.rs
```

For multi-line messages, use a HEREDOC:

```bash
git commit -m "$(cat <<'EOF'
Subject line under 72 chars

Body paragraph or bullets explaining why. Wrap at ~72 chars.

Co-Authored-By: Claude <model-name> <noreply@anthropic.com>
EOF
)"
```

For subject-only commits, use `-m` twice so the trailer lands in its own paragraph:

```bash
git commit -m "Subject line" -m "Co-Authored-By: Claude <model-name> <noreply@anthropic.com>"
```

After commit, run `git status` to confirm a clean tree (or to verify the intended unstaged files were left out).

## 6. Do not push

Never `git push` unless the user explicitly says so. `AGENTS.md` is explicit on this.

## Failure modes to avoid

- Committing through a failing `cargo test` "because the failure looks unrelated" — confirm with the user first.
- Amending a previous commit instead of creating a new one.
- Using `--no-verify` to skip hooks.
- Staging files you didn't read in the diff.
- Drafting a message before reading the actual changes (results in generic "Update X" messages that don't explain why).
