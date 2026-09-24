# Spike: Bash Command Rules (`tree-sitter-bash` AST shapes)

A standalone proof-of-concept (spike) crate that empirically confirms the
`tree-sitter-bash` v0.25.1 AST shapes that
`src/sandbox/bash_command_extractor.rs` (the real,
tested extractor backing `SandboxContext::check_command`) is built around.

This is a **spike, not production code**. It lives in the dedicated
`spikes/` directory, is a separate crate, and is not wired into any `src/`
tree. The findings it confirms are recorded as doc comments at the top of
`src/sandbox/bash_command_extractor.rs` and re-verified there by that
module's own `grammar_assumptions` unit tests, so a future `tree-sitter-bash`
version bump that changes these shapes fails CI loudly instead of silently
misclassifying commands.

## What it proves

Running `cargo run` prints the full parse tree (including anonymous/unnamed
nodes and field names) for a range of scripts, confirming:

- there is no `simple_command` node type — the real node name is `command`;
- `list` is produced only for `&&`/`||`; `pipeline` for `|`-chains;
- `;`, `&`, and newlines are lexical terminators between sibling top-level
  statements, not wrapped in any grouping node — `;`/`&` each surface as
  their own anonymous single-character node, which is how a trailing/inline
  `&` (backgrounding) is detected;
- a `command` node's `variable_assignment` children precede its
  `command_name` field;
- quoting: `"..."` parses as a `string` node (`string_content` child, or a
  nested expansion node when it contains one); `'...'` parses as a fully
  literal `raw_string`; an unquoted brace expansion (`{a,b}`) parses as a
  `concatenation` of plain `word` nodes, while a quoted one (`"{a,b}"`)
  parses as an ordinary `string`;
- out-of-scope constructs (subshell, command/parameter substitution,
  control flow, function definitions, heredocs, redirects) each nest inside
  their own dedicated node type, distinct from `command`/`list`/`pipeline`;
- `tree.root_node().has_error()` reliably flags a script-wide parse failure.

## Run

```shell
cargo run
```
