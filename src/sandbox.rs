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

//! Sandbox: path-based tool approval policy.
//!
//! [`SandboxContext::check_path`] returns `None` (abstain) or an explicit
//! `Some(`[`ApprovalCheck`]`)` verdict. Tools apply their own default when
//! the sandbox abstains.
//!
//! Per-tool rule lists are configurable via a `[tools.<name>]` TOML table
//! (see [`SandboxRuleConfig`] and `docs/configuration.md`'s "Sandbox"
//! section) that is parsed by `crate::config::profile::ProfileConfig` and
//! wired into a tool's [`SandboxContext`] by
//! [`crate::agent::tools::default_registry`] via
//! [`SandboxContext::with_configured_rules`]. When a tool has no such table
//! configured at any resolved layer, [`SandboxContext`] falls back to the
//! unconditional built-in floor: `outside_workspace` → `UserApprovalRequired`
//! and `dot_path` → `UserApprovalRequired`.

mod bash_command_extractor;
pub(crate) mod network_target_pattern;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use glob::Pattern;
use network_target_pattern::NetworkTargetPattern;
use serde::Deserialize;

/// The verdict from a sandbox approval check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalCheck {
    /// The tool call may proceed without asking the user.
    AutoApproved,
    /// The tool call must be rejected automatically; show `reason` to the user/model.
    AutoDenied { reason: String },
    /// The tool call requires explicit user approval.
    UserApprovalRequired,
}

/// A single rule in the sandbox rule list.
#[derive(Debug, Clone)]
pub struct SandboxRule {
    /// The verdict this rule produces when matched.
    pub verdict: ApprovalCheck,
    /// Glob pattern matched against the canonicalized absolute path.
    pub pattern: Pattern,
}

/// Default reason shown when the built-in (or a configured, reason-less)
/// `outside_workspace` rule denies a path.
const OUTSIDE_WORKSPACE_REASON: &str = "Access denied by security policy: path is outside the \
                                         workspace root; only paths within the workspace are \
                                         accessible";

/// Default reason shown when the built-in (or a configured, reason-less)
/// `dot_path` rule denies a path.
const DOT_PATH_REASON: &str =
    "Access denied by security policy: dot-files and dot-directories are not accessible";

/// Default reason shown when a configured, reason-less `cross_filesystem`
/// rule denies a path.
const CROSS_FILESYSTEM_REASON: &str = "Access denied by security policy: path resides on a \
                                       different filesystem (device) than the workspace root";

/// Default reason shown when the built-in (or a configured, reason-less)
/// `backgrounded` rule denies a `bash` command.
const BACKGROUNDED_REASON: &str = "Access denied by security policy: backgrounded (`&`) shell \
                                    commands are not allowed";

/// Default reason shown when a configured, reason-less
/// `variable_assignment_prefix` rule denies a `bash` command.
const VARIABLE_ASSIGNMENT_PREFIX_REASON: &str =
    "Access denied by security policy: shell commands with a leading variable \
     assignment (e.g. `FOO=bar git status`) are not allowed";

/// Default reason shown when a configured, reason-less `unquoted_glob` rule
/// denies a `bash` command.
const UNQUOTED_GLOB_REASON: &str = "Access denied by security policy: shell commands with an \
                                     unquoted glob (`*`, `?`, or `[...]`) argument are not \
                                     allowed";

/// Default reason shown when a configured, reason-less
/// `unquoted_brace_expansion` rule denies a `bash` command.
const UNQUOTED_BRACE_EXPANSION_REASON: &str =
    "Access denied by security policy: shell commands with an unquoted brace \
     expansion (`{...,...}`) argument are not allowed";

/// Default reason shown when a configured, reason-less `network_request`
/// rule denies a network-tool call.
const NETWORK_REQUEST_REASON: &str = "Access denied by security policy: network request \
                                      matched a configured sandbox rule";

/// Opaque match target for the rules appended to a network tool's context
/// (e.g. `--auto-approve-web-search`'s `**` catch-all). Network calls have
/// no path or command string to match a glob against, so appended rules are
/// matched against this constant instead — see
/// [`SandboxContext::check_network_request`].
const NETWORK_REQUEST_MATCH_KEY: &str = "network_request";

/// Default reason shown when a configured, reason-less
/// [`SandboxRuleConfig::PathGlob`] rule denies a path.
const CONFIGURED_PATH_GLOB_REASON: &str =
    "Access denied by security policy: path matched a configured sandbox rule";

/// Default reason shown when a configured, reason-less
/// [`SandboxRuleConfig::CommandGlob`] rule denies a `bash` command.
const CONFIGURED_COMMAND_GLOB_REASON: &str = "Access denied by security policy: shell command \
                                               matched a configured sandbox rule";

/// One of the structural sandbox checks that ship as Rust code
/// (`SandboxContext`'s hardcoded default floor, plus the opt-in
/// [`NamedSandboxRule::CrossFilesystem`]) but may also be referenced
/// by name from a `[tools.<name>]` config table's `rules` list, so a user can
/// reorder or change their verdict without reimplementing the underlying
/// comparison logic. They fall into three families, each restricted to the
/// tool kinds it applies to (enforced at config load by
/// [`SandboxRuleConfig::is_path_only`]/[`is_bash_only`]/[`is_network_only`]):
/// the three path rules ([`NamedSandboxRule::OutsideWorkspace`],
/// [`NamedSandboxRule::DotPath`], [`NamedSandboxRule::CrossFilesystem`]) are
/// valid for every tool except `bash` and the network tools; the four
/// `bash`-only command-structure rules, evaluated per extracted command by
/// [`SandboxContext::check_command`]/[`bash_command_extractor`]; and
/// [`NamedSandboxRule::NetworkRequest`], valid only under a network tool's
/// config table (today `[tools.web_search]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedSandboxRule {
    /// Matches any path that normalizes outside the workspace root.
    OutsideWorkspace,
    /// Matches any content-tool path whose relative-to-root form has a
    /// component starting with `.` (unless `allow_dot_paths` bypasses it).
    DotPath,
    /// Matches any path whose filesystem device ID (unix `st_dev`) differs
    /// from the canonical workspace root's, using the deepest existing
    /// ancestor as the anchor when the target itself does not exist.
    /// Fires only on a positively established device difference: unknown
    /// metadata on either side (or any non-unix target) never matches.
    /// Opt-in only — not part of the default floor.
    CrossFilesystem,
    /// Matches a `bash` command immediately followed by a bare `&`
    /// (backgrounding).
    Backgrounded,
    /// Matches a `bash` command with one or more leading
    /// `variable_assignment` children (e.g. `FOO=bar git status`).
    VariableAssignmentPrefix,
    /// Matches a `bash` command with an unquoted `*`, `?`, or `[...]` in any
    /// argv token.
    UnquotedGlob,
    /// Matches a `bash` command with an unquoted `{...,...}` brace
    /// expansion in any argv token.
    UnquotedBraceExpansion,
    /// Matches a call of a network tool that has no path or command to
    /// structurally inspect (`web_search`, `web_fetch`): the rule is the
    /// tool's per-call approval switch (e.g. `verdict = "auto_approved"`
    /// without the CLI flag). An optional `target` parameter scopes the
    /// verdict to request targets whose `scheme://host[:port]` key matches
    /// its [`NetworkTargetPattern`] (e.g. `https://docs.rs`,
    /// `https://*.example.com`, `*://host`); absent
    /// `target` matches every call. Valid only under
    /// `[tools.web_search]`/`[tools.web_fetch]`/
    /// `[agents.<type>.tools.web_search]`/`[agents.<type>.tools.web_fetch]`.
    NetworkRequest,
}

/// The serde-friendly counterpart of [`ApprovalCheck`]'s variants, without
/// the `AutoDenied` variant's mandatory `reason` field (carried as a
/// sibling `reason` field on [`SandboxRuleConfig`] instead, so it can
/// default per-rule via [`SandboxRuleConfig::to_approval_check`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalVerdictConfig {
    AutoApproved,
    AutoDenied,
    UserApprovalRequired,
}

impl ApprovalVerdictConfig {
    /// Convert into an [`ApprovalCheck`], falling back to `default_reason`
    /// when `reason` is `None` for an `AutoDenied` verdict.
    fn to_approval_check(&self, reason: Option<&str>, default_reason: &str) -> ApprovalCheck {
        match self {
            ApprovalVerdictConfig::AutoApproved => ApprovalCheck::AutoApproved,
            ApprovalVerdictConfig::AutoDenied => ApprovalCheck::AutoDenied {
                reason: reason.unwrap_or(default_reason).to_string(),
            },
            ApprovalVerdictConfig::UserApprovalRequired => ApprovalCheck::UserApprovalRequired,
        }
    }
}

/// A single `[tools.bash]`-only argv-shape rule: a sequence of literal argv
/// tokens, optionally ending in a bare `*` (matches exactly one more argv
/// token) or bare `**` (matches one or more more argv tokens). No embedded/
/// mid-pattern globbing (YAGNI, v1).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommandGlobTail {
    /// No trailing wildcard: the pattern's literal tokens must equal `argv`
    /// exactly, length included.
    None,
    /// Trailing bare `*`: matches when `argv` has exactly one more token
    /// than the literal prefix, unless that one extra token contains an
    /// unquoted glob/brace character (in which case this rule abstains,
    /// deferring to `unquoted_glob`/`unquoted_brace_expansion`).
    Single,
    /// Trailing bare `**`: matches when `argv` has one or more extra tokens
    /// beyond the literal prefix; never abstains on unquoted glob/brace
    /// characters in the tail.
    Multi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandGlobPattern {
    literal_prefix: Vec<String>,
    tail: CommandGlobTail,
}

impl CommandGlobPattern {
    /// Returns `true` when `tokens` has a `*` or `**` element before the
    /// last position. `from_tokens` only recognizes a *trailing* `*`/`**`
    /// as a wildcard; an earlier occurrence would otherwise silently
    /// degrade to a literal (effectively never-matching) argument instead
    /// of being treated as a wildcard. This is the single source of truth
    /// for that check, shared by `from_tokens`'s `debug_assert!` and
    /// [`SandboxRuleConfig::has_misplaced_command_glob_wildcard`] (used by
    /// `crate::config::profile::validate_tool_sandbox_rules`'s hard startup
    /// error and [`ResolvedRule::from_config`]'s defense-in-depth check for
    /// callers that bypass profile loading).
    fn has_misplaced_wildcard(tokens: &[String]) -> bool {
        tokens
            .iter()
            .take(tokens.len().saturating_sub(1))
            .any(|token| token == "*" || token == "**")
    }

    fn from_tokens(tokens: &[String]) -> Self {
        debug_assert!(
            !Self::has_misplaced_wildcard(tokens),
            "CommandGlobPattern::from_tokens received a `*`/`**` that isn't the last token \
             ({tokens:?}); callers must reject this before constructing a pattern (see \
             SandboxRuleConfig::has_misplaced_command_glob_wildcard)"
        );
        match tokens.last().map(String::as_str) {
            Some("**") => CommandGlobPattern {
                literal_prefix: tokens[..tokens.len() - 1].to_vec(),
                tail: CommandGlobTail::Multi,
            },
            Some("*") => CommandGlobPattern {
                literal_prefix: tokens[..tokens.len() - 1].to_vec(),
                tail: CommandGlobTail::Single,
            },
            _ => CommandGlobPattern {
                literal_prefix: tokens.to_vec(),
                tail: CommandGlobTail::None,
            },
        }
    }

    /// Returns `true` when this pattern matches `argv`'s token texts,
    /// applying the trailing-`*` unquoted-glob/brace abstention described on
    /// [`CommandGlobTail::Single`].
    fn matches(&self, argv: &[bash_command_extractor::ArgvToken]) -> bool {
        if argv.len() < self.literal_prefix.len() {
            return false;
        }
        let prefix_matches = self
            .literal_prefix
            .iter()
            .zip(argv.iter())
            .all(|(literal, token)| literal == &token.text);
        if !prefix_matches {
            return false;
        }
        match self.tail {
            CommandGlobTail::None => argv.len() == self.literal_prefix.len(),
            CommandGlobTail::Single => {
                if argv.len() != self.literal_prefix.len() + 1 {
                    return false;
                }
                let tail_token = &argv[self.literal_prefix.len()];
                !bash_command_extractor::token_has_unquoted_glob(tail_token)
                    && !bash_command_extractor::token_has_unquoted_brace_expansion(tail_token)
            }
            CommandGlobTail::Multi => argv.len() > self.literal_prefix.len(),
        }
    }
}

