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

//! Serialized and compiled rule contracts, including command-glob matching.

use super::network_target_pattern::NetworkTargetPattern;
use super::{bash_command_extractor, command_arguments};
use glob::Pattern;
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
pub(super) const CROSS_FILESYSTEM_REASON: &str =
    "Access denied by security policy: path resides on a \
                                       different filesystem (device) than the sandbox root";

/// Default reason shown when the built-in (or a configured, reason-less)
/// `backgrounded` rule denies a `bash` command.
pub(super) const BACKGROUNDED_REASON: &str =
    "Access denied by security policy: backgrounded (`&`) shell \
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
pub(super) const NETWORK_REQUEST_REASON: &str =
    "Access denied by security policy: network request \
                                      matched a configured sandbox rule";

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
/// [`SandboxRuleConfig::is_path_tool_only`]/[`is_bash_tool_only`]/[`is_network_tool_only`]):
/// the three path rules ([`NamedSandboxRule::OutsideWorkspace`],
/// [`NamedSandboxRule::DotPath`], [`NamedSandboxRule::CrossFilesystem`]) are
/// valid for every tool except `bash` and the network tools; the four
/// `bash`-only command-structure rules, evaluated per extracted command by
/// [`super::SandboxContext::check_command`]/[`bash_command_extractor`]; and
/// [`NamedSandboxRule::NetworkRequest`], valid only under a network tool's
/// config table (today `[tools.web_search]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedSandboxRule {
    /// Matches any path that normalizes outside the workspace root.
    OutsideWorkspace,
    /// Matches any content-tool path whose resolved absolute form has a
    /// component starting with `.` (unless `allow_dot_paths` bypasses it).
    DotPath,
    /// Matches any path whose filesystem device ID (unix `st_dev`) differs
    /// from the canonical sandbox root's, using the deepest existing
    /// ancestor as the anchor when the target itself does not exist.
    /// Fires only on a positively established device difference: unknown
    /// metadata on either side (or any non-unix target) never matches.
    /// Opt-in only — not part of the default floor.
    CrossFilesystem,
    /// Opt-in read-only access to regular resources within callable skills.
    SkillSidecar,
    /// Matches a `bash` command immediately followed by a bare `&`
    /// (backgrounding).
    Backgrounded,
    /// Matches a `bash` command with one or more leading
    /// `variable_assignment` subagents (e.g. `FOO=bar git status`).
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
    /// Permits a network tool to connect to loopback, private, link-local or
    /// otherwise reserved addresses. Refused by default; only
    /// `verdict = "auto_approved"` permits, any other verdict leaves the
    /// refusal in place (last match wins). The optional `target` is matched
    /// against the `scheme://host[:port]` key of the request or redirect hop
    /// being connected to. It is never granted by `--auto-approve-web-fetch`.
    /// Valid only where [`NamedSandboxRule::NetworkRequest`] is.
    NetworkPrivateAddress,
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
pub(super) struct CommandGlobPattern {
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
    pub(super) fn matches(&self, argv: &[bash_command_extractor::ArgvToken]) -> bool {
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
#[serde(try_from = "RawSandboxRuleConfig")]
pub enum SandboxRuleConfig {
    /// `{ rule = "<named_rule>", verdict = "...", reason = "...", target = "..." }`.
    Named {
        rule: NamedSandboxRule,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
        /// Optional [`NetworkTargetPattern`] matched against the request's
        /// `scheme://host[:port]` match key (see
        /// [`super::SandboxContext::check_network_request`]); absent `target` matches
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
        /// Any one confidently identified option/subcommand must match.
        #[serde(default)]
        with_arguments: Option<Vec<String>>,
        /// Every option/subcommand must match, and parsing must be complete.
        #[serde(default)]
        only_arguments: Option<Vec<String>>,
        /// `sed` only (required there): every script command must fully match
        /// one of these regular expressions. Can only narrow the built-in floor.
        #[serde(default)]
        allowed_scripts: Option<Vec<String>>,
        verdict: ApprovalVerdictConfig,
        #[serde(default)]
        reason: Option<String>,
    },
}

/// The keys of any rule table before its kind is decided. Unknown keys are a
/// hard error: a misspelled key must never silently weaken a restriction.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSandboxRuleConfig {
    rule: Option<NamedSandboxRule>,
    pattern: Option<String>,
    command: Option<Vec<String>>,
    verdict: ApprovalVerdictConfig,
    reason: Option<String>,
    target: Option<String>,
    with_arguments: Option<Vec<String>>,
    only_arguments: Option<Vec<String>>,
    allowed_scripts: Option<Vec<String>>,
}

impl TryFrom<RawSandboxRuleConfig> for SandboxRuleConfig {
    type Error = String;

