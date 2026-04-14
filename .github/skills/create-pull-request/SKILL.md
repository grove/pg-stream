---
name: create-pull-request
description: "Create or update a pull request for pg_trickle. Use when: opening a new PR, fixing a garbled PR description, updating an existing PR body, or preparing a PR after finishing a feature or fix. Handles the safe body-file workflow to avoid Unicode corruption."
argument-hint: "Branch name or short description of the change (e.g. fix/scheduler-race)"
---

# Create Pull Request

Safe, Unicode-correct workflow for creating or updating GitHub pull requests in
the pg_trickle repository.  **Never use shell heredocs or `echo` for PR
bodies** — they silently corrupt Unicode (math symbols, em-dashes, etc.) and
can pick up stale content from a previous session.

## When to Use

- Opening a new pull request after completing a fix, feature, or refactor
- Repairing a garbled PR description (`gh pr edit`)
- Updating the PR body after new commits

## Prerequisites

- `gh` CLI authenticated (`gh auth status`)
- Working tree is clean or changes are committed
- Current branch is **not** `main` (never create a new branch unless the
  current branch is `main` — see AGENTS.md)

## Procedure

### Step 1 — Gather Context

Before writing the PR body, collect:

1. `git log main..HEAD --oneline` — commits included in the PR
2. `git diff main --stat` — files changed
3. Relevant plans or issues referenced by the branch name or commit messages
4. Check for an existing PR: `gh pr list --head <branch>`

### Step 2 — Choose a Title

Format: `<type>(<scope>): <short imperative summary>` (conventional commits)

| Type | When |
|------|------|
| `feat` | New user-visible capability |
| `fix` | Bug fix |
| `perf` | Performance improvement |
| `refactor` | Internal restructure, no behaviour change |
| `test` | Tests only |
| `docs` | Documentation only |
| `chore` | Build / CI / tooling |

Keep the title under 72 characters.

### Step 3 — Write the PR Description

Structure the body using this template:

```markdown
## Summary

<!-- 2-4 sentences: what changed and why -->

## Changes

<!-- Bullet list of the most important changes -->
- …

## Testing

<!-- How the change was verified -->
- `just test-unit` — passes
- `just test-integration` — passes
- `just test-e2e` — passes (include relevant test names if targeted)

## Notes

<!-- Optional: migration steps, rollback plan, known limitations, follow-up issues -->
```

### Step 4 — Write Description Using `create_file` Tool

**CRITICAL**: Use the `create_file` tool — never `echo` or a shell heredoc.

1. Delete any stale file at the target path first:
   ```bash
   rm -f /tmp/pr_<TICKETNAME>.md
   ```

2. Use **`create_file`** to write the body to `/tmp/pr_<TICKETNAME>.md`.
   The file is written in UTF-8 and read directly by `gh --body-file`, so
   Unicode characters (math symbols, em-dashes, arrows) are safe.

3. Verify the file is clean before using it:
   ```bash
   python3 -c "
   with open('/tmp/pr_<TICKETNAME>.md') as f:
       body = f.read()
   print('lines:', body.count(chr(10)))
   print('ok:', '####' not in body)
   print(body[:120])
   "
   ```

### Step 5 — Create or Update the PR

**New PR:**
```bash
gh pr create \
  --title "<type>(<scope>): <summary>" \
  --body-file /tmp/pr_<TICKETNAME>.md \
  --base main
```

**Fix garbled description on existing PR:**
```bash
gh pr edit <number> --body-file /tmp/pr_<TICKETNAME>.md
```

### Step 6 — Verify the Live PR Body

```bash
gh pr view <number> --json body --jq '.body' | head -20
```

Confirm the first heading and a Unicode character (if any) render correctly.
If the body is garbled, re-run Step 3–5.

## Branch Rules

- **Never create a new git branch unless the current branch is `main`.**
- Branch names follow `<type>/<short-slug>`, e.g. `fix/scheduler-race`,
  `feat/window-function-diff`.

## Checklist Before Opening the PR

- [ ] `just fmt && just lint` passes with zero warnings
- [ ] Relevant test tier passes (`just test-unit` / `just test-integration` / `just test-e2e`)
- [ ] No `unwrap()` / `panic!()` in non-test code paths
- [ ] All `unsafe` blocks have `// SAFETY:` comments
- [ ] PR title follows conventional commit format
- [ ] PR body written via `create_file` tool (not heredoc / echo)
- [ ] Live PR body verified with `gh pr view`
