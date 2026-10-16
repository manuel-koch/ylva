# Ylva agent-harness CLI options

Reference for Ylva's command-line interface. Parsing is backed by `clap`
(derive model in `src/cli.rs`); the tables below describe the resulting
surface.

## Flags

| Flag | Type | Accepted values | Default | Description |
| --- | --- | --- | --- | --- |
| `--version` | none | — | — | Print build provenance (`Version`, `Commit`, `Built`, `Profile`, `Features`), the Ylva home (`Ylva home:`), and the workspace root (`Workspace root:`) for the current directory, then exit. Paths under `$HOME` are shown with a leading `~`. Takes precedence over all other arguments. |
| `--help` | none | — | — | Print usage (all flags and subcommands) to stdout and exit with code `0`. |
| `init [--workspace] [--force]` | subcommand | `--workspace`, `--force` | — | Write configuration starters and exit before provider/session startup. Without `--workspace`, writes `SOUL.md`, `profiles/default/config.toml`, and an annotated `profiles/default/.env` under `YLVA_HOME` (or `${HOME}/.ylva`); `--workspace` writes the default profile config and `.env` starters plus `.gitignore` under the discovered workspace root's `.ylva/`; both modes write-if-missing unless `--force` is supplied. The workspace `.gitignore` lists `.env`, `.envrc`, and `chats/`; an existing one keeps its lines and only the missing entries are appended. Agent-type files are not seeded. |
| `--no-tui` | none | — | — | Force headless (CLI) mode even when stdout is a terminal. Useful for scripting or debugging without the TUI. |
| `--color=<mode>` | enum | `auto`, `no`, `yes` | `auto` | Select CLI color behavior: detect terminal support, force no colors, or force colors. Has no effect on TUI rendering. |
| `--prompt <text>` | string | any text | *none* | Initial user turn sent to the agent. Resolved with the precedence described below. |
| `--profile <name>` | string | a profile name: 1–64 ASCII characters, starting with a letter or digit, then letters, digits, `_`, or `-` | `default` | An invalid name is a usage error (exit code `2`). Resolve `<workspace-root>/.ylva/profiles/<name>/config.toml`, then `~/.ylva/profiles/<name>/config.toml`. When `--profile` is given explicitly, a missing file at both disk layers is a hard error (exit code `2`) naming the profile. With no `--profile` flag, the implicit `default` profile uses `ProfileConfig::default()` when no disk file exists. A malformed or unreadable file at the winning layer is always an error. See `docs/configuration.md`. |
| `--log-file <path>` | path | any writable file path | *none* | Write logs to this file. A leading `~` and `$ENVVAR`/`${ENVVAR}` tokens are expanded, as for other path arguments. The file is opened in truncate/overwrite mode on each start, so logs never accumulate across runs. |
| `--log-level <level>` | enum | `trace`, `debug`, `info`, `warn`, `error` | `info` | Minimum severity written. Selecting a level includes it and all higher-severity levels (`trace < debug < info < warn < error`). |
| `--log-format <format>` | enum | `text`, `json` | `text` | Output format. `text` is human-readable; `json` produces JSONL (one JSON object per line). |
| `--agent <type>` | string | any agent type available in the selected profile | see below | Root agent type to run as. On a fresh session it defaults to `default`; on a resume it defaults to the type the session last stored. An explicit value always wins and becomes the latest stored selection, so later resumes without the flag continue under it. A type that is absent from the selected profile is a startup error (exit code `2`), unless the flag explicitly names an available type that resolves a stale stored name. See [Resuming a session](#resuming-a-session). |
| `--conversation-id <uuid>` | uuid | any persisted session id | *none* | Resume the named session: its `.jsonl` file is re-attached for append instead of a fresh session being started. An unknown id is a hard error (exit code `2`) — never a silent fresh session. With `--clone` it names the session to copy instead. See `docs/persistence.md`. |
| `--clone` | none | — | — | Copy the session named by `--conversation-id` into a **brand-new** session instead of resuming it: the source's full logical event stream is replayed into the new file, the source file stays byte-for-byte unchanged, and all new writes go to the clone. Requires `--conversation-id <uuid>` (exit code `2` without it), and the source must be a root session that no other Ylva process holds open — a child transcript or a locked source is a hard error (exit code `2`) that creates nothing. See [Clone contract](persistence.md#clone-contract). |
| `--auto-approve-read` | none | — | — | Skip the human-approval prompt for `read` tool calls. |
| `--auto-approve-write` | none | — | — | Skip the human-approval prompt for `write` tool calls. |
| `--auto-approve-glob` | none | — | — | Skip the human-approval prompt for `glob` tool calls. |
| `--auto-approve-bash` | none | — | — | Skip the human-approval prompt for `bash` tool calls. |
| `--auto-approve-search` | none | — | — | Skip the human-approval prompt for `search` tool calls. |
| `--auto-approve-edit` | none | — | — | Skip the human-approval prompt for `edit` tool calls. |
| `--auto-approve-web-search` | none | — | — | Skip the human-approval prompt for `web_search` tool calls. |
| `--auto-approve-web-fetch` | none | — | — | Skip the human-approval prompt for `web_fetch` tool calls. |
| `--auto-approve-spawn` | none | — | — | Skip the human-approval prompt for `spawn_subagent` tool calls. See `can_spawn` in `docs/configuration.md` and "Child-agent delegation" in `docs/features.md`. |
| `--max-concurrent-subagents <N>` | signed integer | any integer | profile setting | Override the profile's maximum number of simultaneously running child agents for this run. A value `<= 0` means unlimited. |

An invalid `--color`, `--log-level`, `--log-format`, or `--conversation-id`
value, an unknown flag, a flag missing its required value (for any
value-taking flag, including `--profile`), `--clone` without
`--conversation-id`, or a `--log-file` that cannot be opened (for example, its
containing directory does not exist) prints a usage error to stderr and exits
with code `2` — the error names the attempted path or flag. Repeating
`--color` or `--prompt` uses the last occurrence; other
value-taking flags may not be repeated.

`--color`, `--log-file`, `--log-level`, and `--log-format` are global, so they work
before or after a subcommand. Every other top-level flag, including `--profile`
and `--no-tui`, must come before the subcommand (`ylva --no-tui init --workspace`).
The `init`, `session`, `profile`, and `skill` subcommands ignore `--profile` and never
load a profile. The `init` subcommand also exits with code `2` when its target
root cannot be resolved or a seeded file cannot be written.

When the CLI writes to a terminal, its section headers and tool status lines
use the same semantic colors as the TUI. Colors are disabled automatically for
non-terminal output, `TERM=dumb`, or when the standard `NO_COLOR` environment
variable is set when `--color=auto` is selected. Use `--color=no` to disable
them explicitly, or `--color=yes` to force them even for non-terminal output,
`TERM=dumb`, or `NO_COLOR`. The flag only affects CLI output, not the TUI.

## Exit codes

Every `ylva` process ends with exactly one of these codes. The table covers chat
runs in headless and TUI mode and the `ylva session`, `ylva profile`, and
`ylva skill` subcommands. The `--help`
epilog lists the same codes, generated from the same source.

| Code | Meaning | Examples |
| --- | --- | --- |
| `0` | Successful run | The response finished, and no error, interruption, or approval denial was recorded in the latest turn. `--help` and `--version` also exit `0`. |
| `1` | General execution error | A provider or stream failure, a root-level error such as the tool-use iteration cap, an I/O failure while a run is in progress, a partial `ylva session` maintenance failure, or a non-zero exit from the editor run by `ylva profile edit`. |
| `2` | Usage or startup error; no run started | An unknown flag, a missing flag value, a `--log-file` that cannot be opened, an unresolvable profile or `--agent`, an unknown `--conversation-id`, a failing `init`, or a `ylva profile` refusal such as an invalid name, an existing profile without `--force`, or a broken `config.toml` in scope. |
| `3` | Approval required but denied | A tool needed approval and was denied. Headless mode cannot ask, so it denies every such call; in the TUI a user denial counts too. The run continues after a denial. |
| `130` | User interruption (SIGINT or SIGTERM) | An external SIGINT or SIGTERM that aborts the run: Ctrl+C in headless mode, or a signal to the TUI in any state. `130` is the shell convention `128 + 2`. |

When several outcomes occur in one run, the highest-precedence one sets the code:
`130`, then `1`, then `3`, then `0`. A usage or startup error (`2`) ends the
process before a run starts, so it never competes with them.

Headless Ctrl+C (SIGINT) and SIGTERM are cooperative. The first signal stops the
agent, the run ends through the normal path, the resume hint is printed, and the
process exits `130`. Later signals during that shutdown do not force an exit.
SIGHUP is not handled.

In the TUI, only the latest turn counts. A new prompt starts a fresh outcome, and a
steer joins the running turn. Ctrl+C and Esc stop a turn without setting `130`,
because Ctrl+C is also how the TUI quits. An external SIGINT or SIGTERM always sets
`130` in the TUI, in any state: it quits an idle TUI at once, and it stops a running
turn (including a live subagent) before quitting. The TUI then restores the
terminal and prints the resume hint, as on every other TUI exit.

## Skill commands

`ylva skill list` discovers the complete local skill catalog without loading
a profile, initializing providers, consuming stdin, or creating profiles or
sessions. It ignores a preceding `--profile`. Winners sort by name:

```text
example
  Description: Example instructions, with continuation lines aligned beneath
               the first description line.
  Source:      /absolute/canonical/example/SKILL.md
  Body tokens: ~1.2K
  Shadowed:    /other/example/SKILL.md

other-skill
  Description: Another skill.
  Source:      /absolute/canonical/other-skill/SKILL.md
  Body tokens: ~567
```

Entries are separated by one blank line, and label values share one aligned
column. Descriptions preserve logical line/paragraph breaks and wrap to the
terminal width detected at runtime, with continuation lines indented beneath
the description text. If terminal dimensions are unavailable, wrapping uses
80 columns. Paths and provenance remain intact on single terminal-safe lines;
all untrusted control characters are escaped. Paths beneath the user's home
directory use a `~` prefix, including source, shadowed/duplicate provenance,
and warning paths. Paths outside home remain absolute; canonical source
identities are unchanged.

`Body tokens:` estimates the winning skill's instruction body using Ylva's
shared local `cl100k_base` tokenizer. Frontmatter, the loader's source footer,
tool-envelope framing, and sidecar resources are excluded. The `~` indicates
an estimate, not a token limit, provider-specific count, or billed usage.
Counts use the TUI footer's compact decimal notation: `~950`, `~1.2K`,
or `~1.2M`. These are tokens, not bytes.

`ylva skill list --color=auto|yes|no` uses the global CLI color policy, with
`auto` as the default. Supported terminal output renders skill names in bold
and labels underlined; redirected output, `TERM=dumb`, and `NO_COLOR` remain
plain in auto mode. `yes` forces ANSI styling, and `no` disables it.
The flag may also precede `skill`.

`Shadowed:` identifies lower-priority same-name sources with differing content,
or sources whose equality could not be established. These follow their winner
in precedence order, showing only the canonical source path without root or
discovery suffixes.
`Duplicate:` is reserved for canonical aliases and verified identical copies of
the winner, hidden unless `ylva skill list --with-duplicates` is requested.
Sharing a name alone does not make a source a duplicate. Content comparison includes
the complete instruction file and bounded resource fingerprints; see
[skill configuration](configuration.md#agent-skills) for comparison limits
and conservative handling of unavailable comparisons. Duplicates show only
their location, without root or discovery suffixes: the discovered path for an
alias of the winner's canonical file, otherwise the duplicate's canonical path.

```text
  Source:      ~/.agents/skills/example/SKILL.md
  Duplicate:   ~/.claude/skills/example/SKILL.md
```

Full source provenance remains in the catalog, not in the listing.
Rejected candidates produce stderr warnings, not usable
records. Empty discovery prints `No skills discovered.`. Empty/partial listings
still exit `0` when warnings occur; fatal initialization/output failures exit
`2`. This is the unfiltered discovery view, not an agent's callable catalog.
See [skill configuration](configuration.md#agent-skills).

## Session commands

`ylva session list [--include-subagents]` lists every session for the canonicalized
launch directory as `<timestamp> <session-id> <title>`. The timestamp is last-use
time: the latest persisted event timestamp, with creation time as the baseline
when no later timestamp exists. Resuming records a new startup event; committing
messages and other timestamped activity advances last use. Listing, title
rewrites, source cloning, and maintenance do not advance it. Titles are rendered
on one safe line. By default only roots are shown, least recently used first.

With `--include-subagents`, children appear immediately below their stored
parent, with a `+` marker after the timestamp and two additional spaces per deeper
nesting level:

```text
<last-used-at> <root-uuid> Root title
<last-used-at> + <child-uuid> Child title
<last-used-at>   + <nested-child-uuid> Nested title
```

Roots and siblings are ordered by last use, oldest first, with UUID tie-breakers. This is a
parent-grouped listing, not a globally chronological timeline. Children with
missing or unreadable parents remain visible with `[parent unavailable: <uuid>]`.
Parent cycles produce warnings without hiding sessions. Grouping follows the
stored parent identity; shared child transcripts are not duplicated under clones.

Listing is read-only, includes active sessions, and reports unreadable entries
on stderr. Session commands do not load provider configuration, read piped prompts,
or start a conversation.

`ylva session delete`, `ylva session compress`, and `ylva session decompress`
require exactly one selector: one or more positional UUIDs, `--before <date>`,
or `--all`. Examples:

```shell
ylva session delete <uuid> <uuid> --dry-run
ylva session delete --before 2026-01-01
ylva session compress --all
ylva session decompress --before 2026-01-01T12:00:00+02:00
```

Date-only input means UTC midnight; timestamps require an RFC 3339 offset.
Selection compares last-use times strictly before that instant, not session
creation times. Last use is revalidated under the identity lock: a session/group
used again after discovery is skipped if it no longer qualifies. There is no
recent-session limit or cross-project option. Bulk compression/decompression
includes subagent transcripts; bulk deletion selects roots and unshared
descendants. Shared descendants referenced by surviving roots/clones are kept
and reported, and explicit deletion of a referenced child is refused.

`--dry-run` previews the selected operation without changing files, directories,
or creating locks. Deletion prints one `would delete <uuid>` line per selected
session, including descendants. Compression/decompression previews list selected
sessions as `would compress <uuid>` or `would uncompress <uuid>`, respectively,
without listing storage files. Active writers are refused even in previews. Without
`--dry-run`, a valid selector executes; no `--yes` flag is required. Unknown
explicit IDs fail before any target changes. Independent targets/groups
continue after runtime failures with diagnostics on stderr and exit with the
general-error code `1` (see [exit codes](#exit-codes)). No-ops print
`already compressed <uuid>` or `already uncompressed <uuid>`,
including in previews.

Normal maintenance prints one `deleted <uuid>`, `compressed <uuid>`, or
`decompressed <uuid>` line per changed session, including deleted descendants.
Neither maintenance nor its preview lists storage files.
Retained-descendant notices and errors remain visible; file-level errors identify
the affected path.

You do not need to close every Ylva session in the project. Other sessions may
remain open while maintenance processes unlocked targets. To maintain an open
target, close that session first; a locked root also blocks deletion of its
descendant group.

Logging flags work before or after session subcommands, without loading provider
configuration. Optional logfile logging never replaces console diagnostics.
For locking, archive recovery and platform constraints, see
[persistence](persistence.md#manual-maintenance-and-archive-recovery).

## Profile commands

`ylva profile` manages profile directories. Like `init` and `session`, it runs
before any profile is loaded and never starts a session. Without `--workspace`
it acts on `profiles/` under `YLVA_HOME` (or `${HOME}/.ylva`); with
`--workspace` it acts on `<workspace-root>/.ylva/profiles/`. `--workspace`
without a discovered workspace root exits `2`. Unlike `init --workspace`, it
does not fall back to the current directory. Profile names follow the
`--profile` name rule.

`profile list` without `--workspace` merges both locations: when the current
directory is inside a workspace, that workspace's profiles come first, then the
Ylva home's. Only `list` merges; `create`, `edit`, `delete`, and `shims` keep
the home-by-default behavior above.

```shell
ylva profile list
ylva profile create work --shim-name office
ylva profile edit work
ylva profile delete work --dry-run
ylva profile shims --workspace
```

- `list` prints one line per profile directory as `<name>  <directory>`, followed
  by any marks. Rows are grouped by scope: the workspace's rows first, then the
  home's, with a blank line between the groups. Names are sorted within each group.
  Names and directories are padded into aligned columns separated by two spaces,
  with no header. A directory
  under `$HOME` is shown with a leading `~` (for example `~/.ylva/profiles/work`),
  and so is the `No profiles in <directory>` line. Marks:
  - `(shadowed by workspace)` marks a home profile whose name the current
    workspace also defines. Ylva loads the workspace profile in that case, so
    the home row is inactive. It appears only in the merged view, and only when
    the current directory is inside a workspace.
  - `(implicit default)` marks the `default` row, the profile used when no
    `--profile` flag is given. It appears only when `profiles/default/` exists,
    because the built-in default needs no directory.
  - `(no config.toml)` when `config.toml` is missing.
  - `(invalid name)` for a directory whose name fails the name rule.
  - `(invalid config)` when `config.toml` cannot be read or does not parse as TOML.

  Each mark is its own parenthesized tag, with `(implicit default)` first.
  `list` is read-only and exits `0` for all marks.
- `create <name>` seeds `config.toml` and `.env` with the same starters that
  `init` writes for `default`. If either file exists, it exits `2` and changes
  nothing, unless `--force` is given; `--force` overwrites only those two files.
  With `--workspace`, a missing `.ylva/.gitignore` is seeded too; an existing one
  is never overwritten, but any of its missing `.env`, `.envrc`, and `chats/`
  entries are appended. `--shim-name <shim-name>` sets the profile's shim name
  (see [shims](#shims)). After the `Shim <path>` line, it prints the same PATH
  hint as `shims`. Add the shims directory to `PATH` as that hint shows; for
  per-workspace setup, see
  [configuration](configuration.md#managing-profiles-and-shims).
- `delete <name>` removes `profiles/<name>/` and everything in it, with no
  prompt and no `--yes`, as `session delete` does. It prints
  `Deleted profile "<name>" at <directory>`. `--dry-run` prints
  `would delete "<name>" at <directory>` and changes nothing. A missing profile, or a target
  `config.toml` that cannot be read or parsed, exits `2` with nothing removed.
  `delete default` is allowed; the implicit default then falls back to the
  built-in default.
- `edit <name>` opens `profiles/<name>/config.toml` in `$VISUAL`, then
  `$EDITOR`; blank values count as unset. It exits `2` when the profile or its
  `config.toml` is missing (run `create` first) and when neither variable is
  set. The editor runs through `sh -c`, so a value such as `code --wait` works.
  A non-zero editor exit returns `1`. After a successful edit, the scope's
  shims are reconciled. Scope-wide configuration validation for reconciliation
  happens after the editor runs. Editor changes persist even if subsequent
  validation or shim reconciliation fails; Ylva does not roll them back.
- `shims [--workspace]` reconciles the scope's shims (see below), then prints
  one aligned `<profile>  <path>` row per shim, followed by a PATH hint. Add the
  shims directory to `PATH` as that hint shows; for per-workspace setup,
  see [configuration](configuration.md#managing-profiles-and-shims).

Exit codes for `profile`: `0` on success, including an empty `list`; `1` when
the editor exits non-zero; `2` for every refusal and I/O failure. On non-Unix
platforms, `profile shims` exits `2`, and `create`, `edit`, and `delete` skip
shim work.

### Shims

Each profile with a `config.toml` gets a shim: a POSIX `sh` script that runs
`ylva --profile '<name>' "$@"`, so `ylva-work --help` behaves like
`ylva --profile work --help`. Home shims go in `$YLVA_HOME/shims/`; workspace
shims go in `<workspace-root>/.ylva/shims/`. A shim is named `ylva-<name>`
unless the profile sets a top-level `shim_name` key in `config.toml`, which
replaces the whole file name. `create --shim-name` writes that key for you. A
shim name must pass the profile name rule and must not be `ylva`. `default`
gets no shim, so a `shim_name` in `default` is an error. A home profile and a
workspace profile with the same name are not refused or renamed; `PATH` order
decides which shim runs.

A shim sets no environment. A home shim runs `ylva` with the `YLVA_HOME` of the
shell that launched it, so it stores chats where plain `ylva --profile <name>`
does in that shell. Export `YLVA_HOME` before running a shim to use another Ylva
home. A workspace shim `cd`s to the workspace root first.

`create`, `edit` (after a successful editor run), `delete`, and `shims`
reconcile their scope. Each claimed shim is written, and every other regular
file in the shims directory is removed. Subdirectories are never touched, and
symlinked shims are not supported. Ylva owns the shims directory, so keep other
files out of it. `list` and `--dry-run` never reconcile.

Shim reconciliation plans every shim write and removal before applying them.
Planning exits `2` without writing or removing shims when two profiles in one
scope claim the same shim name, when any in-scope `config.toml` is unreadable
or invalid, or when any in-scope `shim_name` is invalid. The whole scope is
checked, so a broken config in another profile also blocks reconciliation.
`create` and `delete` perform this check before changing profile files;
`edit` runs the editor first and retains its changes even when this check fails.
The planning guarantee is not a rollback guarantee for editor changes or for
I/O failures while applying an accepted shim plan.

`profile shims` first reports any shims it removed. It then lists the shims in
scope, one row each, with the profile name first and the shim path after it.
Names are padded into a column, as in `list`, and there is no header. The PATH
hint follows, as `#` comment lines so it can be pasted without changing `PATH`
by accident. It starts with `# Update your PATH to use shims:`, then
`# export PATH="<directory>:$PATH"`, single-quoting the directory when it
contains shell-special characters. The last line states whether the shims
directory is on `PATH`: `# <directory> is already on PATH` or
`# <directory> is not on PATH`. A directory under `$HOME` is shown as `~/<rest>`,
and its export line is `# export PATH=~/<rest>:"$PATH"`: the unquoted `~` lets
the shell expand it, and only `<rest>` is quoted when needed. With no shims in
scope, it prints `No shims in <directory>` and no PATH hint. For per-workspace
`PATH` setup, see
[configuration](configuration.md#managing-profiles-and-shims).

## Resuming a session

Launches start a fresh session unless `--conversation-id` or `--clone` is supplied.

On exit — with any code in the [exit-code table](#exit-codes), as long as a
session was persisted — Ylva prints the session id and a ready-to-paste
command to stderr:

```text
Session: dbe9a693-d709-4670-89cd-6162e6ae3e11
Title:   Example session title
Resume with: ylva --conversation-id dbe9a693-d709-4670-89cd-6162e6ae3e11
```

The title line is omitted while untitled. The hint identifies the session active
at shutdown, including a replacement created by `/clear`; printing it does not
start or wait for a new title-generation request.

A resume also restores the root agent type the session last stored; an
explicit `--agent <type>` on the resume wins instead and is persisted as the
session's latest selection, while sessions written before agent-type metadata
existed continue as `default`.

When the run used an explicit `--profile <name>`, the hint includes it too
(`--profile <name>` is otherwise omitted, since the implicit default profile
needs no flag to resume with the same configuration). See
`docs/persistence.md` for how `--conversation-id` re-attaches the session
file, and `--clone` starts a copy of it instead.

For resume or clone within a running TUI, see
[`/session`](features.md#slash-commands).

## Prompt resolution

The initial user turn (`--prompt`) is resolved by precedence:

1. `--prompt <text>`
2. piped stdin read to EOF — only when stdin is **not** a terminal (something
   is piped in)

## Environment variables

Provider environments are loaded from the selected profile's optional
`.env` file (`~/.ylva/profiles/<profile>/.env`) and from the process
environment. An already-exported process variable takes precedence. TOML
string values may reference the merged environment as `${ENV_VAR}`; a
missing reference is a startup error. Named
`[provider_connections.<name>]` tables are constructed independently, even
when they share the same implementation. `/models` queries every named
connection and displays connection-qualified model choices. When no explicit
engines or provider connections are configured, the supported environment
providers are discovered instead. When no backend is configured, the
`MockProvider` (deterministic streaming tokens) is used.

In the TUI, `/models` presents named-engine shortcuts before raw catalog
choices. Shortcuts activate the exact current Engine runtime; raw catalog
choices retain provider-connection routing. The picker previews how each
selection affects the conversation's summarizer.

| Variable | Default | Description |
| --- | --- | --- |
| `OLLAMA_BASE_URL` | — | Origin of the Ollama server (for example, `http://localhost:11434`). The provider derives its `/v1` chat and native `/api` model endpoints. A non-empty value configures the Ollama backend. |
| `OLLAMA_MODEL` | `gemma4:e4b-mlx` | Ollama model. |
| `OLLAMA_API_KEY` | — | Optional Ollama API key. |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai` | OpenRouter origin. The provider derives `/api/v1` for chat and model discovery. Unlike Ollama, OpenRouter is a single hosted service, so this has a working default and never has to be set manually. |
| `OPENROUTER_MODEL` | `deepseek/deepseek-v4-flash-0731` | OpenRouter model. |
| `OPENROUTER_API_KEY` | — | OpenRouter API key. Setting `OPENROUTER_API_KEY` (or `OPENROUTER_MODEL`) alone is enough to configure OpenRouter — `OPENROUTER_BASE_URL` does not need to be exported. |

For explicit provider connections, `api_key` values in `config.toml` remain
authoritative. When omitted, `OLLAMA_API_KEY` or `OPENROUTER_API_KEY` fills the
connection based on its `provider` implementation kind. API keys and expanded
environment values are never written to logs.

### Paths

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_HOME` | `${HOME}/.ylva` | Ylva's home directly (chat store `chats/` and profiles `profiles/`). Set it to redirect Ylva's own state. See [persistence](./persistence.md#relocating-the-ylva-home-with-ylva_home). |

### Debug

| Variable | Default | Description |
| --- | --- | --- |
| `YLVA_REQUEST_DUMP_PATH` | — | When set, every outbound HTTP request is dumped, including model catalogs, Ollama metadata, chat/SSE, and web search. Request and response files share a timestamp and per-request id. A relative directory is resolved against the process working directory. Used by `make build-debug-and-run-with-logs`. |
| `YLVA_TOKEN_USAGE_DUMP_PATH` | — | When set, append one metadata-only JSONL record per model request to this file. Records pair the pre-send `cl100k_base` estimate of the serialized Chat Completions body with provider-reported input usage for that request when present. Each record includes its UTC request timestamp and session ID when associated with a persisted session (`null` for standalone utility requests). A relative path is resolved against the process working directory. No conversation content, credentials, or request payload is written. This is diagnostic data, not billed usage, and is independent of `YLVA_REQUEST_DUMP_PATH` and session persistence. |

The token usage dump is a single append-only JSONL file. Each record contains a
request correlation ID, UTC timestamp, nullable session ID, provider
implementation and connection, model, local estimated input-token count and estimate source, and nullable
`reported_input_tokens`/`reported_source` fields. OpenAI-compatible streams request
usage and recognize `usage.prompt_tokens` or `usage.input_tokens`; provider
support varies, so a missing or interrupted observation is `null`, never zero.
The local estimate tokenizes the exact serialized request JSON with
`cl100k_base`; it is a consistent comparison baseline, not a prediction of the
provider's model-native token count.

The value is a path template: the directory and filename stem are extracted and
the request is written as
`<stem>_<timestamp>_<dump_id>_request.jsonl`. Its first line contains the
request's `scheme`/`host`/`path`/`query`/`headers`; `Authorization`, `Cookie`,
and API-key-style header values are redacted as `<REDACTED>`, and so is every
header whose name ends in `-token` (matched case-insensitively — this covers
Brave's `X-Subscription-Token`). Its second line is
the compact JSON request body, or `null` for a request without a body. Base64
tool-result payloads and inline image/file data are replaced with stable
redaction markers while roles, IDs, media types, filenames, and other safe
metadata remain. Client-level default headers, such as Ylva's `User-Agent`, are
not included in the header projection. The live request is not changed.

The paired response is written as
`<stem>_<timestamp>_<dump_id>_response.<extension>`. The extension and file
contents follow the response `Content-Type`: `text/event-stream` uses `.jsonl`
with one line per SSE `data:` payload, `application/json` uses `.json` with the
original buffered bytes, `text/html` uses `.html`, `text/plain` uses `.txt`,
and common XML, JavaScript, image, and PDF types use their corresponding
extensions. Unknown buffered types use `.bin`. Buffered responses are written
as their original bytes rather than JSON-encoded strings. Transport failures
without response headers use `.jsonl` and are recorded as `[transport] ...`
markers.
