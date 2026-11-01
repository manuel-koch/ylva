# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.35.0](https://github.com/manuel-koch/ylva/compare/v0.34.0...v0.35.0) - 2026-11-01

### Added

- Improve SSE parsing for EdenAI provider wrt thinking events
- Improve handling of reasoning-effort for EdenAI provider
- Improve sandbox detection of harmless bash command redirect
- Sent session-id with EdenAI requests to benefit from sticky
- Add EdenAI provider implementation
- Show popup dialog with all agent conversations ( root and

### Documentation

- Fix typos

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