    fn try_from(raw: RawSandboxRuleConfig) -> Result<Self, Self::Error> {
        let RawSandboxRuleConfig {
            rule,
            pattern,
            command,
            verdict,
            reason,
            target,
            with_arguments,
            only_arguments,
            allowed_scripts,
        } = raw;
        if command.is_none()
            && (with_arguments.is_some() || only_arguments.is_some() || allowed_scripts.is_some())
        {
            return Err(
                "`with_arguments`, `only_arguments` and `allowed_scripts` are only valid on `command` entries".into(),
            );
        }
        match (rule, pattern, command) {
            (Some(rule), None, None) => Ok(Self::Named {
                rule,
                verdict,
                reason,
                target,
            }),
            (None, Some(pattern), None) if target.is_none() => Ok(Self::PathGlob {
                pattern,
                verdict,
                reason,
            }),
            (None, None, Some(command)) if target.is_none() => Ok(Self::CommandGlob {
                command,
                with_arguments,
                only_arguments,
                allowed_scripts,
                verdict,
                reason,
            }),
            (None, Some(_), None) | (None, None, Some(_)) => {
                Err("`target` is only valid on a named `rule` entry".to_string())
            }
            _ => Err(
                "a sandbox rule must set exactly one of `rule`, `pattern` or `command`".to_string(),
            ),
        }
    }
}

impl SandboxRuleConfig {
    /// Validate and compile this rule, including argument-condition semantics.
    pub fn validate(&self) -> Result<(), ResolvedRuleError> {
        ResolvedRule::from_config(self.clone()).map(|_| ())
    }