/// One entry of a `[tools.<name>]` table's `rules` list — either a reference
/// to one of the built-in [`NamedSandboxRule`] checks (with a configurable
/// verdict), a custom glob-pattern path rule (the config-parseable form of
/// [`SandboxRule`]), or (`[tools.bash]`/`[agents.<type>.tools.bash]` only) an
/// argv-shape [`CommandGlobPattern`] rule. Distinguished on deserialization
/// by the presence of `rule` (named), `pattern` (path glob), or `command`
/// (argv glob). `reason` is optional for `verdict = "auto_denied"` and
/// defaults to the same message the built-in floor uses for that rule (or a
/// generic message for a custom rule) when omitted.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SandboxRuleConfig {
    /// `{ rule = "<named_rule>", verdict = "...", reason = "...", target = "..." }`.
    Named {
        rule: NamedSandboxRule,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
        /// Optional [`NetworkTargetPattern`] matched against the request's
        /// `scheme://host[:port]` match key (see
        /// [`SandboxContext::check_network_request`]); absent `target` matches
        /// any request target. Only meaningful on
        /// [`NamedSandboxRule::NetworkRequest`].
        #[serde(default)]
        target: Option<String>,
    },
    /// `{ pattern = "...", verdict = "...", reason = "..." }`.
    PathGlob {
        pattern: String,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
    },
    /// `{ command = ["git", "reflog", "*"], verdict = "...", reason = "..." }`.
    /// Only valid under `[tools.bash]`/`[agents.<type>.tools.bash]`.
    CommandGlob {
        command: Vec<String>,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
    },
}

impl SandboxRuleConfig {
    /// Returns `true` when this is the tool-restricted [`SandboxRuleConfig::CommandGlob`]
    /// variant, or a [`SandboxRuleConfig::Named`] variant referencing one of
    /// the four `bash`-only [`NamedSandboxRule`] variants. Used by
    /// `crate::config::profile::ProfileConfig`'s startup validation to
    /// reject the wrong rule kind under the wrong tool.
    pub fn is_bash_only(&self) -> bool {
        match self {
            SandboxRuleConfig::CommandGlob { .. } => true,
            SandboxRuleConfig::Named { rule, .. } => matches!(
                rule,
                NamedSandboxRule::Backgrounded
                    | NamedSandboxRule::VariableAssignmentPrefix
                    | NamedSandboxRule::UnquotedGlob
                    | NamedSandboxRule::UnquotedBraceExpansion
            ),
            SandboxRuleConfig::PathGlob { .. } => false,
        }
    }

    /// Returns `true` when this is a path-only rule kind: [`SandboxRuleConfig::PathGlob`],
    /// or a [`SandboxRuleConfig::Named`] variant referencing one of the three
    /// path [`NamedSandboxRule`] variants
    /// ([`NamedSandboxRule::OutsideWorkspace`], [`NamedSandboxRule::DotPath`],
    /// [`NamedSandboxRule::CrossFilesystem`]).
    pub fn is_path_only(&self) -> bool {
        match self {
            SandboxRuleConfig::PathGlob { .. } => true,
            SandboxRuleConfig::Named { rule, .. } => matches!(
                rule,
                NamedSandboxRule::OutsideWorkspace
                    | NamedSandboxRule::DotPath
                    | NamedSandboxRule::CrossFilesystem
            ),
            SandboxRuleConfig::CommandGlob { .. } => false,
        }
    }

    /// Returns `true` when this is a network-tool-only rule: a
    /// [`SandboxRuleConfig::Named`] variant referencing
    /// [`NamedSandboxRule::NetworkRequest`]. Valid only under a network
    /// tool's config table — `[tools.web_search]`/`[tools.web_fetch]` /
    /// `[agents.<type>.tools.web_search]`/`[agents.<type>.tools.web_fetch]`.
    /// Used by
    /// `crate::config::profile::ProfileConfig`'s startup validation to
    /// reject the rule under every other tool, and vice versa.
    pub fn is_network_only(&self) -> bool {
        matches!(
            self,
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                ..
            }
        )
    }

    /// Returns `true` when this rule carries a `target` parameter. Only
    /// meaningful on [`NamedSandboxRule::NetworkRequest`]; used by
    /// `crate::config::profile::ProfileConfig`'s startup validation to reject
    /// `target` on every other rule kind (there is nothing to match it
    /// against there).
    pub fn has_target(&self) -> bool {
        match self {
            SandboxRuleConfig::Named { target, .. } => target.is_some(),
            SandboxRuleConfig::PathGlob { .. } | SandboxRuleConfig::CommandGlob { .. } => false,
        }
    }

    /// Returns this rule's `target` parameter, when it carries one. Used by
    /// `crate::config::profile::ProfileConfig`'s startup validation to
    /// compile-check the pattern (see [`NetworkTargetPattern::parse`]).
    pub fn target(&self) -> Option<&str> {
        match self {
            SandboxRuleConfig::Named { target, .. } => target.as_deref(),
            SandboxRuleConfig::PathGlob { .. } | SandboxRuleConfig::CommandGlob { .. } => None,
        }
    }

    /// Returns `true` when this is a [`SandboxRuleConfig::CommandGlob`] whose
    /// `command` list has a `*` or `**` token anywhere but the last
    /// position. `CommandGlobPattern::from_tokens` only recognizes `*`/`**`
    /// as the trailing token — an earlier occurrence is otherwise silently
    /// treated as a literal argument (matching a command that passes a
    /// literal `*`/`**`, which never happens in practice), turning the rule
    /// into a silent no-op. Used by `crate::config::profile`'s startup
    /// validation to reject this footgun instead.
    pub fn has_misplaced_command_glob_wildcard(&self) -> bool {
        match self {
            SandboxRuleConfig::CommandGlob { command, .. } => {
                CommandGlobPattern::has_misplaced_wildcard(command)
            }
            SandboxRuleConfig::Named { .. } | SandboxRuleConfig::PathGlob { .. } => false,
        }
    }
}

/// An entry in [`SandboxContext`]'s resolved per-tool rule list: either a
/// named structural check (re-evaluated dynamically against the current
/// `workspace_root`/path) or a fixed glob pattern. Built from
/// [`SandboxRuleConfig`] by [`SandboxContext::with_configured_rules`]; kept
/// distinct from the public [`SandboxRule`] type (which stays glob-only) so
/// that `SandboxRule`'s existing field shape — relied on by pre-existing
/// tests and by the `--auto-approve-write`/`--auto-approve-edit` catch-all
/// rules — is unaffected by this addition.
#[derive(Debug, Clone)]
enum ResolvedRule {
    Named {
        rule: NamedSandboxRule,
        verdict: ApprovalCheck,
        /// Compiled [`NetworkTargetPattern`] for
        /// [`NamedSandboxRule::NetworkRequest`]: `None` matches every request
        /// target.
        target: Option<NetworkTargetPattern>,
    },
    PathGlob {
        pattern: Pattern,
        verdict: ApprovalCheck,
    },
    CommandGlob {
        command: CommandGlobPattern,
        verdict: ApprovalCheck,
    },
}

/// Error produced by [`ResolvedRule::from_config`] when a
/// [`SandboxRuleConfig`] can't be resolved into a working rule.
/// [`SandboxContext::with_configured_rules`] treats every variant the same
/// way: skip the rule and `tracing::warn!`, since a single malformed rule
/// must not silently disable an entire tool's protection.
#[derive(Debug, thiserror::Error)]
enum ResolvedRuleError {
    #[error(transparent)]
    InvalidGlobPattern(#[from] glob::PatternError),
    #[error(transparent)]
    InvalidNetworkTargetPattern(#[from] network_target_pattern::NetworkTargetPatternError),
    /// A `command` rule's `*`/`**` wildcard isn't the last element — see
    /// [`SandboxRuleConfig::has_misplaced_command_glob_wildcard`].
    /// `crate::config::profile::validate_tool_sandbox_rules` already
    /// rejects this as a hard startup error for profile-loaded rules; this
    /// is defense in depth for callers that build [`SandboxRuleConfig`]s
    /// directly and pass them to [`SandboxContext::with_configured_rules`]
    /// without going through profile loading.
    #[error(
        "`*`/`**` wildcard is not the last element of `command`; it would silently degrade to \
         a literal (never-matching) argument instead of a wildcard"
    )]
    MisplacedCommandGlobWildcard,
}

impl ResolvedRule {
    fn from_config(config: SandboxRuleConfig) -> Result<Self, ResolvedRuleError> {
        if config.has_misplaced_command_glob_wildcard() {
            return Err(ResolvedRuleError::MisplacedCommandGlobWildcard);
        }
        Ok(match config {
            SandboxRuleConfig::Named {
                rule,
                verdict,
                reason,
                target,
            } => {
                let default_reason = match rule {
                    NamedSandboxRule::OutsideWorkspace => OUTSIDE_WORKSPACE_REASON,
                    NamedSandboxRule::DotPath => DOT_PATH_REASON,
                    NamedSandboxRule::CrossFilesystem => CROSS_FILESYSTEM_REASON,
                    NamedSandboxRule::Backgrounded => BACKGROUNDED_REASON,
                    NamedSandboxRule::VariableAssignmentPrefix => VARIABLE_ASSIGNMENT_PREFIX_REASON,
                    NamedSandboxRule::UnquotedGlob => UNQUOTED_GLOB_REASON,
                    NamedSandboxRule::UnquotedBraceExpansion => UNQUOTED_BRACE_EXPANSION_REASON,
                    NamedSandboxRule::NetworkRequest => NETWORK_REQUEST_REASON,
                };
                ResolvedRule::Named {
                    rule,
                    verdict: verdict.to_approval_check(reason.as_deref(), default_reason),
                    target: target
                        .map(|pattern| NetworkTargetPattern::parse(&pattern))
                        .transpose()?,
                }
            }
            SandboxRuleConfig::PathGlob {
                pattern,
                verdict,
                reason,
            } => ResolvedRule::PathGlob {
                pattern: Pattern::new(&pattern)?,
                verdict: verdict.to_approval_check(reason.as_deref(), CONFIGURED_PATH_GLOB_REASON),
            },
            SandboxRuleConfig::CommandGlob {
                command,
                verdict,
                reason,
            } => ResolvedRule::CommandGlob {
                command: CommandGlobPattern::from_tokens(&command),
                verdict: verdict
                    .to_approval_check(reason.as_deref(), CONFIGURED_COMMAND_GLOB_REASON),
            },
        })
    }
}

/// Shared workspace sandbox. Holds the workspace root used to evaluate
/// path-based rules.
pub struct Sandbox {
    /// Absolute path of the workspace root (cwd at startup).
    pub workspace_root: PathBuf,
}

impl Sandbox {
    /// Create a new sandbox rooted at the current working directory.
    ///
    /// The workspace root is canonicalized (symlinks resolved) so that
    /// `check_path` comparisons are consistent on platforms where temp
    /// directories live behind symlinks (e.g. macOS `/private/var/…`).
    pub fn new() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Sandbox {
            workspace_root: std::fs::canonicalize(&cwd).unwrap_or(cwd),
        }
    }

    /// Create a sandbox rooted at an explicit path.
    ///
    /// The path is canonicalized for the same reason as in [`Sandbox::new`].
    /// Falls back to the raw path when canonicalization fails (e.g. the
    /// directory does not yet exist).
    pub fn with_root(root: PathBuf) -> Self {
        Sandbox {
            workspace_root: std::fs::canonicalize(&root).unwrap_or(root),
        }
    }
}

