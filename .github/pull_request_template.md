## Summary

<!-- One or two sentences describing what this PR does. -->

Fixes # <!-- issue number, if applicable -->

## Changes

<!-- Bullet list of what changed and why. -->

-

## Testing

<!-- How was this tested? Which test tiers were run? -->

- [ ] `just test-unit` passes
- [ ] `just test-integration` passes (if DB-facing code changed)
- [ ] `just test-e2e` passes (if SQL API or CDC changed)
- [ ] New tests added for the changed behaviour

## Code review checklist

- [ ] No `unwrap()` / `panic!()` in non-test code
- [ ] All `unsafe` blocks have `// SAFETY:` comments
- [ ] New SQL functions use `#[pg_extern(schema = "pgstream")]`
- [ ] `just fmt && just lint` passes with zero warnings
- [ ] Error messages include context (table name, query fragment, etc.)
- [ ] CHANGELOG.md updated under `## [Unreleased]` if user-visible

## Notes for reviewer

<!-- Anything the reviewer should pay particular attention to, open questions, or follow-up work. -->