    /// Validate rule families for callers constructing policies directly.
    pub fn validate_for_tool(&self, tool_name: &str) -> Result<(), ResolvedRuleError> {
        let network_tool = matches!(tool_name, "web_search" | "web_fetch");
        let invalid_kind = (self.is_read_tool_only() && tool_name != "read")
            || (self.is_bash_tool_only() && tool_name != "bash")
            || (self.is_network_tool_only() && !network_tool)
            || (self.is_path_tool_only() && (tool_name == "bash" || network_tool))
            || (self.has_target() && !self.is_network_tool_only());
        if invalid_kind {
            return Err(ResolvedRuleError::InvalidRuleKind(tool_name.to_string()));
        }
        Ok(())
    }
    /// Whether this rule is restricted to the `read` tool, not read-only I/O generally.
    pub fn is_read_tool_only(&self) -> bool {
        matches!(
            self,
            Self::Named {
                rule: NamedSandboxRule::SkillSidecar,
                ..
            }
        )
    }
    /// Returns `true` when this is the tool-restricted [`SandboxRuleConfig::CommandGlob`]
    /// variant, or a [`SandboxRuleConfig::Named`] variant referencing one of
    /// the four `bash`-only [`NamedSandboxRule`] variants. Used by
    /// `crate::config::profile::ProfileConfig`'s startup validation to
    /// reject the wrong rule kind under the wrong tool.
    pub fn is_bash_tool_only(&self) -> bool {
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
    pub fn is_path_tool_only(&self) -> bool {
        match self {
            SandboxRuleConfig::PathGlob { .. } => true,
            SandboxRuleConfig::Named { rule, .. } => matches!(
                rule,
                NamedSandboxRule::OutsideWorkspace
                    | NamedSandboxRule::DotPath
                    | NamedSandboxRule::CrossFilesystem
                    | NamedSandboxRule::SkillSidecar
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
    pub fn is_network_tool_only(&self) -> bool {
        matches!(
            self,
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest | NamedSandboxRule::NetworkPrivateAddress,
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

/// An entry in [`super::SandboxContext`]'s resolved per-tool rule list: either a
/// named structural check (re-evaluated dynamically against the current
/// sandbox root/path) or a fixed glob pattern. Built from
/// [`SandboxRuleConfig`] by [`super::SandboxContext::with_configured_rules`]; kept
/// distinct from the public [`SandboxRule`] type (which stays glob-only) so
/// that `SandboxRule`'s existing field shape — relied on by pre-existing
/// tests and by the `--auto-approve-write`/`--auto-approve-edit` catch-all
/// rules — is unaffected by this addition.
#[derive(Debug, Clone)]
pub(super) enum ResolvedRule {
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
        conditions: Option<ArgumentConditions>,
        verdict: ApprovalCheck,
    },
}

#[derive(Debug, Clone)]
pub(super) struct ArgumentConditions {
    with_arguments: Option<Vec<Pattern>>,
    only_arguments: Option<Vec<Pattern>>,
    allowed_scripts: Option<Vec<regex::Regex>>,
}

impl ArgumentConditions {
    fn compile(
        command: &[String],
        with_arguments: Option<Vec<String>>,
        only_arguments: Option<Vec<String>>,
        allowed_scripts: Option<Vec<String>>,
    ) -> Result<Option<Self>, ResolvedRuleError> {
        if with_arguments.is_none() && only_arguments.is_none() && allowed_scripts.is_none() {
            return Ok(None);
        }
        if !command
            .first()
            .is_some_and(|name| command_arguments::supports_command(name))
        {
            return Err(ResolvedRuleError::InvalidArgumentCondition(
                "conditional command requires a literal bare executable with a built-in grammar"
                    .into(),
            ));
        }
        let command_name = command.first().map_or("", String::as_str);
        if allowed_scripts.is_some() != command_arguments::requires_allowed_scripts(command_name) {
            return Err(ResolvedRuleError::InvalidArgumentCondition(
                if allowed_scripts.is_some() {
                    format!("`allowed_scripts` is not valid for `{command_name}`")
                } else {
                    format!("conditional `{command_name}` rules require `allowed_scripts`")
                },
            ));
        }
        let allowed_scripts = allowed_scripts
            .map(|patterns| {
                if patterns.is_empty() || patterns.iter().any(String::is_empty) {
                    return Err(ResolvedRuleError::InvalidArgumentCondition(
                        "`allowed_scripts` needs at least one non-empty pattern".into(),
                    ));
                }
                patterns
                    .iter()
                    .map(|pattern| {
                        regex::Regex::new(&format!("^(?:{pattern})$")).map_err(|error| {
                            ResolvedRuleError::InvalidArgumentCondition(format!(
                                "invalid `allowed_scripts` pattern '{pattern}': {error}"
                            ))
                        })
                    })
                    .collect()
            })
            .transpose()?;
        if with_arguments.as_ref().is_some_and(Vec::is_empty) {
            return Err(ResolvedRuleError::InvalidArgumentCondition(
                "`with_arguments` must not be empty".into(),
            ));
        }
        fn compile_patterns(
            patterns: Option<Vec<String>>,
        ) -> Result<Option<Vec<Pattern>>, ResolvedRuleError> {
            let Some(patterns) = patterns else {
                return Ok(None);
            };
            let mut compiled_patterns = Vec::with_capacity(patterns.len());
            for pattern in patterns {
                if pattern.is_empty()
                    || pattern.chars().any(char::is_whitespace)
                    || pattern.contains('=')
                {
                    return Err(ResolvedRuleError::InvalidArgumentCondition(
                        format!("argument pattern '{pattern}' must match an option/subcommand identity, not values"),
                    ));
                }
                compiled_patterns.push(Pattern::new(&pattern)?);
            }
            Ok(Some(compiled_patterns))
        }
        Ok(Some(Self {
            with_arguments: compile_patterns(with_arguments)?,
            only_arguments: compile_patterns(only_arguments)?,
            allowed_scripts,
        }))
    }

    pub(super) fn matches(&self, analysis: &command_arguments::ArgumentAnalysis) -> bool {
        let matches_identity = |patterns: &[Pattern], aliases: &[&str]| {
            patterns
                .iter()
                .any(|pattern| aliases.iter().any(|alias| pattern.matches(alias)))
        };
        self.with_arguments.as_ref().is_none_or(|patterns| {
            analysis
                .identities
                .iter()
                .any(|identity| matches_identity(patterns, identity))
        }) && self.allowed_scripts.as_ref().is_none_or(|patterns| {
            analysis.complete
                && !analysis.script_commands.is_empty()
                && analysis
                    .script_commands
                    .iter()
                    .all(|script| patterns.iter().any(|pattern| pattern.is_match(script)))
        }) && self.only_arguments.as_ref().is_none_or(|patterns| {
            analysis.complete
                && analysis
                    .identities
                    .iter()
                    .all(|identity| matches_identity(patterns, identity))
        })
    }
}

/// Error produced by [`ResolvedRule::from_config`] when a
/// [`SandboxRuleConfig`] can't be resolved into a working rule.
/// Invalid policies are rejected rather than dropping individual restrictions.
#[derive(Debug, thiserror::Error)]
pub enum ResolvedRuleError {
    #[error(transparent)]
    InvalidGlobPattern(#[from] glob::PatternError),
    #[error("invalid network target pattern: {0}")]
    InvalidNetworkTargetPattern(String),
    /// A `command` rule's `*`/`**` wildcard isn't the last element — see
    /// [`SandboxRuleConfig::has_misplaced_command_glob_wildcard`].
    /// `crate::config::profile::validate_tool_sandbox_rules` already
    /// rejects this as a hard startup error for profile-loaded rules; this
    /// is defense in depth for callers that build [`SandboxRuleConfig`]s
    /// directly and pass them to [`super::SandboxContext::with_configured_rules`]
    /// without going through profile loading.
    #[error(
        "`*`/`**` wildcard is not the last element of `command`; it would silently degrade to \
         a literal (never-matching) argument instead of a wildcard"
    )]
    MisplacedCommandGlobWildcard,
    #[error("invalid argument condition: {0}")]
    InvalidArgumentCondition(String),
    #[error("sandbox rule is not valid for tool '{0}'")]
    InvalidRuleKind(String),
}

impl ResolvedRule {
    pub(super) fn from_config(config: SandboxRuleConfig) -> Result<Self, ResolvedRuleError> {
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
                    NamedSandboxRule::SkillSidecar => "Access denied by skill sidecar policy",
                    NamedSandboxRule::Backgrounded => BACKGROUNDED_REASON,
                    NamedSandboxRule::VariableAssignmentPrefix => VARIABLE_ASSIGNMENT_PREFIX_REASON,
                    NamedSandboxRule::UnquotedGlob => UNQUOTED_GLOB_REASON,
                    NamedSandboxRule::UnquotedBraceExpansion => UNQUOTED_BRACE_EXPANSION_REASON,
                    NamedSandboxRule::NetworkRequest => NETWORK_REQUEST_REASON,
                    NamedSandboxRule::NetworkPrivateAddress => NETWORK_REQUEST_REASON,
                };
                ResolvedRule::Named {
                    rule,
                    verdict: verdict.to_approval_check(reason.as_deref(), default_reason),
                    target: target
                        .map(|pattern| NetworkTargetPattern::parse(&pattern))
                        .transpose()
                        .map_err(|error| {
                            ResolvedRuleError::InvalidNetworkTargetPattern(error.to_string())
                        })?,
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
                with_arguments,
                only_arguments,
                allowed_scripts,
                verdict,
                reason,
            } => ResolvedRule::CommandGlob {
                conditions: ArgumentConditions::compile(
                    &command,
                    with_arguments,
                    only_arguments,
                    allowed_scripts,
                )?,
                command: CommandGlobPattern::from_tokens(&command),
                verdict: verdict
                    .to_approval_check(reason.as_deref(), CONFIGURED_COMMAND_GLOB_REASON),
            },
        })
    }
}

#[cfg(test)]
mod tests;