/// Collapse `path` to a form relative to `base` when `path` is absolute and
/// falls inside `base`'s directory tree; returns `None` otherwise (`path` is
/// already relative, or lies outside `base`).
///
/// Used by [`crate::agent::tools::ToolRegistry::create_call`] to rewrite
/// absolute tool-call `path`/`pattern` arguments — some LLMs emit these even
/// where a relative path would do just as well — to their cwd-relative form
/// before dispatch, so both the executed argument and the on-screen
/// tool-call rendering are short and workspace-portable. `base` is expected
/// to be the process's actual current working directory, not necessarily a
/// [`Sandbox`]'s configured `workspace_root`: relative paths in tool
/// execution (e.g. `std::fs::read`) resolve against cwd, and the two need
/// not coincide (e.g. in tests that point a sandbox at a tempdir without
/// changing the process's cwd).
///
/// Symlinked prefixes (e.g. macOS `/var` → `/private/var`) are resolved the
/// same way as [`SandboxContext::check_path`] before comparing against
/// `base`. Works for glob patterns too: wildcard segments (`**`, `*.rs`) are
/// treated as opaque path components and passed through unchanged in the
/// returned tail.
pub fn to_relative_path(path: &str, base: &Path) -> Option<String> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return None;
    }
    let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
    let normalized = SandboxContext::normalize(&base, candidate);
    let effective = SandboxContext::canonicalize_best_effort(&normalized);
    let relative = effective.strip_prefix(&base).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Expand the path tokens a model may quote verbatim in a path-like tool
/// argument: `$NAME`/`${NAME}` environment variables and a leading `~`.
///
/// This is plain string substitution performed in Rust — never a shell — and
/// it runs **before** [`to_relative_path`] and before any sandbox approval
/// check, so the sandbox sees only the expanded, canonicalized path. It never
/// bypasses approval: an expanded path outside the workspace still hits the
/// `outside_workspace` approval floor.
///
/// Rules:
///
/// - `$NAME` and `${NAME}` tokens (`NAME` = `[A-Za-z_][A-Za-z0-9_]*`) are
///   replaced by `lookup(NAME)` when it is defined, and left literal when it
///   is not — a literal `$` in a file name survives.
/// - Substitution runs once: substituted values are never rescanned for
///   further tokens.
/// - After environment substitution, a leading `~` or `~/` expands through
///   `lookup("HOME")`. `~user/…` is deliberately unsupported and stays
///   untouched.
/// - The `lookup` parameter keeps this helper free of process-environment
///   access, so unit tests never mutate the process environment.
pub fn expand_path_tokens(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    expand_leading_tilde(&substitute_environment_variables(value, lookup), lookup)
}

/// Replace every well-formed `$NAME`/`${NAME}` token in `value` exactly once.
///
/// The output is built only from untouched input characters and resolved
/// values, so a replacement value containing `$OTHER` is emitted verbatim.
fn substitute_environment_variables(
    value: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> String {
    let bytes = value.as_bytes();
    let mut expanded = String::with_capacity(value.len());
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] != b'$' {
            let character = value[position..]
                .chars()
                .next()
                .expect("every position starts a character");
            expanded.push(character);
            position += character.len_utf8();
            continue;
        }
        match read_variable_reference(bytes, position) {
            Some((name, length)) => {
                match lookup(name) {
                    Some(replacement) => expanded.push_str(&replacement),
                    None => expanded.push_str(&value[position..position + length]),
                }
                position += length;
            }
            None => {
                expanded.push('$');
                position += 1;
            }
        }
    }
    expanded
}

/// Read a `$NAME` or `${NAME}` reference starting at `position` (the index
/// of the `$`), returning the variable name and the number of bytes the
/// reference occupies from `position`.
///
/// Returns `None` when no valid reference starts there — an empty name, a
/// name that does not start with `[A-Za-z_]`, an unterminated `${…`, or a
/// bare `$` — so the caller leaves that `$` literal.
fn read_variable_reference(bytes: &[u8], position: usize) -> Option<(&str, usize)> {
    let (name_start, name_end, consumed_end) = if bytes.get(position + 1) == Some(&b'{') {
        let name_start = position + 2;
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start
            || !is_variable_name_start(bytes[name_start])
            || bytes.get(name_end) != Some(&b'}')
        {
            return None;
        }
        (name_start, name_end, name_end + 1)
    } else {
        let name_start = position + 1;
        if name_start >= bytes.len() || !is_variable_name_start(bytes[name_start]) {
            return None;
        }
        let mut name_end = name_start;
        while name_end < bytes.len() && is_variable_name_byte(bytes[name_end]) {
            name_end += 1;
        }
        (name_start, name_end, name_end)
    };
    let name = std::str::from_utf8(&bytes[name_start..name_end]).ok()?;
    Some((name, consumed_end - position))
}

/// First byte of a variable name: `[A-Za-z_]`.
fn is_variable_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Any byte of a variable name: `[A-Za-z0-9_]`.
fn is_variable_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Expand a leading `~` or `~/` prefix through `lookup("HOME")`.
///
/// `~user/…` is deliberately unsupported: expanding it would require parsing
/// the operating system's user database for no benefit, so the value stays
/// untouched.
fn expand_leading_tilde(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let Some(rest) = value.strip_prefix('~') else {
        return value.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/')) {
        return value.to_string();
    }
    match lookup("HOME") {
        Some(home) => format!("{home}{rest}"),
        None => value.to_string(),
    }
}

/// Resolve the filesystem device ID of `path` by stat-ing it (following
/// symlinks), returning the kernel's opaque numeric `st_dev` value.
///
/// Returns `None` when the path's metadata is unavailable, and always `None`
/// on non-unix targets, so [`is_cross_device`] never reports a difference
/// there (the `CrossFilesystem` rule parses on every platform but never
/// matches off-unix). Device IDs are only ever compared for equality, never
/// interpreted.
fn resolve_device(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|metadata| metadata.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Pure comparison for the `cross_filesystem` rule: `true` only when both
/// device IDs are known and differ. Any `None` on either side — metadata
/// unavailable for the candidate anchor or the workspace root — means "no
/// positively established difference" and yields `false` (the rule does not
/// match; `outside_workspace`/`dot_path` remain the boundary guards).
fn is_cross_device(candidate: Option<u64>, workspace_root: Option<u64>) -> bool {
    matches!(
        (candidate, workspace_root),
        (Some(candidate), Some(workspace_root)) if candidate != workspace_root
    )
}

/// Canonicalized deepest existing ancestor of `path` — the device-comparison
/// anchor used by [`resolve_device`] when `path` itself does not exist yet
/// (e.g. a write target or a not-yet-created read path). Sibling of
/// [`SandboxContext::canonicalize_best_effort`]: that function reattaches the
/// missing tail to the canonicalized anchor, while this one returns only the
/// anchor. Falls back to `path` unchanged when no ancestor can be
/// canonicalized.
fn canonicalize_existing_anchor(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            return canonical;
        }
        match current.file_name() {
            Some(_) => match current.parent() {
                Some(parent) => current = parent.to_path_buf(),
                None => break,
            },
            None => break,
        }
    }
    path.to_path_buf()
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

/// Per-tool sandbox context. Carries an `Arc<RwLock<Sandbox>>` so it always
/// sees the latest sandbox state, plus a tool-specific ordered rule list.
///
/// Evaluation: normalize path (workspace_root.join(path), collapse ".."
/// without hitting the filesystem) → walk rules in order → last match wins.
/// Path does not need to exist on disk; normalization alone determines
/// workspace membership.
///
/// Returns `None` when no rule fires (sandbox abstains); the calling tool
/// applies its own default.
#[derive(Clone)]
pub struct SandboxContext {
    sandbox: Arc<RwLock<Sandbox>>,
    /// Ordered rules; last match wins. Always evaluated last, after either
    /// `configured_rules` or the built-in default floor — this is what
    /// `--auto-approve-write`/`--auto-approve-edit`'s catch-all rule uses so
    /// it wins regardless of any `[tools.<name>]` config.
    rules: Vec<SandboxRule>,
    /// When `Some`, fully replaces the built-in default floor (the two
    /// hardcoded `outside_workspace`/`dot_path` checks) for this tool: only
    /// this list's entries are evaluated in list order (last match wins)
    /// before `rules`. `None` means "no `[tools.<name>]` config for this
    /// tool" — the default floor applies instead. Populated from a
    /// profile's `[tools.<name>]` table via
    /// [`SandboxContext::with_configured_rules`].
    configured_rules: Option<Vec<ResolvedRule>>,
}

impl std::fmt::Debug for SandboxContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SandboxContext")
            .field("rules_count", &self.rules.len())
            .field(
                "configured_rules_count",
                &self.configured_rules.as_ref().map(Vec::len),
            )
            .finish()
    }
}

impl SandboxContext {
    /// Build a context with an empty rule list and no `[tools.<name>]`
    /// config, so the built-in default floor
    /// (`outside_workspace`/`dot_path` → `UserApprovalRequired`) applies.
    /// Named glob rules (if any) are appended by tool constructors.
    pub fn new(sandbox: Arc<RwLock<Sandbox>>) -> Self {
        SandboxContext {
            sandbox,
            rules: vec![],
            configured_rules: None,
        }
    }

