# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.39.0](https://github.com/manuel-koch/ylva/compare/v0.38.0...v0.39.0) - 2026-11-27

### Added

- Refactor grant-dialog layout

### Documentation

- *(setup)* Add note for graphify tool
- *(setup)* Describe required cargo tooling for developer setup
- *(release-notes)* Allow using subagent to get more info from commit

### Fixed

- *(search)* Bump tool timeout for long running searches
- *(llm)* Preserve all tool-call stream deltas
- Improve rendering of tool errors from a resumed session
- Improve rendering of markdown paragraphs in lists
- Refactor grant-dialog to explicitly reload grant.toml after

## [0.38.0](https://github.com/manuel-koch/ylva/compare/v0.37.0...v0.38.0) - 2026-11-26

### Added

- Implement approval verdict trail
- Apply changes to align implementation to the styleguide
- Apply changes to align implementation to the styleguide
- Tool grants can be edited
- User can grant tool calls temporary or persist the decision in
- Export sessions to json or markdown format
- [**breaking**] Refactor tool configuration

### Documentation

- Update CONTEXT.md with "tool approval trail" term
- Introduce documentation for logging

### Fixed

- Improve wildcard glob rules to not match accidentally on dotted
- Fix edit tool bubble expanding on error
- Improve model score matching on existing models
- Improve cursor handling when editing long draft message
- Improve handling of tab characters in TUI bubble markdown rendering

## [0.37.0](https://github.com/manuel-koch/ylva/compare/v0.36.0...v0.37.0) - 2026-11-13

### Added

- Handle slash-command in initial prompt in TUI
- Allow switching engine on startup, list all avail engines/models

## [0.36.0](https://github.com/manuel-koch/ylva/compare/v0.35.0...v0.36.0) - 2026-11-10

### Added

- Add more bash grammer
- Check for latest released version

## [0.35.0](https://github.com/manuel-koch/ylva/compare/v0.34.0...v0.35.0) - 2026-11-10

### Added

- Handle git worktree paths too to discover sessions
- Model picker dialog shows prices and scores if available
- [**breaking**] add /status popup, session deletion and related store changes
- Refactor model picker and footer rendering of selected model
- Allow configuring max_output_tokens on engines to constrain LLM
- Show triggered tool calls in agents popup for root and subagent
- Align /agents popup sizing to /models and /sessions popup
- Change keybindings to not collide with standard cursor movement
- Refactor footer rendering for narrow terminal
- Improve SSE parsing for EdenAI provider wrt thinking events
- Improve handling of reasoning-effort for EdenAI provider
- Improve sandbox detection of harmless bash command redirect
- Sent session-id with EdenAI requests to benefit from sticky
- Add EdenAI provider implementation
- Show popup dialog with all agent conversations ( root and

### Documentation

- Improve docs after review
- Improve docs after review
- Improve docs after review
- Improve docs after review
- rewrite persistence reference and add implementation-notes
- Update bash rule examples
- Update sample bash rule configuration
- Fix typos

### Fixed

- Ensure compaction will always use a sufficient max_output_tokens
- Allow manual focus switch on buttons via tab key to overrule mouse

## [0.34.0](https://github.com/manuel-koch/ylva/compare/v0.33.0...v0.34.0) - 2026-10-28

### Added

- Allow auto-expand of named tool bubbles in TUI
- Refactor approval/clarify dialogs and related keybindings
- Refactor how bash tool runs with non-zero-exitcode are rendered in
- Provider connection has favorite_models and allowed_models
- Enhance builtin tool grammar for some handpicked tools
- Improve notice after changing provider/model using the /models
- Improve footer rendering for narrow terminal
- Block pasting too much data into draft message
- Handle SIGHUB and refactor terminal restore functionality
- Native web_fetch attempts to autodetect content-type when response
- Improve bash command grammar detection
- Improve CLI output for tool invocations

### Documentation

- Update release-notes-workflow guideline for LLM wrt to git usage

### Fixed

- Improve sluggish scrolling and text selection in huge draft message

## [0.33.0](https://github.com/manuel-koch/ylva/compare/v0.32.0...v0.33.0) - 2026-10-26

### Added

- Show bash tool running duration as full seconds
- Support colored rendering based on theme in TUI
- [**breaking**] Refactor bubble rendering for tui

### Documentation

- Update release-notes-workflow to account for conventional-commits

### Fixed

- Improve rendering of markdown sub-list bullets
- Fix concurrent approval/clarify handling to ensure original FIFO

## [0.32.0](https://github.com/manuel-koch/ylva/compare/v0.31.1...v0.32.0) - 2026-10-24

### Added

- Improve request retry handling
- Use app attribution for openrouter provider
- Add plan for openrouter attribution

## [0.31.1](https://github.com/manuel-koch/ylva/releases/tag/v0.31.1) - 2026-10-23

### Documentation

- Update readme and release plan
- Refine goals and implementation status of ylva project

### Fixed

- Drop mock provider from normal use cases, use it in tests explicitly
- Only run trusted workspace configs
- Improve resolving canonical path wrt symlink handling
