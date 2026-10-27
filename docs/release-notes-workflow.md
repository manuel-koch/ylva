# Release Notes Guidelines

## Context

Releases are automated by release-plz from conventional commits (see
[development workflow](./development-workflow.md#continuous-integration-and-releases));
its generated `CHANGELOG.md` is the developer-facing commit log. This workflow is the
user-facing, curated summary: the
[`release-notes.yml`](../.github/workflows/release-notes.yml) job runs it with Ylva after
each release and replaces the GitHub release body. It can also be run manually.

When requested by user, create brief release notes for recent commits (HEAD vs last tag,
or an explicit range).
Check the content of every commit, omit commits that are clearly not related to a
feature or bugfix.

Read each commit as a Conventional Commit: the title has the form
`type(scope): description`. Treat `!` before the colon (for example,
`feat(api)!: ...`) as a breaking-change marker. Also recognize a body line beginning
with `BREAKING CHANGES:` or `BREAKING-CHANGES:` as a breaking-change marker.
When either marker is present, make the breaking change and its compatibility impact
clear in the release-note entry; use the commit body and diff to describe the impact.

Write the release notes to `docs/release-notes-<FROM>-to-<TO>.md`,
e.g. `docs/release-notes-v0.2-to-v0.3.md` (overwrite if it already exists).
Do not print the content to the conversation — only confirm the file was written.

Prefix the release notes with a one-sentence title naming the most important
aspect(s) of the release.

## Entry Format

Use the following format for each applicable commit (curly braces denote dynamic
content based on the commit).
Output entries one after another with nothing between them — no `---` separators,
only blank lines between entries, no numbered lists, no extra headings.
The trailing blank line inside each entry is the only allowed spacing.

Breaking changes are never mixed into ordinary entries. A commit is breaking when its
type is followed by `!` (e.g. `feat!:`) or its message has a `BREAKING CHANGE:` footer.
When there are any, put them first, under a `## Breaking changes` heading placed
directly after the title, and put all remaining entries under a following `## Changes`
heading so the two groups stay distinct (these are the only allowed extra headings).
Give each breaking entry the title prefix `Breaking:` and state in its text what users
must change or lose. Omit both headings when there are no breaking changes.
A breaking commit is included even if it looks like a refactor.

```markdown
**{change title}**

{Brief topic ( using 1-2 sentences max ) of feature or bugfix change}
```

## Release Notes Example

Example file content for two consecutive entries in the generated release notes
( do not deviate from this format ):

```markdown
# Minor bugfix and new agent tool availability

**Fix: Wrong Activity Indicator**

Executing a chat message should update the current activity indicator.

**Tool `read` introduced**

AI model can now use the `read` tool to access file within the workspace,
```

Example with a breaking change:

```markdown
# Session storage changes and new agent tool availability

## Breaking changes

**Breaking: Session section labels renamed**

Persisted session files use new content-section labels; sessions saved by older
versions may display their previous labels.

## Changes

**Tool `read` introduced**

AI model can now use the `read` tool to access file within the workspace,
```