    /// Append a rule to the end of this context's rule list. Last match wins,
    /// so appending a catch-all rule (e.g. `**` → `AutoApproved`) effectively
    /// overrides any earlier verdict for every path. Always evaluated after
    /// `configured_rules`/the default floor, so this continues to win
    /// regardless of `[tools.<name>]` config.
    pub fn with_rule(mut self, rule: SandboxRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Replace this tool's built-in default floor with the rule list parsed
    /// from a `[tools.<name>]` table's `rules` key. This *fully replaces* the
    /// floor for this tool (it does not merge with it): a caller that still
    /// wants `outside_workspace`/`dot_path` protection must include those as
    /// explicit [`SandboxRuleConfig::Named`] entries in `rules`.
    ///
    /// Invalid rules (a malformed glob pattern, or a `command` rule with a
    /// misplaced `*`/`**` wildcard) are skipped (with a `tracing::warn!`)
    /// rather than causing a panic or a hard config-load failure, since
    /// sandbox rules are safety-critical and a single malformed rule should
    /// not silently disable outside_workspace/dot_path protection.
    pub fn with_configured_rules(mut self, rules: Vec<SandboxRuleConfig>) -> Self {
        let resolved = rules
            .into_iter()
            .filter_map(|rule| match ResolvedRule::from_config(rule) {
                Ok(resolved) => Some(resolved),
                Err(error) => {
                    tracing::warn!(%error, "skipping invalid sandbox rule");
                    None
                }
            })
            .collect();
        self.configured_rules = Some(resolved);
        self
    }

    /// Collapse `path` to a clean absolute path using only lexical rules —
    /// no filesystem access, no symlink resolution.
    ///
    /// If `path` is relative it is joined to `base`. `..` and `.` components
    /// are then folded away. The result is suitable as a fallback for paths
    /// that do not yet exist; callers that need the real target must follow up
    /// with `std::fs::canonicalize`.
    fn normalize(base: &Path, path: &Path) -> PathBuf {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        // Collapse `..` and `.` components without `canonicalize` (which
        // requires the path to exist).
        let mut components: Vec<std::path::Component> = Vec::new();
        for component in absolute.components() {
            match component {
                std::path::Component::ParentDir => {
                    // Pop the last non-root component (never pop the root).
                    let last = components.last();
                    if matches!(last, Some(std::path::Component::Normal(_))) {
                        components.pop();
                    } else {
                        components.push(component);
                    }
                }
                std::path::Component::CurDir => {}
                other => components.push(other),
            }
        }
        components.iter().collect()
    }

    /// Return `true` when any path component starts with `.`.
    fn is_dot_path(path: &Path) -> bool {
        path.components().any(|component| {
            if let std::path::Component::Normal(os) = component {
                os.to_string_lossy().starts_with('.')
            } else {
                false
            }
        })
    }

    /// Canonicalize `path`, resolving symlinks and normalizing the form.
    ///
    /// When the full path does not exist (e.g. a write target), falls back to
    /// walking up the directory tree until a component that *does* exist is
    /// found, canonicalizing that anchor, and reattaching the non-existent
    /// tail. This ensures that a path like `/var/folders/ws/new_file.rs` is
    /// resolved to `/private/var/folders/ws/new_file.rs` on macOS even though
    /// `new_file.rs` has not been created yet — which is necessary for
    /// workspace-membership comparisons when `workspace_root` is canonical.
    /// On macOS, `/var` is a symlink to `/private/var`; canonicalizing the
    /// existing ancestor `/var/folders/ws` follows that symlink, producing
    /// the `/private/…` prefix that matches a canonical `workspace_root`.
    pub(crate) fn canonicalize_best_effort(path: &Path) -> PathBuf {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            return canonical;
        }
        // Walk up to find the deepest existing ancestor.
        let mut current = path.to_path_buf();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if let Ok(canonical) = std::fs::canonicalize(&current) {
                let mut result = canonical;
                for component in tail.iter().rev() {
                    result.push(component);
                }
                return result;
            }
            match current.file_name() {
                Some(name) => {
                    tail.push(name.to_owned());
                    match current.parent() {
                        Some(parent) => current = parent.to_path_buf(),
                        None => break,
                    }
                }
                None => break,
            }
        }
        path.to_path_buf()
    }

    /// Check whether `path` is allowed by the sandbox rules.
    ///
    /// Returns `None` when no rule fires — the sandbox abstains and the calling
    /// tool applies its own default. Returns `Some(verdict)` when a rule has an
    /// explicit opinion that the tool must honour unconditionally.
    ///
    /// **Path resolution:** the input is first normalized lexically (no filesystem
    /// access), then resolved via [`canonicalize_best_effort`] which resolves
    /// symlinks for existing paths and walks up to the deepest existing ancestor
    /// for paths that do not yet exist (e.g. write targets). This ensures that
    /// workspace-membership comparisons are correct even when the input path
    /// uses a non-canonical prefix such as macOS
    /// `/var/folders` → `/private/var/folders`.
    ///
    /// Rule evaluation order:
    /// 1. When `configured_rules` is `Some` (this tool has a `[tools.<name>]`
    ///    config table), evaluate exactly that list, in order, last match
    ///    wins — this *replaces* the default floor rather than merging with
    ///    it. Otherwise evaluate the built-in default floor: outside-workspace
    ///    → `Some(UserApprovalRequired)`, then any component of the path
    ///    relative to the workspace root starting with `.` →
    ///    `Some(UserApprovalRequired)`.
    /// 2. Named glob rules (from `self.rules`) are evaluated last; last match
    ///    wins. This is what `--auto-approve-write`/`--auto-approve-edit`'s
    ///    catch-all rule relies on to win regardless of step 1's outcome.
    /// 3. If nothing fires → `None` (sandbox abstains).
    pub fn check_path(&self, path: &Path) -> Option<ApprovalCheck> {
        self.check_path_with_dot_path_access(path, false)
    }

    /// Check whether an in-workspace enumeration may traverse `path`,
    /// allowing dot-prefixed components while retaining workspace and named
    /// rule enforcement.
    pub fn check_path_allowing_dot_paths(&self, path: &Path) -> Option<ApprovalCheck> {
        self.check_path_with_dot_path_access(path, true)
    }

    /// Returns `true` when `effective` normalizes outside `workspace_root`.
    /// Shared by the default floor and the `outside_workspace` named rule so
    /// the underlying comparison exists in exactly one place.
    fn matches_outside_workspace(workspace_root: &Path, effective: &Path) -> bool {
        !effective.starts_with(workspace_root)
    }

    /// Returns `true` when the portion of `effective` relative to
    /// `workspace_root` has a dot-prefixed component, unless `allow_dot_paths`
    /// bypasses the check (used by `glob` enumeration). A path outside the
    /// workspace (whose `strip_prefix` fails) never matches here — the
    /// `outside_workspace` rule is responsible for that case. Shared by the
    /// default floor and the `dot_path` named rule so the underlying
    /// comparison exists in exactly one place.
    fn matches_dot_path(workspace_root: &Path, effective: &Path, allow_dot_paths: bool) -> bool {
        if allow_dot_paths {
            return false;
        }
        match effective.strip_prefix(workspace_root) {
            Ok(relative) => Self::is_dot_path(relative),
            Err(_) => false,
        }
    }

    /// Evaluate one of the three path-based [`NamedSandboxRule`] variants
    /// against `workspace_root`/`effective`. The four `bash`-only variants
    /// are never valid under a path tool (rejected by config validation);
    /// defensively `false` here if one slips through.
    ///
    /// `CrossFilesystem` resolves both device IDs fresh on every evaluation
    /// (workspace root: the defensively re-canonicalized `workspace_root`;
    /// candidate: [`resolve_device`] of [`canonicalize_existing_anchor`] of
    /// `effective`) — never cached, so a mid-session remount cannot leave a
    /// stale ID behind.
    fn evaluate_named_rule(
        rule: NamedSandboxRule,
        workspace_root: &Path,
        effective: &Path,
        allow_dot_paths: bool,
    ) -> bool {
        match rule {
            NamedSandboxRule::OutsideWorkspace => {
                Self::matches_outside_workspace(workspace_root, effective)
            }
            NamedSandboxRule::DotPath => {
                Self::matches_dot_path(workspace_root, effective, allow_dot_paths)
            }
            NamedSandboxRule::CrossFilesystem => is_cross_device(
                resolve_device(&canonicalize_existing_anchor(effective)),
                resolve_device(workspace_root),
            ),
            NamedSandboxRule::Backgrounded
            | NamedSandboxRule::VariableAssignmentPrefix
            | NamedSandboxRule::UnquotedGlob
            | NamedSandboxRule::UnquotedBraceExpansion
            | NamedSandboxRule::NetworkRequest => false,
        }
    }

    fn check_path_with_dot_path_access(
        &self,
        path: &Path,
        allow_dot_paths: bool,
    ) -> Option<ApprovalCheck> {
        let workspace_root = {
            let guard = self.sandbox.read().expect("sandbox lock poisoned");
            guard.workspace_root.clone()
        };
        // Ensure workspace_root is canonical even when the Sandbox was built via
        // direct struct construction rather than Sandbox::new() / Sandbox::with_root().
        // This guards against macOS /var/folders → /private/var/folders mismatches.
        let workspace_root = std::fs::canonicalize(&workspace_root).unwrap_or(workspace_root);
        let normalized = Self::normalize(&workspace_root, path);

        // Resolve symlinks and non-canonical prefixes (e.g. macOS /var → /private/var).
        let effective = Self::canonicalize_best_effort(&normalized);
        let path_str = effective.to_string_lossy();

        // No implicit zeroth verdict — absence of a match returns None.
        let mut verdict: Option<ApprovalCheck> = None;

        match &self.configured_rules {
            Some(configured) => {
                // `[tools.<name>]` config fully replaces the default floor:
                // evaluate exactly this list, in order, last match wins.
                for rule in configured {
                    match rule {
                        ResolvedRule::Named {
                            rule: named,
                            verdict: rule_verdict,
                            target: _,
                        } => {
                            if Self::evaluate_named_rule(
                                *named,
                                &workspace_root,
                                &effective,
                                allow_dot_paths,
                            ) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        ResolvedRule::PathGlob {
                            pattern,
                            verdict: rule_verdict,
                        } => {
                            if pattern.matches(&path_str) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        // A `bash`-only `CommandGlob` rule under a path tool
                        // is rejected by config validation; never matches a
                        // path defensively if it slips through.
                        ResolvedRule::CommandGlob { .. } => {}
                    }
                }
            }
            None => {
                // Built-in default floor: outside_workspace, then dot_path.
                // Both boundary checks ask the user instead of rejecting the
                // call outright, so a legitimate out-of-workspace read (or a
                // dot-file the model was pointed at) can be granted; the
                // `OUTSIDE_WORKSPACE_REASON`/`DOT_PATH_REASON` texts remain
                // the messages for a *configured* `auto_denied` named rule.
                if Self::evaluate_named_rule(
                    NamedSandboxRule::OutsideWorkspace,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::UserApprovalRequired);
                }
                if Self::evaluate_named_rule(
                    NamedSandboxRule::DotPath,
                    &workspace_root,
                    &effective,
                    allow_dot_paths,
                ) {
                    verdict = Some(ApprovalCheck::UserApprovalRequired);
                }
            }
        }

        // Named glob rules (appended by tool constructors, e.g.
        // --auto-approve-write/--auto-approve-edit's catch-all). Always
        // evaluated last so they win regardless of configured_rules/the
        // default floor.
        for rule in &self.rules {
            if rule.pattern.matches(&path_str) {
                verdict = Some(rule.verdict.clone());
            }
        }

        tracing::debug!(
            path = %effective.display(),
            sandbox_verdict = match &verdict {
                None => "abstain".to_string(),
                Some(ApprovalCheck::AutoApproved) => "auto-approved".to_string(),
                Some(ApprovalCheck::AutoDenied { reason }) => format!("auto-denied: {reason}"),
                Some(ApprovalCheck::UserApprovalRequired) => "user-approval-required".to_string(),
            },
            "sandbox check_path"
        );

        verdict
    }

    /// Check whether `command` is allowed by the sandbox.
    ///
    /// Returns `None` when no rule fires — the sandbox abstains and the calling
    /// tool applies its own default (typically `UserApprovalRequired` for shell
    /// commands). Returns `Some(verdict)` when a rule has an explicit opinion.
    ///
    /// **Extraction:** `command` is parsed via [`bash_command_extractor::extract`]
    /// into its top-level statements (see that module's doc comment for the
    /// exact `tree-sitter-bash` grammar facts this relies on). A statement
    /// outside the fixed scanning scope (`list`/`pipeline`/`command`
    /// only — subshells, redirections, substitutions, control flow,
    /// function definitions, heredocs) contributes `UserApprovalRequired`.
    ///
    /// **Per-command evaluation:** each extracted `command` is evaluated
    /// independently against this tool's resolved rule list — a configured
    /// `[tools.bash]`/`[agents.<type>.tools.bash]` table (ordinary
    /// last-match-wins across its entries, since list order is a deliberate
    /// user choice there) or, absent one, the unconditional default floor
    /// (`backgrounded` → `AutoDenied`, `variable_assignment_prefix`/
    /// `unquoted_glob`/`unquoted_brace_expansion` → `UserApprovalRequired`),
    /// whose matching rules are combined by worst-case ranking rather than
    /// last-match-wins, so a stronger signal is never silently shadowed by a
    /// weaker one that happens to also match.
    ///
    /// **Aggregation:** all per-statement contributions are combined across
    /// the whole script by worst-case ranking
    /// (`AutoDenied` > `UserApprovalRequired` > `AutoApproved`) into a single
    /// structural verdict. `self.rules` (e.g. `--auto-approve-bash`'s
    /// catch-all) is then evaluated once, matched against the raw `command`
    /// string, last-match-wins against the structural verdict — so it always
    /// wins regardless of any `[tools.bash]` config, mirroring
    /// [`SandboxContext::check_path`]'s existing precedence.
    ///
    /// Returns `None` only when the script fails to parse at all, or every
    /// extracted statement is fully analysable and legal but no rule/floor
    /// fires and `self.rules` has no match either (pure abstention).
    pub fn check_command(&self, command: &str) -> Option<ApprovalCheck> {
        let structural_verdict = match bash_command_extractor::extract(command) {
            bash_command_extractor::ExtractionResult::ParseFailed => None,
            bash_command_extractor::ExtractionResult::Statements(statements) => {
                let mut aggregate: Option<ApprovalCheck> = None;
                for statement in &statements {
                    let contribution = match statement {
                        bash_command_extractor::ExtractedStatement::Command(extracted) => {
                            self.evaluate_command(extracted)
                        }
                        bash_command_extractor::ExtractedStatement::Unanalysable => {
                            Some(ApprovalCheck::UserApprovalRequired)
                        }
                    };
                    aggregate = Self::worse_verdict(aggregate, contribution);
                }
                aggregate
            }
        };

        // `self.rules` (e.g. --auto-approve-bash's catch-all) is always
        // evaluated last, matched against the raw command string, so it wins
        // regardless of the structural verdict above.
        let mut verdict = structural_verdict;
        for rule in &self.rules {
            if rule.pattern.matches(command) {
                verdict = Some(rule.verdict.clone());
            }
        }

        tracing::debug!(
            command = %command,
            sandbox_verdict = match &verdict {
                None => "abstain".to_string(),
                Some(ApprovalCheck::AutoApproved) => "auto-approved".to_string(),
                Some(ApprovalCheck::AutoDenied { reason }) => format!("auto-denied: {reason}"),
                Some(ApprovalCheck::UserApprovalRequired) => "user-approval-required".to_string(),
            },
            "sandbox check_command"
        );

        verdict
    }

    /// Check whether a network-tool call (`web_search`, `web_fetch`) is
    /// allowed.
    ///
    /// `target` is the call's `scheme://host[:port]` match key (an explicit
    /// port is included when present in the URL): `web_fetch` passes the
    /// requested URL's key, `web_search` its configured backend endpoint's
    /// key. A configured [`NamedSandboxRule::NetworkRequest`] rule without a
    /// `target` parameter matches every call; with a [`NetworkTargetPattern`]
    /// `target` it matches only calls whose key the pattern matches. There is
    /// **no default floor** (the path-based floor rules have no path here):
    /// returning `None` means the sandbox abstains and the calling tool
    /// applies its own default (`UserApprovalRequired`).
    ///
    /// Precedence mirrors [`check_path`](Self::check_path) and
    /// [`check_command`](Self::check_command): configured rules in list
    /// order (last match wins), then the appended rules — the `**`
    /// catch-all `--auto-approve-web-search`/`--auto-approve-web-fetch`
    /// adds — matched against [`NETWORK_REQUEST_MATCH_KEY`], so the CLI
    /// flag always wins regardless of `[tools.web_search]`/`[tools.web_fetch]`
    /// config. Path/command rule kinds under a network tool's config are
    /// rejected at config load and defensively never match here.
    pub fn check_network_request(&self, target: &str) -> Option<ApprovalCheck> {
        let mut verdict: Option<ApprovalCheck> = None;
        // No `[tools.web_search]`/`[tools.web_fetch]` config → no floor; the
        // tool's own default (`UserApprovalRequired`) applies on abstention.
        if let Some(configured) = &self.configured_rules {
            for rule in configured {
                match rule {
                    ResolvedRule::Named {
                        rule: named,
                        verdict: rule_verdict,
                        target: rule_target,
                    } => {
                        if Self::evaluate_network_named_rule(*named, rule_target.as_ref(), target) {
                            verdict = Some(rule_verdict.clone());
                        }
                    }
                    // Path/command rules under a network tool table are
                    // rejected by config validation; never match a
                    // network call defensively if they slip through.
                    ResolvedRule::PathGlob { .. } | ResolvedRule::CommandGlob { .. } => {}
                }
            }
        }

        // Appended rules (e.g. `--auto-approve-web-search`'s catch-all).
        // Always evaluated last so they win regardless of configured rules.
        for rule in &self.rules {
            if rule.pattern.matches(NETWORK_REQUEST_MATCH_KEY) {
                verdict = Some(rule.verdict.clone());
            }
        }

        tracing::debug!(
            sandbox_verdict = match &verdict {
                None => "abstain".to_string(),
                Some(ApprovalCheck::AutoApproved) => "auto-approved".to_string(),
                Some(ApprovalCheck::AutoDenied { reason }) => format!("auto-denied: {reason}"),
                Some(ApprovalCheck::UserApprovalRequired) => "user-approval-required".to_string(),
            },
            "sandbox check_network_request"
        );

        verdict
    }

    /// Only [`NamedSandboxRule::NetworkRequest`] matches a network call;
    /// path and `bash` named rules never do (config validation rejects them
    /// under network-tool tables; defense in depth if they slip through). A
    /// `network_request` rule without a `target` matches every call; with a
    /// `target` it matches only when the call's `scheme://host[:port]` key
    /// matches the [`NetworkTargetPattern`].
    fn evaluate_network_named_rule(
        rule: NamedSandboxRule,
        rule_target: Option<&NetworkTargetPattern>,
        call_target: &str,
    ) -> bool {
        match (rule, rule_target) {
            (NamedSandboxRule::NetworkRequest, None) => true,
            (NamedSandboxRule::NetworkRequest, Some(pattern)) => pattern.matches(call_target),
            _ => false,
        }
    }

    /// Rank an [`ApprovalCheck`] for worst-case combination:
    /// `AutoDenied` > `UserApprovalRequired` > `AutoApproved`.
    fn approval_check_rank(check: &ApprovalCheck) -> u8 {
        match check {
            ApprovalCheck::AutoDenied { .. } => 2,
            ApprovalCheck::UserApprovalRequired => 1,
            ApprovalCheck::AutoApproved => 0,
        }
    }

    /// Combine two optional verdicts by worst-case ranking (`None` losing to
    /// any `Some`).
    fn worse_verdict(a: Option<ApprovalCheck>, b: Option<ApprovalCheck>) -> Option<ApprovalCheck> {
        match (a, b) {
            (None, None) => None,
            (Some(only), None) | (None, Some(only)) => Some(only),
            (Some(left), Some(right)) => {
                if Self::approval_check_rank(&left) >= Self::approval_check_rank(&right) {
                    Some(left)
                } else {
                    Some(right)
                }
            }
        }
    }

    /// Evaluate one extracted `command` against this tool's resolved rule
    /// list: a configured `[tools.bash]` list (ordinary last-match-wins) or,
    /// absent one, the unconditional default floor (worst-case combination
    /// of the four named command rules).
    fn evaluate_command(
        &self,
        command: &bash_command_extractor::ExtractedCommand,
    ) -> Option<ApprovalCheck> {
        match &self.configured_rules {
            Some(configured) => {
                let mut verdict = None;
                for rule in configured {
                    match rule {
                        ResolvedRule::Named {
                            rule: named,
                            verdict: rule_verdict,
                            target: _,
                        } => {
                            if Self::evaluate_command_named_rule(*named, command) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        ResolvedRule::CommandGlob {
                            command: pattern,
                            verdict: rule_verdict,
                        } => {
                            if pattern.matches(&command.argv) {
                                verdict = Some(rule_verdict.clone());
                            }
                        }
                        // A path `PathGlob` rule under `[tools.bash]` is
                        // rejected by config validation
                        // (`crate::config::profile::ProfileConfig`); never
                        // matches a command defensively if it slips through.
                        ResolvedRule::PathGlob { .. } => {}
                    }
                }
                verdict
            }
            None => Self::evaluate_bash_default_floor(command),
        }
    }

    /// Evaluate one of the four `bash`-only [`NamedSandboxRule`] variants
    /// against an extracted command. The three path rules
    /// (`OutsideWorkspace`/`DotPath`/`CrossFilesystem`) are never valid
    /// under `[tools.bash]` (rejected by config validation); defensively
    /// `false` here if they slip through.
    fn evaluate_command_named_rule(
        rule: NamedSandboxRule,
        command: &bash_command_extractor::ExtractedCommand,
    ) -> bool {
        match rule {
            NamedSandboxRule::Backgrounded => command.backgrounded,
            NamedSandboxRule::VariableAssignmentPrefix => command.has_variable_assignment_prefix,
            NamedSandboxRule::UnquotedGlob => command
                .argv
                .iter()
                .any(bash_command_extractor::token_has_unquoted_glob),
            NamedSandboxRule::UnquotedBraceExpansion => command
                .argv
                .iter()
                .any(bash_command_extractor::token_has_unquoted_brace_expansion),
            NamedSandboxRule::OutsideWorkspace
            | NamedSandboxRule::DotPath
            | NamedSandboxRule::CrossFilesystem
            | NamedSandboxRule::NetworkRequest => false,
        }
    }

    /// The unconditional `bash` default floor (no `[tools.bash]` config):
    /// worst-case combination of the four named command rules, not
    /// last-match-wins — see [`SandboxContext::check_command`]'s doc
    /// comment.
    fn evaluate_bash_default_floor(
        command: &bash_command_extractor::ExtractedCommand,
    ) -> Option<ApprovalCheck> {
        let mut verdict = None;
        if command.backgrounded {
            verdict = Self::worse_verdict(
                verdict,
                Some(ApprovalCheck::AutoDenied {
                    reason: BACKGROUNDED_REASON.to_string(),
                }),
            );
        }
        if command.has_variable_assignment_prefix {
            verdict = Self::worse_verdict(verdict, Some(ApprovalCheck::UserApprovalRequired));
        }
        if command
            .argv
            .iter()
            .any(bash_command_extractor::token_has_unquoted_glob)
        {
            verdict = Self::worse_verdict(verdict, Some(ApprovalCheck::UserApprovalRequired));
        }
        if command
            .argv
            .iter()
            .any(bash_command_extractor::token_has_unquoted_brace_expansion)
        {
            verdict = Self::worse_verdict(verdict, Some(ApprovalCheck::UserApprovalRequired));
        }
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sandbox(root: &Path) -> Arc<RwLock<Sandbox>> {
        Arc::new(RwLock::new(Sandbox::with_root(root.to_path_buf())))
    }

    #[test]
    fn check_path_outside_workspace_requires_user_approval() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(
            context.check_path(&outside),
            Some(ApprovalCheck::UserApprovalRequired),
            "path outside workspace should be Some(UserApprovalRequired)"
        );
    }

    #[test]
    fn check_path_dot_path_requires_user_approval() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::UserApprovalRequired),
            "dot-path relative to workspace should be Some(UserApprovalRequired)"
        );
        let dot_dir_path = workspace.join(".git/config");
        assert_eq!(
            context.check_path(&dot_dir_path),
            Some(ApprovalCheck::UserApprovalRequired),
            "dot-directory relative to workspace should be Some(UserApprovalRequired)"
        );
    }

    #[test]
    fn check_path_allowing_dot_paths_abstains_inside_workspace() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let dot_dir_path = workspace.join(".git/config");
        assert_eq!(
            context.check_path_allowing_dot_paths(&dot_dir_path),
            None,
            "dot-directory enumeration inside the workspace should abstain"
        );
    }

    #[test]
    fn check_path_normal_inside_workspace_returns_none() {
        // No rule fires for a plain in-workspace path: sandbox abstains.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let path = workspace.join("src/main.rs");
        assert_eq!(
            context.check_path(&path),
            None,
            "normal workspace path: sandbox abstains (None)"
        );
    }

    #[test]
    fn normalize_collapses_dot_dot_components() {
        let base = Path::new("/workspace");
        let path = Path::new("src/../README.md");
        let normalized = SandboxContext::normalize(base, path);
        assert_eq!(normalized, PathBuf::from("/workspace/README.md"));
    }

    #[test]
    fn normalize_collapses_current_dir_component() {
        let base = Path::new("/workspace");
        let path = Path::new("./src/./main.rs");
        let normalized = SandboxContext::normalize(base, path);
        assert_eq!(normalized, PathBuf::from("/workspace/src/main.rs"));
    }

    #[test]
    fn sandbox_context_debug_shows_rules_count() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let debug_str = format!("{:?}", context);
        assert!(debug_str.contains("rules_count"));
    }

    #[test]
    fn sandbox_default_uses_current_dir() {
        // Acquires SUBSCRIBER_LOCK to prevent concurrent execution with
        // `init_logging_relative_filename_uses_current_directory`, which
        // temporarily changes the process-global cwd while holding the same
        // lock.  Without this guard the two tests race: one reads cwd during
        // the other's cwd mutation, causing a spurious mismatch.
        let _lock = crate::test_support::SUBSCRIBER_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sandbox = Sandbox::default();
        let cwd = std::env::current_dir().expect("cwd");
        let canonical_cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        assert_eq!(sandbox.workspace_root, canonical_cwd);
    }

    #[test]
    fn normalize_does_not_pop_past_root() {
        // A `..` that cannot pop (root) is left in place.
        let base = Path::new("/");
        let path = Path::new("/../file.txt");
        let normalized = SandboxContext::normalize(base, path);
        // The `..` at root is a no-op; normalized stays at /../file.txt level.
        assert!(normalized.ends_with("file.txt"));
    }

    #[test]
    fn check_path_glob_rule_overrides_to_explicit_verdict() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        // Build the glob pattern from the canonical workspace_root stored in
        // the sandbox, not from the raw `workspace` variable, so that the
        // pattern matches the canonical `effective` path inside check_path.
        let canonical_workspace = sandbox.read().unwrap().workspace_root.clone();
        let allow_pattern = glob::Pattern::new(&format!(
            "{}/allowed/**",
            canonical_workspace.to_string_lossy()
        ))
        .unwrap();
        let mut context = SandboxContext::new(Arc::clone(&sandbox));
        context.rules.push(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: allow_pattern,
        });

        // Glob rule fires → Some(AutoApproved).
        let allowed_path = workspace.join("allowed/file.txt");
        assert_eq!(
            context.check_path(&allowed_path),
            Some(ApprovalCheck::AutoApproved),
            "glob rule should produce Some(AutoApproved)"
        );

        // No rule fires for this path → sandbox abstains.
        let normal_path = workspace.join("src/main.rs");
        assert_eq!(
            context.check_path(&normal_path),
            None,
            "unmatched path: sandbox abstains (None)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escaping_workspace_requires_user_approval() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = temporary_dir.path().join("secret.txt");
        std::fs::write(&outside, "sensitive").unwrap();

        // A workspace-relative symlink whose target lives outside the workspace.
        let link = workspace.join("innocent.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        assert_eq!(
            context.check_path(&link),
            Some(ApprovalCheck::UserApprovalRequired),
            "symlink escaping the workspace must require user approval"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_dot_file_inside_workspace_requires_user_approval() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let real_dot_file = workspace.join(".env");
        std::fs::write(&real_dot_file, "SECRET=x").unwrap();

        // A non-dot symlink name pointing at a dot-file inside the workspace.
        let link = workspace.join("config.txt");
        std::os::unix::fs::symlink(&real_dot_file, &link).unwrap();

        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        assert_eq!(
            context.check_path(&link),
            Some(ApprovalCheck::UserApprovalRequired),
            "symlink whose real target is a dot-file must require user approval"
        );
    }

    #[test]
    fn check_command_default_floor_abstains_for_plain_safe_commands() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(
            context.check_command("echo hello"),
            None,
            "sandbox abstains for ordinary commands: no default-floor rule fires"
        );
        assert_eq!(
            context.check_command("rm -rf /"),
            None,
            "sandbox abstains even for dangerous-looking commands: argv-shape rules do not \
             evaluate command semantics, only structural signals"
        );
        assert_eq!(
            context.check_command(""),
            None,
            "sandbox abstains for empty command"
        );
    }

    #[test]
    fn check_command_default_floor_denies_backgrounded_command() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert!(
            matches!(
                context.check_command("sleep 10 &"),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "a backgrounded command is denied by the unconditional default floor"
        );
    }

    #[test]
    fn check_command_default_floor_requires_approval_for_variable_assignment_prefix() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(
            context.check_command("FOO=bar git status"),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }

    #[test]
    fn check_command_default_floor_requires_approval_for_unquoted_glob_or_brace() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(
            context.check_command("cat *.txt"),
            Some(ApprovalCheck::UserApprovalRequired)
        );
        assert_eq!(
            context.check_command("echo {a,b}"),
            Some(ApprovalCheck::UserApprovalRequired)
        );
        assert_eq!(
            context.check_command(r#"cat "*.txt""#),
            None,
            "a quoted glob-like token does not trigger unquoted_glob"
        );
    }

    #[test]
    fn check_command_unanalysable_construct_requires_approval() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(
            context.check_command("echo $(whoami)"),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }

    #[test]
    fn check_command_parse_failure_returns_none() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        assert_eq!(context.check_command("cmd1 |"), None);
    }

    #[test]
    fn check_command_aggregates_worst_case_across_multiple_commands() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["git".to_string(), "status".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        // First segment matches an AutoApproved CommandGlob rule; second
        // segment ($(whoami)) is un-analysable -> overall UserApprovalRequired.
        assert_eq!(
            context.check_command("git status ; echo $(whoami)"),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }

    #[test]
    fn check_command_auto_denied_in_one_segment_wins_over_others() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        // "echo hello" abstains (no rule fires), "sleep 10 &" is
        // AutoDenied by the default floor -> overall AutoDenied.
        assert!(matches!(
            context.check_command("echo hello ; sleep 10 &"),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
    }

    #[test]
    fn check_command_configured_rules_lose_default_floor_protection() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        // A `[tools.bash]` config that omits the four named rules fully
        // replaces the default floor: a backgrounded command is no longer
        // denied.
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["git".to_string(), "*".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        assert_eq!(
            context.check_command("sleep 10 &"),
            None,
            "backgrounded no longer denied once [tools.bash] fully replaces the default floor"
        );
    }

    #[test]
    fn check_command_default_floor_combines_named_rules_by_worst_case_not_last_match() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox));

        // A single command simultaneously matches `unquoted_glob`
        // (UserApprovalRequired) and `backgrounded` (AutoDenied); the
        // unconfigured floor must resolve to the worse of the two
        // regardless of which rule's condition happens to be evaluated
        // first in the implementation.
        assert!(matches!(
            context.check_command("cat *.txt &"),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
    }

    #[test]
    fn check_command_self_rules_catch_all_wins_over_structural_auto_denied() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_rule(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: Pattern::new("**").unwrap(),
        });

        // "sleep 10 &" would be AutoDenied by the default floor, but the
        // code-appended catch-all (simulating --auto-approve-bash) wins.
        assert_eq!(
            context.check_command("sleep 10 &"),
            Some(ApprovalCheck::AutoApproved)
        );
    }

    #[test]
    fn check_command_glob_rule_matches_trailing_single_wildcard() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["git".to_string(), "reflog".to_string(), "*".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        assert_eq!(
            context.check_command("git reflog show"),
            Some(ApprovalCheck::AutoApproved)
        );
        assert_eq!(
            context.check_command("git reflog show extra"),
            None,
            "trailing * matches exactly one more token, not two"
        );
        assert_eq!(
            context.check_command("git status"),
            None,
            "literal prefix must match exactly"
        );
    }

    #[test]
    fn with_configured_rules_skips_a_command_glob_rule_with_a_misplaced_wildcard() {
        // Defense in depth: `crate::config::profile::validate_tool_sandbox_rules`
        // already rejects this as a hard startup error for profile-loaded
        // rules, but `with_configured_rules` is a public API that a caller
        // could feed directly, bypassing that validation. Confirm the rule
        // is dropped (with a warning) rather than silently degrading to a
        // literal (never-matching) "*" token.
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["git".to_string(), "*".to_string(), "status".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        assert_eq!(
            context.check_command("git status"),
            None,
            "the misplaced-wildcard rule must be dropped, leaving an empty configured list"
        );
        assert_eq!(
            context.check_command("git * status"),
            None,
            "the rule must not survive as a literal '*' token match either"
        );
    }

    #[test]
    fn check_command_glob_rule_trailing_single_wildcard_abstains_on_unquoted_glob_tail() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["cat".to_string(), "*".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        // The tail token "*.txt" contains an unquoted glob char, so this
        // CommandGlob rule abstains (never matches); since this is a fully
        // configured list (no default floor), the whole check abstains.
        assert_eq!(context.check_command("cat *.txt"), None);
        assert_eq!(
            context.check_command("cat notes.txt"),
            Some(ApprovalCheck::AutoApproved)
        );
    }

    #[test]
    fn check_command_glob_rule_trailing_double_wildcard_matches_multiple_tokens() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let sandbox = make_sandbox(temporary_dir.path());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::CommandGlob {
                command: vec!["find".to_string(), "**".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        assert_eq!(
            context.check_command("find . -type f"),
            Some(ApprovalCheck::AutoApproved)
        );
        assert_eq!(
            context.check_command("find *.txt -type f"),
            Some(ApprovalCheck::AutoApproved),
            "** never abstains on unquoted glob/brace in its tail"
        );
        assert_eq!(
            context.check_command("find"),
            None,
            "** requires at least one extra token"
        );
    }

    #[test]
    fn to_relative_path_rewrites_absolute_path_inside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        let absolute = base.join("src/main.rs");

        assert_eq!(
            to_relative_path(absolute.to_str().unwrap(), &base),
            Some("src/main.rs".to_string())
        );
    }

    #[test]
    fn to_relative_path_rewrites_absolute_glob_pattern_inside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        std::fs::create_dir_all(base.join("src")).unwrap();
        let pattern = format!("{}/src/**/*.rs", base.display());

        assert_eq!(
            to_relative_path(&pattern, &base),
            Some("src/**/*.rs".to_string())
        );
    }

    #[test]
    fn to_relative_path_returns_none_for_relative_path() {
        let temporary_dir = tempfile::tempdir().unwrap();
        assert_eq!(to_relative_path("src/main.rs", temporary_dir.path()), None);
    }

    #[test]
    fn to_relative_path_returns_none_for_path_outside_base() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = temporary_dir.path().join("workspace");
        std::fs::create_dir_all(&base).unwrap();
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(to_relative_path(outside.to_str().unwrap(), &base), None);
    }

    #[test]
    fn to_relative_path_returns_none_for_base_itself() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
        assert_eq!(to_relative_path(base.to_str().unwrap(), &base), None);
    }

    fn lookup_from<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            values
                .iter()
                .find(|(variable_name, _)| *variable_name == name)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn expand_path_tokens_replaces_defined_variables() {
        let lookup = lookup_from(&[("YLVA_HOME", "/home/user/.ylva")]);

        assert_eq!(
            expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
            "/home/user/.ylva/AGENTS.md"
        );
        assert_eq!(
            expand_path_tokens("${YLVA_HOME}/AGENTS.md", &lookup),
            "/home/user/.ylva/AGENTS.md"
        );
        assert_eq!(
            expand_path_tokens("pre$YLVA_HOME_post", &lookup),
            "pre$YLVA_HOME_post",
            "an underscore is part of the variable name, not a delimiter"
        );
    }

    #[test]
    fn expand_path_tokens_leaves_undefined_or_malformed_tokens_literal() {
        let lookup = lookup_from(&[]);

        for value in [
            "$UNDEFINED/file.md",
            "${UNDEFINED}/file.md",
            "${unterminated",
            "${}",
            "$1/file.md",
            "cost$5/file.md",
            "$",
            "src/main.rs",
        ] {
            assert_eq!(expand_path_tokens(value, &lookup), value, "for {value:?}");
        }
    }

    #[test]
    fn expand_path_tokens_substitutes_once_without_rescanning() {
        let lookup = lookup_from(&[("FIRST", "$SECOND"), ("SECOND", "resolved")]);

        assert_eq!(
            expand_path_tokens("$FIRST", &lookup),
            "$SECOND",
            "a substituted value is never rescanned for further tokens"
        );
    }

    #[test]
    fn expand_path_tokens_expands_only_a_leading_tilde() {
        let lookup = lookup_from(&[("HOME", "/home/user")]);

        assert_eq!(expand_path_tokens("~", &lookup), "/home/user");
        assert_eq!(
            expand_path_tokens("~/notes.md", &lookup),
            "/home/user/notes.md"
        );
        assert_eq!(
            expand_path_tokens("~user/notes.md", &lookup),
            "~user/notes.md",
            "`~user` expansion is deliberately unsupported"
        );
        assert_eq!(
            expand_path_tokens("docs/~template.md", &lookup),
            "docs/~template.md",
            "only a leading tilde is expanded"
        );
    }

    #[test]
    fn expand_path_tokens_without_any_lookup_returns_the_value_unchanged() {
        let lookup = lookup_from(&[]);

        assert_eq!(expand_path_tokens("src/main.rs", &lookup), "src/main.rs");
        assert_eq!(expand_path_tokens("~/notes.md", &lookup), "~/notes.md");
        assert_eq!(
            expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
            "$YLVA_HOME/AGENTS.md"
        );
    }

    #[test]
    fn sandbox_rule_config_named_variant_parses_from_toml() {
        let parsed: SandboxRuleConfig = toml::from_str(
            r#"
            rule = "dot_path"
            verdict = "user_approval_required"
            "#,
        )
        .expect("named rule config parses");
        assert!(matches!(
            parsed,
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                ..
            }
        ));
    }

    #[test]
    fn sandbox_rule_config_path_glob_variant_parses_from_toml() {
        let parsed: SandboxRuleConfig = toml::from_str(
            r#"
            pattern = "**/*.secret"
            verdict = "auto_denied"
            reason = "no secrets"
            "#,
        )
        .expect("glob rule config parses");
        match parsed {
            SandboxRuleConfig::PathGlob {
                pattern,
                verdict,
                reason,
            } => {
                assert_eq!(pattern, "**/*.secret");
                assert_eq!(
                    verdict.to_approval_check(reason.as_deref(), "fallback"),
                    ApprovalCheck::AutoDenied {
                        reason: "no secrets".to_string()
                    }
                );
            }
            other => panic!("expected PathGlob variant, got {other:?}"),
        }
    }

    #[test]
    fn has_misplaced_command_glob_wildcard_detects_non_trailing_single_wildcard() {
        let rule = SandboxRuleConfig::CommandGlob {
            command: vec!["git".to_string(), "*".to_string(), "status".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        };
        assert!(rule.has_misplaced_command_glob_wildcard());
    }

    #[test]
    fn has_misplaced_command_glob_wildcard_detects_non_trailing_multi_wildcard() {
        let rule = SandboxRuleConfig::CommandGlob {
            command: vec!["git".to_string(), "**".to_string(), "status".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        };
        assert!(rule.has_misplaced_command_glob_wildcard());
    }

    #[test]
    fn has_misplaced_command_glob_wildcard_abstains_on_trailing_wildcard() {
        let single = SandboxRuleConfig::CommandGlob {
            command: vec!["git".to_string(), "status".to_string(), "*".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        };
        let multi = SandboxRuleConfig::CommandGlob {
            command: vec!["git".to_string(), "status".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        };
        assert!(!single.has_misplaced_command_glob_wildcard());
        assert!(!multi.has_misplaced_command_glob_wildcard());
    }

    #[test]
    fn has_misplaced_command_glob_wildcard_abstains_on_non_command_glob_rules() {
        let named = SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        };
        let glob = SandboxRuleConfig::PathGlob {
            pattern: "**".to_string(),
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        };
        assert!(!named.has_misplaced_command_glob_wildcard());
        assert!(!glob.has_misplaced_command_glob_wildcard());
    }

    #[test]
    fn approval_verdict_config_auto_denied_defaults_reason_when_omitted() {
        let parsed = ApprovalVerdictConfig::AutoDenied;
        assert_eq!(
            parsed.to_approval_check(None, "default reason"),
            ApprovalCheck::AutoDenied {
                reason: "default reason".to_string()
            }
        );
    }

    #[test]
    fn configured_named_rule_overrides_default_dot_path_verdict() {
        // A `[tools.read]`-style config changing `dot_path`'s verdict to
        // `AutoApproved` for this tool only — observable against the default
        // floor, which asks for user approval; `outside_workspace`
        // protection is intentionally NOT included, confirming the
        // full-replace (not merge) semantics.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            },
        ]);

        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoApproved),
            "configured dot_path rule should override the default floor's \
             user-approval verdict"
        );

        // outside_workspace is not part of this tool's configured list, so it
        // no longer fires (full replace, not merge) — sandbox abstains.
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(
            context.check_path(&outside),
            None,
            "outside_workspace should not fire when omitted from configured_rules"
        );
    }

    #[test]
    fn unconfigured_tool_falls_back_to_default_floor() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().join("workspace");
        let sandbox = make_sandbox(&workspace);
        // No with_configured_rules call: default floor applies to every tool.
        let context = SandboxContext::new(Arc::clone(&sandbox));

        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::UserApprovalRequired)
        );
        let outside = temporary_dir.path().join("other/file.rs");
        assert_eq!(
            context.check_path(&outside),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }

    #[test]
    fn configured_rules_mix_named_and_glob_last_match_wins() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let canonical_workspace = sandbox.read().unwrap().workspace_root.clone();
        let allow_pattern = format!("{}/allowed/**", canonical_workspace.to_string_lossy());

        // List order: dot_path (AutoDenied) then a glob rule that re-approves
        // one specific dot-path — last match wins, so the glob rule (later in
        // the list) should override the earlier named rule for matching paths.
        let dot_allow_pattern =
            format!("{}/.allowed_dot/**", canonical_workspace.to_string_lossy());
        let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: dot_allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);

        // Regular dot-path still denied (only the named rule matches it).
        let dot_path = workspace.join(".env");
        assert!(matches!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));

        // Dot-path under .allowed_dot: named dot_path rule matches (deny),
        // then the later glob rule also matches and overrides to approved —
        // proving last-match-wins across Named and Glob in one list.
        let allowed_dot_path = workspace.join(".allowed_dot/file.txt");
        assert_eq!(
            context.check_path(&allowed_dot_path),
            Some(ApprovalCheck::AutoApproved)
        );

        // Non-dot path under allowed/: only the glob rule matches.
        let allowed_path = workspace.join("allowed/file.txt");
        assert_eq!(
            context.check_path(&allowed_path),
            Some(ApprovalCheck::AutoApproved)
        );
    }

    #[test]
    fn auto_approve_catch_all_wins_over_configured_deny_rule() {
        // Mirrors --auto-approve-write/--auto-approve-edit: the catch-all
        // `with_rule` is appended after `with_configured_rules` and must win
        // even though the configured list would otherwise deny the path.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            }])
            .with_rule(SandboxRule {
                verdict: ApprovalCheck::AutoApproved,
                pattern: glob::Pattern::new("**").unwrap(),
            });

        let dot_path = workspace.join(".env");
        assert_eq!(
            context.check_path(&dot_path),
            Some(ApprovalCheck::AutoApproved),
            "catch-all auto-approve rule must win over a configured deny rule"
        );
    }

    /// A context whose configured list contains only one `cross_filesystem`
    /// rule with the given verdict/reason (the configured list fully
    /// replaces the default floor).
    fn cross_filesystem_configured_context(
        sandbox: &Arc<RwLock<Sandbox>>,
        verdict: ApprovalVerdictConfig,
        reason: Option<String>,
    ) -> SandboxContext {
        SandboxContext::new(Arc::clone(sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict,
                reason,
                target: None,
            },
        ])
    }

    #[test]
    fn is_cross_device_requires_two_known_and_different_ids() {
        assert!(is_cross_device(Some(1), Some(2)), "different ids match");
        assert!(!is_cross_device(Some(1), Some(1)), "equal ids abstain");
        assert!(
            !is_cross_device(Some(1), None),
            "unknown workspace-root metadata abstains"
        );
        assert!(
            !is_cross_device(None, Some(2)),
            "unknown candidate metadata abstains"
        );
        assert!(!is_cross_device(None, None), "both sides unknown abstains");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_device_returns_some_for_existing_path_and_none_for_missing() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let existing = temporary_dir.path().join("present.txt");
        std::fs::write(&existing, "x").unwrap();
        assert!(
            resolve_device(&existing).is_some(),
            "an existing path yields its st_dev on unix"
        );
        assert_eq!(
            resolve_device(&temporary_dir.path().join("missing.txt")),
            None,
            "a path with no metadata yields None"
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn resolve_device_is_always_none_on_non_unix_targets() {
        assert_eq!(
            resolve_device(Path::new("/")),
            None,
            "non-unix targets never report a device, so cross_filesystem never matches"
        );
    }

    #[test]
    fn check_path_cross_filesystem_does_not_match_same_device_path() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::UserApprovalRequired,
            None,
        );

        let existing_path = workspace.join("src/main.rs");
        std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
        std::fs::write(&existing_path, "fn main() {}").unwrap();
        assert_eq!(
            context.check_path(&existing_path),
            None,
            "an existing same-device path inside the workspace must not match"
        );
        assert_eq!(
            context.check_path(&workspace.join("not-yet-created.txt")),
            None,
            "a missing target whose deepest existing ancestor is the workspace \
             (same device) must not match"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_path_cross_filesystem_matches_path_on_a_different_device() {
        // `/dev` (devfs/tmpfs) is a different device from a normal workspace
        // tempdir on macOS and Linux — no mounts required.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let workspace_device = resolve_device(&sandbox.read().unwrap().workspace_root)
            .expect("the workspace root is stat-able");
        let cross_device_probe = resolve_device(Path::new("/dev")).expect("/dev is stat-able");
        assert_ne!(
            workspace_device, cross_device_probe,
            "fixture assumption: /dev must be a different device than the workspace tempdir"
        );

        let context = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::UserApprovalRequired,
            None,
        );
        assert_eq!(
            context.check_path(Path::new("/dev/null")),
            Some(ApprovalCheck::UserApprovalRequired),
            "a path on a different device than the workspace root must match, \
             even though the configured list replaced the default floor \
             (the floor's outside_workspace check never runs)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_path_cross_filesystem_matches_missing_target_via_cross_device_ancestor() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::UserApprovalRequired,
            None,
        );

        assert_eq!(
            context.check_path(Path::new("/dev/ylva-cross-fs-missing/deep/file.txt")),
            Some(ApprovalCheck::UserApprovalRequired),
            "a nonexistent target whose deepest existing ancestor is /dev (a \
             different device) must match via the ancestor anchor"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_path_cross_filesystem_honors_each_verdict() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let cross_device_path = Path::new("/dev/null");

        let approved = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::AutoApproved,
            None,
        );
        assert_eq!(
            approved.check_path(cross_device_path),
            Some(ApprovalCheck::AutoApproved)
        );

        let denied =
            cross_filesystem_configured_context(&sandbox, ApprovalVerdictConfig::AutoDenied, None);
        assert_eq!(
            denied.check_path(cross_device_path),
            Some(ApprovalCheck::AutoDenied {
                reason: CROSS_FILESYSTEM_REASON.to_string(),
            }),
            "auto_denied without a custom reason falls back to CROSS_FILESYSTEM_REASON"
        );

        let custom_denied = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::AutoDenied,
            Some("cross-device reads are blocked".to_string()),
        );
        assert_eq!(
            custom_denied.check_path(cross_device_path),
            Some(ApprovalCheck::AutoDenied {
                reason: "cross-device reads are blocked".to_string(),
            }),
            "a custom reason overrides the default denial reason"
        );

        let approval_required = cross_filesystem_configured_context(
            &sandbox,
            ApprovalVerdictConfig::UserApprovalRequired,
            None,
        );
        assert_eq!(
            approval_required.check_path(cross_device_path),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_path_cross_filesystem_last_match_wins_against_outside_workspace() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let cross_device_path = Path::new("/dev/null");

        let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::OutsideWorkspace,
                    verdict: ApprovalVerdictConfig::AutoDenied,
                    reason: None,
                    target: None,
                },
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::CrossFilesystem,
                    verdict: ApprovalVerdictConfig::UserApprovalRequired,
                    reason: None,
                    target: None,
                },
            ]);
        assert_eq!(
            cross_filesystem_last.check_path(cross_device_path),
            Some(ApprovalCheck::UserApprovalRequired),
            "both rules match a cross-device path outside the workspace; the \
             later cross_filesystem rule wins"
        );

        let outside_workspace_last = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::CrossFilesystem,
                    verdict: ApprovalVerdictConfig::UserApprovalRequired,
                    reason: None,
                    target: None,
                },
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::OutsideWorkspace,
                    verdict: ApprovalVerdictConfig::AutoDenied,
                    reason: None,
                    target: None,
                },
            ]);
        assert!(matches!(
            outside_workspace_last.check_path(cross_device_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn check_path_cross_filesystem_last_match_wins_against_path_glob() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let cross_device_path = Path::new("/dev/null");

        let glob_last = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: "/dev/**".to_string(),
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ]);
        assert_eq!(
            glob_last.check_path(cross_device_path),
            Some(ApprovalCheck::AutoApproved),
            "the later path glob re-approves the cross-device path"
        );

        let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![
                SandboxRuleConfig::PathGlob {
                    pattern: "/dev/**".to_string(),
                    verdict: ApprovalVerdictConfig::AutoApproved,
                    reason: None,
                },
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::CrossFilesystem,
                    verdict: ApprovalVerdictConfig::AutoDenied,
                    reason: None,
                    target: None,
                },
            ]);
        assert!(matches!(
            cross_filesystem_last.check_path(cross_device_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));
    }

    #[test]
    fn check_path_dot_path_ordering_unaffected_by_cross_filesystem_on_same_device() {
        // A dot path inside the workspace is always on the workspace's
        // device (no mount involved), so `cross_filesystem` abstains and
        // `dot_path`'s verdict applies in either list order.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let dot_path = workspace.join(".env");

        let dot_path_last = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
        ]);
        assert!(matches!(
            dot_path_last.check_path(&dot_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ));

        let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
            .with_configured_rules(vec![
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::DotPath,
                    verdict: ApprovalVerdictConfig::AutoDenied,
                    reason: None,
                    target: None,
                },
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::CrossFilesystem,
                    verdict: ApprovalVerdictConfig::AutoApproved,
                    reason: None,
                    target: None,
                },
            ]);
        assert!(
            matches!(
                cross_filesystem_last.check_path(&dot_path),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "cross_filesystem does not match a same-device path, so the later \
             rule cannot override the earlier dot_path verdict"
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_path_default_floor_does_not_include_cross_filesystem() {
        // Unconfigured tool: the built-in floor applies. With `/` as the
        // workspace root, `/dev/null` is *inside* the workspace but on a
        // different device, so `cross_filesystem` is the only rule that could
        // match — and because it stays opt-in, the sandbox abstains.
        let workspace = Path::new("/");
        let sandbox = make_sandbox(workspace);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let workspace_device = resolve_device(workspace).expect("/ is stat-able");
        let candidate_device =
            resolve_device(Path::new("/dev/null")).expect("/dev/null is stat-able");
        assert_ne!(
            workspace_device, candidate_device,
            "fixture assumption: /dev/null must live on a different device than /"
        );

        assert_eq!(
            context.check_path(Path::new("/dev/null")),
            None,
            "cross_filesystem is opt-in: the default floor abstains for a \
             cross-device path inside the workspace"
        );
        assert_eq!(
            context.check_path(&workspace.join("file.txt")),
            None,
            "an unconfigured tool still abstains for a normal in-workspace path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn auto_approve_catch_all_wins_over_configured_cross_filesystem_rule() {
        // Mirrors --auto-approve-read-style catch-all: appended last, so it
        // wins over a configured cross_filesystem rule for a cross-device path.
        let temporary_dir = tempfile::tempdir().unwrap();
        let workspace = temporary_dir.path().to_path_buf();
        let sandbox = make_sandbox(&workspace);
        let context =
            cross_filesystem_configured_context(&sandbox, ApprovalVerdictConfig::AutoDenied, None)
                .with_rule(SandboxRule {
                    verdict: ApprovalCheck::AutoApproved,
                    pattern: glob::Pattern::new("**").unwrap(),
                });

        assert_eq!(
            context.check_path(Path::new("/dev/null")),
            Some(ApprovalCheck::AutoApproved),
            "the catch-all rule must win over a configured cross_filesystem deny"
        );
    }

    // ── network_request rule ──────────────────────────────────────────────────

    /// A configured rule entry for the network-tool-only `network_request`
    /// named rule; `target` optionally scopes it to request targets whose
    /// `scheme://host` key matches the glob.
    fn network_request_rule(
        verdict: ApprovalVerdictConfig,
        reason: Option<&str>,
        target: Option<&str>,
    ) -> SandboxRuleConfig {
        SandboxRuleConfig::Named {
            rule: NamedSandboxRule::NetworkRequest,
            verdict,
            reason: reason.map(str::to_string),
            target: target.map(str::to_string),
        }
    }

    /// A representative `scheme://host` key of a network call.
    const CALL_TARGET: &str = "https://stub.example";

    #[test]
    fn check_network_request_abstains_without_config() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()));
        assert_eq!(
            context.check_network_request(CALL_TARGET),
            None,
            "no [tools.web_search] config → no floor; the tool's own default applies"
        );
    }

    #[test]
    fn configured_targetless_network_request_rule_applies_to_every_call() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context =
            SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
                network_request_rule(ApprovalVerdictConfig::AutoApproved, None, None),
            ]);
        assert_eq!(
            context.check_network_request("https://docs.rs/anything"),
            Some(ApprovalCheck::AutoApproved),
            "a rule without target matches every call — there is no target to inspect"
        );
        assert_eq!(
            context.check_network_request("http://example.com:8080"),
            Some(ApprovalCheck::AutoApproved)
        );
    }

    #[test]
    fn configured_network_request_last_match_wins() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![
                network_request_rule(ApprovalVerdictConfig::AutoApproved, None, None),
                network_request_rule(ApprovalVerdictConfig::AutoDenied, Some("disabled"), None),
            ]);
        assert_eq!(
            context.check_network_request(CALL_TARGET),
            Some(ApprovalCheck::AutoDenied {
                reason: "disabled".to_string()
            }),
            "rule lists are ordinary last-match-wins for network tools"
        );
    }

    #[test]
    fn reasonless_network_request_deny_uses_the_default_reason() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context =
            SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
                network_request_rule(ApprovalVerdictConfig::AutoDenied, None, None),
            ]);
        assert_eq!(
            context.check_network_request(CALL_TARGET),
            Some(ApprovalCheck::AutoDenied {
                reason: NETWORK_REQUEST_REASON.to_string()
            })
        );
    }

    #[test]
    fn auto_approve_catch_all_wins_over_configured_network_deny() {
        // Mirrors --auto-approve-web-search: `sandbox_context_for` appends
        // the `**` catch-all last, so the flag wins over [tools.web_search].
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoDenied,
                None,
                None,
            )])
            .with_rule(SandboxRule {
                verdict: ApprovalCheck::AutoApproved,
                pattern: glob::Pattern::new("**").unwrap(),
            });
        assert_eq!(
            context.check_network_request(CALL_TARGET),
            Some(ApprovalCheck::AutoApproved),
            "the CLI catch-all must win regardless of [tools.web_search] config"
        );
    }

    #[test]
    fn path_and_command_rules_never_match_network_calls() {
        // Defense in depth: config validation rejects both kinds under
        // [tools.web_search]; if one slips through it must still abstain
        // rather than auto-approve a call it cannot evaluate.
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![
                SandboxRuleConfig::PathGlob {
                    pattern: "**".to_string(),
                    verdict: ApprovalVerdictConfig::AutoApproved,
                    reason: None,
                },
                SandboxRuleConfig::CommandGlob {
                    command: vec!["git".to_string()],
                    verdict: ApprovalVerdictConfig::AutoApproved,
                    reason: None,
                },
                SandboxRuleConfig::Named {
                    rule: NamedSandboxRule::OutsideWorkspace,
                    verdict: ApprovalVerdictConfig::AutoApproved,
                    reason: None,
                    target: None,
                },
            ]);
        assert_eq!(context.check_network_request(CALL_TARGET), None);
    }

    #[test]
    fn targeted_network_request_rule_matches_only_its_target() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://docs.rs"),
            )]);
        assert_eq!(
            context.check_network_request("https://docs.rs"),
            Some(ApprovalCheck::AutoApproved),
            "an exact scheme://host target matches"
        );
        assert_eq!(
            context.check_network_request("http://docs.rs"),
            None,
            "a scheme mismatch abstains"
        );
        assert_eq!(
            context.check_network_request("https://docs.rs.evil.example"),
            None,
            "a different host abstains"
        );
    }

    #[test]
    fn targeted_network_request_rule_supports_wildcard_globs() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://*.example.com"),
            )]);
        assert_eq!(
            context.check_network_request("https://api.example.com"),
            Some(ApprovalCheck::AutoApproved),
            "a host wildcard matches"
        );
        assert_eq!(
            context.check_network_request("https://example.com"),
            None,
            "the wildcard does not match the bare host"
        );
        assert_eq!(
            context.check_network_request("https://a.b.example.com"),
            None,
            "`*` matches exactly one host label"
        );

        let any_depth = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://**.example.com"),
            )]);
        assert_eq!(
            any_depth.check_network_request("https://api.example.com"),
            Some(ApprovalCheck::AutoApproved)
        );
        assert_eq!(
            any_depth.check_network_request("https://a.b.example.com"),
            Some(ApprovalCheck::AutoApproved),
            "`**` matches subdomains of any depth"
        );
        assert_eq!(
            any_depth.check_network_request("https://example.com"),
            None,
            "`**` matches one or more labels — the apex needs its own rule"
        );

        let any_scheme =
            SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
                network_request_rule(ApprovalVerdictConfig::AutoApproved, None, Some("*://host")),
            ]);
        assert_eq!(
            any_scheme.check_network_request("http://host"),
            Some(ApprovalCheck::AutoApproved)
        );
        assert_eq!(
            any_scheme.check_network_request("https://host"),
            Some(ApprovalCheck::AutoApproved)
        );
        assert_eq!(any_scheme.check_network_request("https://other"), None);
    }

    #[test]
    fn targeted_network_request_rule_matches_effective_ports() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://github.com"),
            )]);
        assert_eq!(
            context.check_network_request("https://github.com:443"),
            Some(ApprovalCheck::AutoApproved),
            "an explicitly spelled default port matches an absent port"
        );
        assert_eq!(
            context.check_network_request("https://github.com:8443"),
            None,
            "a non-default port does not match"
        );

        let explicit_default = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://github.com:443"),
            )]);
        assert_eq!(
            explicit_default.check_network_request("https://github.com"),
            Some(ApprovalCheck::AutoApproved),
            "the default-port spelling matches the bare key too"
        );

        let any_port = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://github.com:*"),
            )]);
        assert_eq!(
            any_port.check_network_request("https://github.com:8443"),
            Some(ApprovalCheck::AutoApproved),
            "`:*` matches any port"
        );
        assert_eq!(
            any_port.check_network_request("https://github.com"),
            Some(ApprovalCheck::AutoApproved),
            "`:*` matches keys without an explicit port too"
        );
    }

    #[test]
    fn targeted_network_request_rule_defaults_a_schemeless_target_to_https() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("github.com"),
            )]);
        assert_eq!(
            context.check_network_request("https://github.com"),
            Some(ApprovalCheck::AutoApproved),
            "a schemeless target means https"
        );
        assert_eq!(
            context.check_network_request("http://github.com"),
            None,
            "http still abstains — the https default can only narrow"
        );
    }

    #[test]
    fn non_matching_target_abstains() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoDenied,
                Some("blocked"),
                Some("https://ads.example.com"),
            )]);
        assert_eq!(
            context.check_network_request("https://docs.rs"),
            None,
            "a non-matching target abstains instead of applying the verdict"
        );
    }

    #[test]
    fn targeted_network_request_rules_follow_last_match_wins() {
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![
                network_request_rule(
                    ApprovalVerdictConfig::AutoApproved,
                    None,
                    Some("*://docs.rs"),
                ),
                network_request_rule(
                    ApprovalVerdictConfig::AutoDenied,
                    Some("https only"),
                    Some("https://docs.rs"),
                ),
            ]);
        assert_eq!(
            context.check_network_request("https://docs.rs"),
            Some(ApprovalCheck::AutoDenied {
                reason: "https only".to_string()
            }),
            "the later matching rule wins"
        );
        assert_eq!(
            context.check_network_request("http://docs.rs"),
            Some(ApprovalCheck::AutoApproved),
            "only the earlier rule matches this target"
        );
    }

    #[test]
    fn catch_all_wins_over_a_matching_target_rule() {
        // Mirrors --auto-approve-web-fetch: the appended `**` catch-all is
        // evaluated last and wins over any configured target rule.
        let temporary_dir = tempfile::tempdir().unwrap();
        let context = SandboxContext::new(make_sandbox(temporary_dir.path()))
            .with_configured_rules(vec![network_request_rule(
                ApprovalVerdictConfig::AutoDenied,
                None,
                Some("https://docs.rs"),
            )]);
        let context = context.with_rule(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: glob::Pattern::new("**").unwrap(),
        });
        assert_eq!(
            context.check_network_request("https://docs.rs"),
            Some(ApprovalCheck::AutoApproved),
            "the CLI catch-all must win regardless of a matching target rule"
        );
    }

    #[test]
    fn target_parameter_is_rejected_on_non_network_rule_kinds() {
        assert!(network_request_rule(
            ApprovalVerdictConfig::AutoApproved,
            None,
            Some("https://docs.rs")
        )
        .has_target());
        assert!(
            !network_request_rule(ApprovalVerdictConfig::AutoApproved, None, None).has_target()
        );
        assert!(
            !SandboxRuleConfig::PathGlob {
                pattern: "**".to_string(),
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            }
            .has_target(),
            "path rules cannot carry a target"
        );
    }
}
