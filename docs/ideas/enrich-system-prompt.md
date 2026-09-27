# Enrich system prompt

**Implemented.** The design questions this idea raised were settled in a
design review and shipped together with the rest of the work. The contract
lives in:

- [configuration](../configuration.md#global-persona-soulmd) → *Global
  persona (`SOUL.md`)* → *Trusted instruction files* — the initial
  system-prompt contract, including composition order, sentinel markers,
  limits, and failure behavior;
- [features](../features.md#layered-global-persona-and-initialization) — the
  user-facing wording, plus the `~`/`$ENVVAR` expansion contract under
  *Tools to be used by models*;
- [implementation notes](../implementation-notes.md#layered-configuration-and-persona-assembly)
  — `config::instructions` mechanics and *Path-like argument normalization*;
- [plans](../plans/enrich-system-prompt-plan.md) — the executed plan.

Summary of the settled decisions:

- the trusted filename set is exactly `AGENTS.md` and `CLAUDE.md`, matched
  case-sensitively; `AGENT.md`, `GEMINI.md`, and tool-specific rule formats
  stay out of scope, so arbitrary Markdown is never loaded;
- eligible locations are the Ylva home directory and the workspace chain from
  the workspace root down to the working directory — not parents above the
  workspace root, the OS home directory, the workspace `.ylva/`, or
  subdirectories outside that chain;
- composition is concatenation with position-based precedence (closer to the
  working directory wins conflicts), each section wrapped in bracketed
  sentinel markers behind one fixed safety-preamble line;
- instruction content cannot escape the workspace trust boundary and cannot
  override Ylva's runtime safety constraints (tool allowlists, approval
  floors, sandbox), which remain enforced in code;
- oversized (> 64 KiB raw bytes), unreadable, non-UTF-8, empty, and
  duplicated files have deterministic, test-covered behavior.
