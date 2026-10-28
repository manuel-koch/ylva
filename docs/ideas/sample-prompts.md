# Sample prompts

Use subagent(s) to schedule the analysis and/or run it concurrently:

Discover all `docs/ideas/*.md`.
For each idea: analyse the intend and compare the current
state of implementation against the content/aim of the idea.

Save your report with the findings to `docs/ideas/ideas-progress.md`:

- idea is fully implemented
- idea is partly implemented, note the gaps
- idea is not implemented/started yet

---

Why is the sky blue?
And why appear oceans green ?
Why are clouds sometimes shiny bright white or dark gray black ?

---

Please explain more.

---

Please read file `docs/development-workflow.md` and
create a three sentence abstract of its content.

---

How many markdown files are in `docs` folder ?

---

Why is the sky blue?
And why appear oceans green ?
Why are clouds sometimes shiny bright white or dark gray black ?

Please read file `docs/development-workflow.md` and
create a three sentence abstract of its content.

Please read `/Volumes/Backups/foo/bar.txt`
and describe its content.
Delegate reading files to subagent.

Tell me a short story (200 words) about a little lamb that went to school.

Please read file first 10 lines of `docs/development-workflow.md`
and summaries it in one sentence.

How many files are in project ( incl. git ignored files and hidden files) ?

How many files are in project ( excl. git ignored files and hidden files ) ?

How many directories are in project ( incl. git ignored dirs and hidden dirs ) ?

How many directories are in project ( excl. git ignored dirs and hidden dirs ) ?

How many files/directories are in project ( incl. git ignored paths and hidden paths ) ?

How many files/directories are in project ( excl. git ignored paths and hidden paths ) ?

How many markdown files are in `docs` folder ?

Please read file `docs/development-workflow.md` and
count the words in its content.

Create a four line poem about a little lamp and save it to `tmp/poem.md`.

Summarize current conversation and save it to `../logs/summary.md`.

Create a 50 word sentence ( without newline ) and save it too `../logs/long-line.md`.

Create a 50 word sentence ( without newline ) and save it too `tmp/long-line.md`.

Create a 50 word sentence ( without newline, use "-" to join words )
and save it too `tmp/long-words-line.md`.

Use search tool to find markdown files that contain the case-insensitive word `tool`.

Use search tool to find Rust source files that contain the case-sensitive word `SearchTool`,
provide 4 lines of context for each match.

Use search tool to find Rust source files that contain the case-sensitive word `BashTool`,
report only the paths that matched.

Use search tool to find Rust source files that contain the case-sensitive regexp `\w+Tool`,
report only the matched regexp without path/line info.

Create a four line poem about a little lamp and save it to `tmp/poem.md`.

Replace all occurance of "lamp" in `tmp/poem.md` by "frog" using the `edit` tool!

Replace first occurance of "lamp" in `tmp/poem.md` by "frog" using the `edit` tool!

Replace line 2+3 in `tmp/poem.md` by one line "hello world" using the `edit` tool!

Replace line 1+2 in `tmp/poem.md` by

```text
lorem ipsum
this is a nice place to be"
```

using the `edit` tool!

Read `tmp/poem.md` and ask user which other animal ( free text, no constraint
or pre-selected options ) to use to replace existing animal in the poem.

Read `tmp/poem.md` and ask user which other animal to use to replace existing animal
in the poem.
Give user 5 random animal options to choose from.

Read `tmp/poem.md` and ask user which other animal to use to replace existing animal
in the poem.
Provide the current poem as context for the question.
Give user 3 random animal options to choose from.

---

Create an image using a bash command:

```bash
magick -size 200x200 canvas:white \
       -fill red -stroke none \
       -draw "polygon 100,71 75,114 125,114" \
       tmp/sample.png
```

Read image `tmp/sample.png` ( using the `read` tool only ) and describe
the content. Estimate the edge length of the geometrical shape, if any.

Write a binary file with the textual content of "hello world" to `tmp/sample.txt`.

---

Read project documentation and summarize the goal and main features of the project:

- `./README.md` (must read)
- `./AGENTS.md` (must read)
- `./docs/*.md` (pick/read files that seem to related to the topic in question)

Summarize features with brief description in a markdown table.
Summerize current state of planned implementation progress.

Use headings to group topics ( features, state etc ).

Show me a simple python script to parse the JSONL persisted session generated
by Ylva run and search for a keyword in conversation messages.
Just show me the script, no need to save it.

