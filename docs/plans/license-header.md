# Roll out the Apache-2.0 license header to all Rust source files

## Goal and scope

Prefix every `.rs` file with a standard Apache-2.0 copyright header owned by "Manuel Koch" for the
year 2026. The repository already ships an Apache-2.0 `LICENSE` at the repo root, but individual
source files carry no per-file licensing notice; this rollout adds the boilerplate header so the
licensing intent is explicit and machine-inspectable.

**Scope:** every `.rs` file in the main crate — `src/` and `tests/`. The `spikes/` crates are
scratch research artifacts and are explicitly excluded. No `.rs` file in the main crate currently
carries a header (verified: `grep -rl "Copyright"` across `.rs` files returns nothing outside
`target/`).

**Done means:**
- Every `.rs` file in the main crate (15 total: 7 under `src/`, 8 under `tests/`) begins with
  exactly the header block defined below on its very first line.
- No existing code, module doc comments, or file content is altered except for the inserted header.
- `make format`, `make lint`, `make test`, and `make test-coverage` all pass after the change.
- A verification step confirms no `.rs` file is missing the header.

**Key constraints:**
- The header text and its placement are prescribed below; do not reformat or reword it.
- For the two files that begin with an inner-doc comment (`//!`), the header goes *above* the
  `//!` block.
- No shebang, `build.rs`, or bench files exist, so nothing must stay above the header.

## Canonical header

Use exactly the following block (each line is a `//` comment; the block ends with one blank line):

```
// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

```

## Task checklist (ordered, independently verifiable)

1. **Enumerate targets** — `find src tests -name '*.rs' -print`. Expected count: 15 files.

2. **Add crate manifest license and author metadata** — in the `[package]` section of
   `Cargo.toml`, add `license = "Apache-2.0"` and `authors = ["Manuel Koch"]` so the manifest
   declares the same license and ownership as the source headers and root `LICENSE`.
   - **Verify:** a read of `Cargo.toml` shows both the `license` and `authors` fields.

3. **Insert the header** — prepend the canonical header to every file. Put it above the leading
   `//!` block in `src/agent/tools.rs` and `src/agent/tools/fs.rs`. Prefer a scripted edit (heredoc
   loop) to keep all insertions identical; skip any file that already starts with the header
   (idempotency guard).
   - **Verify:** every file's line 1 is `// Copyright © 2026 Manuel Koch`; the two `//!` files
     still retain their module doc comments right after the header.

4. **Run standard checks** — `make format`, `make lint`, `make test`, `make test-coverage`
   (≥80% threshold). Header-only additions must not affect coverage.
   - **Verify:** all four commands succeed.

5. **Documentation step and single commit** — this plan (`docs/plans/license-header.md`) is the
   source of truth; close it with a short "executed/status" note recording that the rollout is
   complete. (Note: this deviates from the repo's historical practice of deleting executed plan
   files; the decision here is to retain the closed plan.) Commit implementation + verification +
   doc update together in **one single commit** per "Docs are part of the change set" in
   `AGENTS.md`.

## Target files (15)

- `src/`: `agent.rs`, `agent/tools.rs`, `agent/tools/fs.rs`, `lib.rs`, `llm.rs`,
  `llm/openai_provider.rs`, `main.rs`
- `tests/`: `common.rs`, `common/mock_llm_server.rs`, `concurrent_steers.rs`, `empty_prompt.rs`,
  `http_provider.rs`, `interrupt.rs`, `steering.rs`, `tools_react.rs`
