---
name: Bug report
about: Something is broken or behaves unexpectedly
title: "bug: "
labels: ["bug", "needs-triage"]
assignees: ""
---

## Description

<!-- A clear description of what is broken and what you expected instead. -->

## Reproduction

```sql
-- Minimal SQL to reproduce the issue
```

## Environment

| Item | Value |
|------|-------|
| pg_trickle version | |
| PostgreSQL version | |
| OS / platform | |
| Deployment | bare metal / Docker / CNPG / other |
| CDC mode | trigger / auto / wal |
| Refresh mode | DIFFERENTIAL / FULL |

## Observed behaviour

<!-- Paste error message, wrong query result, or describe the misbehaviour. -->

## Expected behaviour

<!-- What should have happened? -->

## Logs

<details>
<summary>PostgreSQL log output (if relevant)</summary>

```
paste logs here
```

</details>

## Additional context

<!-- Anything else — schema, data volumes, concurrent activity, relevant GUC values. -->