Ask user a thought provoking question about the project ( discover the project, don't come up
with arbitrary question ) using the clarify tool.
Using markdown formatting for the question and the context, where question and context contain a
heading and a sentence paragraph.

---

Starting from src/main.rs, follow every `mod` declaration, read the referenced
module file, follow its `mod` declarations, and so on until you've traced the
full module tree. Then report the tree structure.

---

Run `python3` to find out which version it is and where the python interpreter
is located.

Run `tests/e2e/long-running.bash`.

Run the following command in bash:

```shell
date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum ; date ; sleep 3 ; echo lorem ipsum
```

Run the following command exactly like given (as-is, don't re-write/collapse linebreaks ) in bash:

```shell
for n in $(seq 1 5) ; do
  echo "$(date) lorem ipsum"
  sleep 3
done
```

Run `tests/e2e/heavy-output.bash`.

Run `tests/e2e/long-running.bash`.

Run the following command in bash:

```shell
date
sleep 1
uname -a
sleep 1
grep foo README.md
```

---

Use subagent to delegate discovering tool capabilities in
workspace source-code.

Run the following task in a subagent and let it report its findings back:

Analyze the `src/agent/subagent_manager.rs` functionality
and write a brief summary of its implementation.
Use markdown table to structure your findings.

---

Analyze the given tool functionality implemented in source code
and list currently implemented features in a brief markdown descriptions.
Use a subagent per tool and let subagent report findings back.

- `read` tool
- `write` tool
- `glob` tool
- `search` tool
- `clarify` tool
- `bash` tool

ONLY use `bash` tool to search for relevant files!

Save the complete summary to `tmp/summary.md`, don't edit an existing
file nor use its current content. If file exists, override it.

---

Which subagent types can you use to delegate tasks ?

---

Run the following tasks each in a distinct subagent concurrently:

- Task A:
  - Search for files that relate to bash tool
  - Run bash script `for i in {1..3}; do date +%s; echo "hello world"; sleep 1; done`
  - Use `rumdl` to check markdown docs at `docs/ideas/*.md`

- Task B:
  - Clarify with user ( using the `clarify` tool and ask for an animal name )
  - Create a four line poem about a the selected animal and save it to `tmp/poem.md`

---

Run each the following task(s) in subagent(s) ( type = local-discovery )
and let them report their findings back:

- Analyze the `read` tool functionality implemented in source code
  and list currently implemented features in a brief markdown descriptions.

---

Run each the following task(s) in subagent(s) ( choose least invasive subagent
type ) and let them report their findings back:

- Analyze the `read` tool functionality implemented in source code
  and list currently implemented features in a brief markdown descriptions.

---

Run each the following task(s) in subagent(s)
and let them report their findings back:

- Analyze the costs tracking functionality implemented in source code
  and list currently implemented features in a brief markdown descriptions.

---

Discover the project structure related to agent tool implementations.
Provide a 3-5 sentence summary for every source file.
Run the summary task on each file in separate subagent and let it report
its findings back.

Format the result using markdown headers (per file) and the
related file summary paragraph.
Save the complete summary to `tmp/summary.md`, don't edit an existing file
nor use its current content. If file exists, override it.

---

Discover the project structure related to agent tool implementations.
Provide a 2-3 sentence summary for every source file.
Run the summary task on each file in separate subagent and let it
report its findings back as structure JSON:

```json
{
  "analysed_files": [
    {
      "path": "<source-code-file>",
      "features": [
        "<description of the feature a in 1-3 sentences>",
        "<description of the feature b in 1-3 sentences>",
        ...
        ]
    },
    ...
  ]
}
```

---

Do a web research about agent-harnesses that can be run on MacOS.

Do a web research about agent-harnesses, overview of available agents,
best performance/usability.

Do a web research about search-websites ( and APIs ) that can be used to
search for given terms.
Check whether they can return structured data or just html.
Check whether they have contrains like rate-limits, bot-detection,
need authentification/api-key etc.

Do a web research using `web_fetch` of GitOver project at `https://github.com/manuel-koch/gitover-rust-tui`.

Read `https://www.bundestag.de/` using `web_fetch` and report a summary of the page.

Read `https://github.com/foo/bar` using `web_fetch` and report a summary of the page.

Fetch image using `web_fetch` and describe its content: `https://i.ytimg.com/vi/0iNrGpwZwog/sddefault.jpg`.

Do a search on Github using `web_fetch` and this URL template for url-encoded terms:
`https://api.github.com/search/issues?per_page=3&page=1&q=<terms>+language:rust`.
Search for "agent harness" and report first 30 repository URLs.
DON'T tweak the `per_page` query-param. This will keep the result-size within limits!

---

<Sample prompts that will likely stress a LLM and cause filling up of context>

## Map the agent tool-call lifecycle

Trace how a model tool call moves through Ylva, from the provider response to tool
execution and back into the next model request.

Work in stages. First, locate and read the core agent loop, tool registry,
tool-call/result types, and provider message serialization. Then trace one
filesystem tool and one delegated subagent call through their execution and
persistence paths. Finally, compare the paths and explain how approval,
errors, displayed results, and model-facing results are handled.

Use repository tools to inspect files and search for relevant symbols. Cite file
paths and line ranges for each claim. Do not modify files or run commands that
change the workspace. If the trace is too large for one response, continue with
another focused stage until you have covered the full lifecycle. End with a
concise sequence diagram in plain text and a list of unresolved questions.

## Audit session persistence and resume

Build a detailed account of Ylva session persistence, resume, and clone behavior.

Work in stages. Start by finding the session store, serialized event types,
and tests that define the persistence contract. Then trace fresh-session creation,
incremental writes, resume reconstruction, clone behavior, locking,
and recovery after interrupted writes. Check how root and subagent transcripts
relate, and how compaction events affect the reconstructed model view while
preserving the visible transcript.

Use repository tools to inspect source, tests, and relevant docs. Cite file paths
and line ranges. Do not edit files, create sessions, or run commands that mutate
the workspace. Continue with focused stages until you have covered all the
behaviors above. Finish with a lifecycle outline, a table of key invariants
and where each is enforced, and any inconsistencies you find between implementation,
tests, and documentation.

## Follow context budgeting and compaction

Explain exactly ( reading the sources & documentation ) how Ylva estimates
context size and decides when and how to compact a conversation.

Work in stages. First inspect token estimation and the footer readout.
Then trace threshold evaluation, background summary generation, safe
application boundaries, synchronous fallback, and the construction of the
model view after compaction. Finally, follow persistence and resume to
determine how the summary and boundary are restored, and identify tests
for edge cases such as failed summaries, oversized tool results, or
incomplete tool-call pairs.

Use repository tools to search and read the implementation, tests, and docs.
Cite file paths and line ranges for each claim. Do not edit files or run
commands that change the workspace. Keep investigating in additional focused
stages if needed. Finish with a decision-flow outline, a worked example
using hypothetical token estimates, and any gaps between the documented
contract and tested behavior.

Save your report to `tmp/context-budgeting-and-compaction.md`.

---

Run a read-only smoke test of root-agent and subagent Bash tool execution.
Execute the steps; do not merely describe them.

1. As the root agent, use the Bash tool to run:
   printf 'ROOT_BASH_START\n'; pwd

2. Launch two subagents, in parallel if supported. Explicitly instruct each
   subagent to use its own Bash tool, not ask the root agent to run commands:
   - Subagent A: run `printf 'SUBAGENT_A_BASH\n' ; sleep 10 ; uname -s`
   - Subagent B: run `printf 'SUBAGENT_B_BASH\n' ; sleep 10 ; printf '42\n'`
   Each subagent must return the command, actual stdout, and exit status.
   Do not allow further delegation.

3. Wait for both subagents to finish. Then, as the root agent, use the Bash
   tool again to run:
   printf 'ROOT_BASH_END\n'

4. Report a compact table with one row per Bash execution: agent,
   command, stdout, exit status. Mark the test PASS only if both subagents
   actually executed their Bash calls and both root Bash calls succeeded.
   If a required tool is unavailable or execution fails, report FAIL and
   the reason; do not simulate output or substitute root execution for a
   subagent's execution.

Do not edit files, install anything, access the network, or change Git state.

---

Copy `docs/known-test-issues.md` to `tmp/dummy.md`
( overwrite target file if it exists ) using a `bash` tool call.
Use skill `unslop` to rewrite `tmp/dummy.md`.

---

Create an architecture-decision-record for ylva local skill feature
in `docs/features.md`.
Save the ADR in folder `docs/architecture-design-records`.

---

Perform a documenation review over the following files:

- docs/development-workflow.md
- docs/cli-options.md
- docs/features.md

Save your findings as a markdown formatted report to
`tmp/docs-review.md`.

---
