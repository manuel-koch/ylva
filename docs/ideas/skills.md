# Skills

Implemented through [the skills plan](../plans/skills-plan.md). Discovery,
configuration, and limits are specified in
[configuration](../configuration.md#agent-skills); lifecycle and delivery are
described in [features](../features.md#agent-skills). The original questions
below are retained with their resolutions.

Discover standard agent skills in standard directories and advertise them
to LLM via tools schema/descriptions/instructions.

Each skill will be named and its description will be used when
advertising available skills.

Introduce new tool `load_skill` to allow LLM to load a named skill into
context.
We only load body of the skill into context, discarding any frontmatter for now.
The command will use a `name` attribute to select the skill to be loaded into
LLM context.

We load the skill content and add a footer/instruction including absolute path
of the skill file, to allow LLM to discover and load skill sidecar files
afterwards on-demand.

- What are the standard directories for eligible skills ?
  Resolved: exactly the eight ordered roots in the configuration reference.

- How to prioritize skills in eligible directories that have same name ?
  We can only use one skill if names collide.
  Resolved: first valid retained candidate wins; differing shadowed sources
  are reported, while identical copies and canonical aliases are shown only
  with `--with-duplicates`. Policy filtering never picks an alternate source.

- Which skill frontmatter properties should we support ?
  What are common frontmatter properties in other agent ecosystems, what is a
  standard set of properties we should support ?
  Resolved: validate required `name`/`description`; optional and unknown
  metadata has no runtime effect.

- Should we automatically allow `read` tool to access sidecar files of a loaded
  skill ( likely out-of-workspace path(s) ) ?
  Or should that require a user-approval ( default sandbox floor ) ?
  Resolved: ordinary read policy by default, with an opt-in read-only
  `skill_sidecar` rule based on each agent's callable catalog.

- We need a way to configure which skills are available for an agent,
  `allowed_skills` ( like `allowed_tools` ).
  Default would be to allow any discovered skill.
  Only allowed skills should be advertised in the system prompt with name/description.
  A skill that is "allowed" but unknown should trigger a startup error.
  Implemented, including validation of unused types and explicit-empty lists.

- Do we need a CLI command to show/discover all possible skills ?
  How can user configure the `allowed_skills` without knowing which ones
  are available ?
  `ylva skill list` or `ylva skill discover` could discover all eligible skills and
  print their name/description/path.
  Resolved: `ylva skill list`, provider-independent and read-only. Identical
  copies are hidden unless `--with-duplicates` is requested; differing versions
  remain visible as shadowed variants. Full instruction/resource fingerprints
  establish equality without changing the winning source or its permissions.
