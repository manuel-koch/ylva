# Copyright © 2026 Manuel Koch
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

REPO_ROOT := $(realpath $(dir $(abspath $(firstword $(MAKEFILE_LIST)))))
VERBOSE   := false

ifeq ($(CARGO_TARGET_DIR),)
export CARGO_TARGET_DIR := $(REPO_ROOT)/target
endif

# Minimum aggregate line coverage; enforced by `make test-coverage`
# and `make test-coverage-missing` without project-specific file exclusions.
COVERAGE_THRESHOLD := 80

.PHONY: render-all-diagrams \
	lint audit format format-check format-rust format-markdown \
	install-git-hooks uninstall-git-hooks test-git-conventions \
	graphify-update graphify-watch \
    clean \
	build-debug build-debug-and-run \
	build-release build-release-and-run build-debug-and-run-with-logs \
	test test-e2e-persistence test-coverage test-coverage-missing \
	install \
	outdated-dependencies upgrade-dependencies

docs/diagrams/%.svg: docs/diagrams/%.mmd
	docker run --rm --network none \
	    --user "$$(id -u):$$(id -g)" \
	    --env PUPPETEER_CACHE_DIR=/home/mermaidcli/.cache/puppeteer \
	    --volume "$(REPO_ROOT)/docs/diagrams:/data" \
	    minlag/mermaid-cli \
	    -i "$(notdir $<)" -o "$(notdir $@)"

render-all-diagrams: $(patsubst %.mmd,%.svg,$(wildcard docs/diagrams/*.mmd))

# Run cargo check/clippy and report all warnings
lint:
	cargo check \
	    $(if $(filter true,$(strip $(VERBOSE))),,--quiet) \
	    --all-targets --all-features
	cargo clippy \
 	    $(if $(filter true,$(strip $(VERBOSE))),,--quiet) \
	    --all-targets --all-features

audit:
	cargo audit --quiet

# Run rumdl to enforce consistent formatting of Markdown files
format-markdown:
	rumdl fmt --quiet $(REPO_ROOT)

# Run cargo fmt to enforce consistent formatting of Rust files
format-rust:
	cargo fmt

# Enforce consistent formatting
format: format-rust format-markdown

# Verify formatting without modifying files (used by CI)
format-check:
	cargo fmt --check
	rumdl check --quiet $(REPO_ROOT)

# Install the lefthook git hooks and the commit message template
install-git-hooks:
	lefthook install
	git config --local commit.template .gitmessage

uninstall-git-hooks:
	lefthook uninstall
	git config --local --unset commit.template || true

# Verify the git convention check script
test-git-conventions:
	scripts/test-git-conventions.sh

graphify-update:
	graphify update .
	graphify export html
	graphify export callflow-html

graphify-watch:
	graphify watch

clean:
	cargo clean -p ylva --target-dir "$(CARGO_TARGET_DIR)"

show-debug-executable:
	@echo "$(CARGO_TARGET_DIR)/debug/ylva"

show-release-executable:
	@echo "$(CARGO_TARGET_DIR)/release/ylva"

# Build debug binary (output: target/debug/ylva)
build-debug:
	cargo build

# Build debug binary and launch it
build-debug-and-run:
	cargo run

# Build debug binary and launch it with logging and request-dump enabled
build-debug-and-run-with-logs:
	YLVA_REQUEST_DUMP_PATH=tmp/ylva.json \
	    cargo run --features artificial-tool-delay -- \
	        --log-file tmp/ylva_$(shell date +%s).log \
	        --log-level trace

# Build optimized release binary (output: ${CARGO_TARGET_DIR}/release/ylva)
build-release:
	cargo build --release

# Build optimized release binary
build-release-and-run: build-release
	$(CARGO_TARGET_DIR)/release/ylva

# Build optimized release binary and install it `~/.cargo/bin`
install:
	cargo install --path .

# Run all unit and integration tests
test:
	cargo test $(if $(filter true,$(strip $(VERBOSE))),,--quiet)

# Run e2e persistence tests
test-e2e-persistence:
	. $(REPO_ROOT)/tests/e2e/ollama-local/.envrc && \
	    cargo test --test store_persistence_e2e persistence_e2e_ollama_local -- --ignored --no-capture
	. $(REPO_ROOT)/tests/e2e/ollama-cloud/.envrc && \
	    cargo test --test store_persistence_e2e persistence_e2e_ollama_cloud -- --ignored --no-capture
	. $(REPO_ROOT)/tests/e2e/openrouter/.envrc && \
	    cargo test --test store_persistence_e2e persistence_e2e_openrouter -- --ignored --no-capture

# Run all tests and print a per-file coverage summary.
# Fails if aggregate line coverage drops below $(COVERAGE_THRESHOLD).
# Includes renderer and binary source; cargo-llvm-cov's default filters still apply.
# Requires: cargo install cargo-llvm-cov
#           rustup component add llvm-tools-preview
test-coverage:
	cargo llvm-cov \
	    $(if $(filter true,$(strip $(VERBOSE))),,--quiet) \
	    --fail-under-lines $(COVERAGE_THRESHOLD)

# Same as test-coverage but also prints uncovered line numbers per file.
# Useful for finding exactly which lines to target with new tests.
# Still enforces the coverage threshold — use this variant when you want the
# line-level hint and the build to fail below $(COVERAGE_THRESHOLD)%.
test-coverage-missing:
	cargo llvm-cov \
	    $(if $(filter true,$(strip $(VERBOSE))),,--quiet) \
	    --fail-under-lines $(COVERAGE_THRESHOLD) \
	    --show-missing-lines

# Show available dependency upgrades (within semver bounds)
outdated-dependencies:
	cargo update --dry-run

# Apply dependency upgrades to Cargo.lock (within semver bounds)
upgrade-dependencies:
	cargo update
