REPO_ROOT := $(realpath $(dir $(abspath $(firstword $(MAKEFILE_LIST)))))
QUIET     :=

ifeq ($(CARGO_TARGET_DIR),)
export CARGO_TARGET_DIR := $(REPO_ROOT)/target
endif

# Minimum line coverage for testable files; enforced by `make test-coverage`
# and `make test-coverage-missing`. `ui.rs` and `main.rs` are excluded because
# they require a live terminal and cannot be unit-tested without a full
# terminal emulator harness.
COVERAGE_THRESHOLD := 80

.PHONY: lint format \
	build-debug build-debug-and-run \
	build-release build-release-and-run build-debug-and-run-with-logs \
	test test-coverage test-coverage-missing \
	install tag-version \
	outdated-dependencies upgrade-dependencies

# Run cargo check/clippy and report all warnings
lint:
	cargo check \
	    $(if $(filter true,$(QUIET)),--quiet,) \
	    --all-targets --all-features
	cargo clippy \
	    $(if $(filter true,$(QUIET)),--quiet,) \
	    --all-targets --all-features

# Run cargo fmt to enforce consistent formatting
format:
	cargo fmt

# Build debug binary (output: target/debug/ylva)
build-debug:
	cargo build

# Build debug binary and launch it
build-debug-and-run:
	cargo run

# Build debug binary and launch it with loggin enabled
build-debug-and-run-with-logs:
	YLVA_REQUEST_DUMP_PATH=. cargo run --features artificial-tool-delay -- \
	    --log-file ylva_$(shell date +%s).log \
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
	cargo test $(if $(filter true,$(QUIET)),--quiet,)

# Run all tests and print a per-file coverage summary.
# Fails if total line coverage of testable files drops below $(COVERAGE_THRESHOLD).
# ui.rs, render.rs and main.rs are excluded: they require a live terminal (ratatui/crossterm)
# and cannot be unit-tested without a full terminal emulator harness.
# Requires: cargo install cargo-llvm-cov
#           rustup component add llvm-tools-preview
test-coverage:
	cargo llvm-cov \
	    $(if $(filter true,$(QUIET)),--quiet,) \
	    --ignore-filename-regex "(ui|render|main)\.rs" \
	    --fail-under-lines $(COVERAGE_THRESHOLD)

# Same as test-coverage but also prints uncovered line numbers per file.
# Useful for finding exactly which lines to target with new tests.
# Still enforces the coverage threshold — use this variant when you want the
# line-level hint and the build to fail below $(COVERAGE_THRESHOLD)%.
test-coverage-missing:
	cargo llvm-cov \
	    $(if $(filter true,$(QUIET)),--quiet,) \
	    --ignore-filename-regex "(ui|render|main)\.rs" \
	    --fail-under-lines $(COVERAGE_THRESHOLD) \
	    --show-missing-lines

# Show available dependency upgrades (within semver bounds)
outdated-dependencies:
	cargo update --dry-run

# Apply dependency upgrades to Cargo.lock (within semver bounds)
upgrade-dependencies:
	cargo update

# Tag HEAD with the version from Cargo.toml (e.g. v0.2.0)
tag-version:
	$(eval VERSION := $(shell grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/'))
	git tag v$(VERSION) HEAD
	@echo "Tagged HEAD as v$(VERSION)"
