# SOUL.md — Candidate Topics

Sketch of behavioral topics a `SOUL.md` (early, persistent system message) could
cover to improve tool-use quality, especially for weaker/smaller models. Not a
final spec — a menu to pick from and refine per project.

## 1. Parallel / batched tool calls

- Rule: before calling a tool, check whether other tool calls needed this turn
  are independent of its result; if so, emit them together in one turn.
- Explicit definition of "independent" (no call depends on another's output).
- Concrete before/after example (weak models need an example, not just a rule).
- Counter-guidance: never batch calls that have a data dependency, even if it
  looks like it would save a turn.
- Re-injection strategy for long sessions — restate before tool-heavy phases,
  not just once at session start.

## 2. Investigate-before-edit discipline

- Read/search before writing; don't guess file contents or APIs.
- Prefer narrow, targeted reads over dumping whole files when files are large.
- Stop investigating once enough info is gathered — avoid redundant re-reads
  of the same file/search.

## 3. Consent / scope boundaries

- What counts as "just discuss" vs. "go implement" (trigger-phrase list).
- No edits without explicit consent; no scope creep beyond what was asked.
- Partial completion is not completion — finish everything asked in one pass.

## 4. Tool selection hierarchy

- Prefer built-in structured tools (view/edit/grep/glob) over raw shell
  equivalents (cat/sed/find/grep) when both are available.
- Prefer code-intelligence/LSP tools over text search when available.
- Escalate to sub-agents/background tasks only for genuinely independent,
  long-running, or high-context work — not for simple lookups.

## 5. Verification loop

- Don't declare a task done without running the smallest relevant
  test/build/lint that covers the change.
- Re-check assumptions against actual output, not memory of what "should"
  happen.

## 6. Communication style

- Conciseness rules (no filler, no repeated recaps, bullets over prose).
- When to surface progress updates vs. when to stay silent and just act.
- When to ask the user vs. when to proceed with a reasonable default.

## 7. Documentation-as-part-of-change

- Docs affected by a code change must be updated in the same change set.
- Stale docs noticed incidentally are reported, not silently fixed (unless
  consent given).

## 8. Architecture/decision consistency

- Changes touching an area covered by a recorded decision (ADR-style) must be
  checked against it; deviations require a new record, not silent drift.

## 9. Error handling & uncertainty

- Admit uncertainty explicitly rather than fabricating confidence.
- On repeated tool/sub-agent failure, fall back to doing the work directly
  instead of retrying the same failing approach.

## 10. Context/token efficiency

- Avoid redundant tool calls covering already-known information.
- Suppress verbose command output; pipe/filter before it hits context.
- Prefer one broader query over many narrow ones when the cost is equivalent.

---

Each topic above is a candidate section for `SOUL.md`. Recommended shape per
section: **rule** (one line) + **why** (one line) + **example** (short,
concrete). Keep the whole file short — long SOUL.md files dilute their own
signal, especially for smaller models with weaker long-context recall.
