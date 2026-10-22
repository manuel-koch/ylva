# Release Notes Guidelines

## Context

Releases are automated by release-plz from conventional commits (see
[development workflow](./development-workflow.md#continuous-integration-and-releases));
its generated `CHANGELOG.md` feeds the GitHub release. This workflow remains an optional,
manually curated summary.

When requested by user, create brief release notes for recent commits (HEAD vs last tag,
or an explicit range).
Check the content of every commit, omit commits that are clearly not related to a
feature or bugfix.

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
