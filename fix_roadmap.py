with open("ROADMAP.md", "r") as f:
    text = f.read()

text = text.replace("| B2-1 | **LIMIT / OFFSET / ORDER BY.** Top-K queries evaluated directly within the DVM engine. | 2–3 wk | ⬜ Todo |", "| B2-1 | **LIMIT / OFFSET / ORDER BY.** Top-K queries evaluated directly within the DVM engine. | 2–3 wk | ✅ Done |")
text = text.replace("| B2-2 | **LATERAL Joins.** Expanding the parser and DVM diff engine to handle LATERAL subqueries. | 2 wk | ⬜ Todo |", "| B2-2 | **LATERAL Joins.** Expanding the parser and DVM diff engine to handle LATERAL subqueries. | 2 wk | ✅ Done |")
text = text.replace("| B2-3 | **View Inlining.** Allow stream tables to query standard PostgreSQL views natively. | 1-2 wk | ⬜ Todo |", "| B2-3 | **View Inlining.** Allow stream tables to query standard PostgreSQL views natively. | 1-2 wk | ✅ Done |")
text = text.replace("| B2-4 | **Synchronous / Transactional IVM.** Evaluating DVM diffs synchronously in the same transaction as the DML. | 3 wk | ⬜ Todo |", "| B2-4 | **Synchronous / Transactional IVM.** Evaluatingtext = text.replacnoutext = text.replace("| B2-4 | **Synchronous / Transactional IVM.** Evaluating DVM diffs synchronously in the same transaction as the DML. | 3 wk | ⬜ Todo |", "| B2-4 | **Synchronous / Transactional IVM.** Evaluatingtext = text.replacnoutext = text.replace("| B2-.** Improving engine consistency models when joining multiple tables. | 2 wk | 🟡 In Progress |")
text = text.replace("| B2-6 | **Non-Determinism Guarding.** Better handling or rejection of non-deterministic functions (`random()`, `now()`). | 1 wk | ⬜ Todo |", "text = text.replace("| B2-6 | **Non-Determinism Guarding.** Better handling or rejection of non-deterministic functions (`random()`, `now()`). | 1 wk | ⬜ Todo |", "text = text.replace("| B2-6 | **Non-Determinism Guarding.** Better handling or rejectionLIMIT/OFFSET/ORDER BY) support")
text = text.replace("- [ ] B2-2: LATERAL Joins support", "- [x] B2-2: LATERAL Joins support")
text = text.replace("- [ ] B2-3: View Inlining support", "- [x] B2-3: View Inlining support")
text = text.replace("- [ ] B2-4: Synchronous / Transactional IVM mode", "- [x] B2-4: Synchronous / Transactional IVM mode")
text = text.replace("- [ ] B2-6: Non-Determinism Guarding semantics implemented", "- [x] B2-6: Non-Determinism Guarding semantics implemented")

with open("ROADMAP.md", "w") as f:
    f.write(text)
