# Project History

pg_trickle started with a practical goal. We were inspired by data
platforms built around pipelines that keep themselves incrementally
up to date, and we wanted to bring that same style of self-maintaining
data flow directly into PostgreSQL. In particular, we needed support
for recursive CTEs, which were essential to the kinds of pipelines
we had in mind. We could not find an open-source incremental
view-maintenance system that matched that requirement, so pg_trickle
began as an attempt to close the gap.

It also became an experiment in what coding agents could realistically
help build. We set out to develop pg_trickle without editing code by
hand, while still holding it to the same bar we would expect from any
other systems project: broad feature coverage, strong code quality,
extensive tests, and thorough documentation. Skepticism toward
AI-written software is reasonable; the right way to evaluate
pg_trickle is by the codebase, the tests, and the docs.

That constraint changed how we worked. Agents can produce a lot of
surface area quickly, but database systems are unforgiving of vague
assumptions and hidden edge cases. To make the project hold together,
we had to be unusually explicit about architecture, operator
semantics, failure handling, and test coverage. In practice, that
pushed us toward more written design, more reviewable behavior, and
more verification than a quick prototype would normally get.

The result is a **spec-driven** development process, not vibe-coding.
Every feature starts as a written plan — an architecture decision
record, a gap analysis, or a phased implementation spec — before any
code is generated. The
[`plans/`](https://github.com/grove/pg-trickle/tree/main/plans)
directory contains over 110 documents covering operator semantics,
CDC trade-offs, performance strategies, ecosystem comparisons, and
edge-case catalogues. Agents work from these specs; the specs are
reviewed and revised by humans. This is what makes it possible to
maintain coherence across a large codebase without manually editing
every line: the design is explicit, the invariants are written down,
and the tests verify both.

We also do not think the use of AI should lower the standard for
trust. If anything, it raises it. The point of the experiment was
not to ask people to trust the toolchain; it was to see whether
disciplined use of coding agents could help produce a serious,
inspectable PostgreSQL extension. Whether that worked is for readers
and users to judge, but the intent is simple: make the code, the
tests, the documentation, and the trade-offs visible enough that the
project can stand on its own merits.

---

## Contributors

- [Geir O. Grønmo](https://github.com/grove)
- [Baard H. Rehn Johansen](https://github.com/BaardBouvet)
- [GitHub Copilot](https://github.com/features/copilot) — AI pair
  programmer

---

**See also:**
[Roadmap](roadmap.md) · [Changelog](changelog.md) ·
[Contributing](contributing.md)
