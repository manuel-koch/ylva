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

//! Command structure evaluation and worst-case statement aggregation.

use super::rules::{ResolvedRule, BACKGROUNDED_REASON};
use super::{
    bash_command_extractor, command_arguments, ApprovalCheck, ApprovalVerdict, DecisionMaker,
    NamedSandboxRule, SandboxContext,
};

const UNANALYSABLE_STATEMENT_RULE: &str = "unanalysable_statement";
const UNMATCHED_STATEMENT_RULE: &str = "unmatched_statement";
const ALL_STATEMENTS_APPROVED_RULE: &str = "all_statements_approved";
const WORST_STATEMENT_RULE: &str = "worst_statement";
const BACKGROUNDED_RULE: &str = "backgrounded";
const VARIABLE_ASSIGNMENT_PREFIX_RULE: &str = "variable_assignment_prefix";
const UNQUOTED_GLOB_RULE: &str = "unquoted_glob";
const UNQUOTED_BRACE_EXPANSION_RULE: &str = "unquoted_brace_expansion";

fn builtin(verdict: ApprovalVerdict, rule: &'static str) -> ApprovalCheck {
    ApprovalCheck::builtin(verdict, DecisionMaker::BashSandbox, rule)
}

/// One statement of a `bash` call with its own policy verdict.
pub(crate) enum StatementAnalysis {
    Command {
        extracted: bash_command_extractor::ExtractedCommand,
        verdict: Option<ApprovalCheck>,
    },
    Unanalysable,
}

impl SandboxContext {
    /// Check whether `command` is allowed by the sandbox.
    ///
    /// Returns `None` when no rule fires — the sandbox abstains and the calling
    /// tool applies its own default (typically `UserApprovalRequired` for shell
    /// commands). Returns `Some(verdict)` when a rule has an explicit opinion.
    ///
    /// **Extraction:** `command` is parsed via [`bash_command_extractor::extract`]
    /// into its top-level statements (see that module's doc comment for the
    /// exact `tree-sitter-bash` grammar facts this relies on). A statement
    /// outside the fixed scanning scope (`list`/`pipeline`/`command`, plus
    /// redirected statements containing only fd duplication/closing or
    /// literal `/dev/null` redirects) contributes `UserApprovalRequired`
    /// unless its redirects are in that harmless closed set. File redirects,
    /// substitutions, subshells, control flow, function definitions and
    /// heredocs remain approval-required.
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
    /// structural verdict. An unmatched command alongside an auto-approved
    /// one yields `UserApprovalRequired`, so auto-approval requires every
    /// command to be auto-approved. `self.rules` (e.g. `--auto-approve-bash`'s
    /// catch-all) is then evaluated once, matched against the raw `command`
    /// string, last-match-wins against the structural verdict — so it always
    /// wins regardless of any `[tools.bash]` config, mirroring
    /// [`SandboxContext::check_path`]'s existing precedence.
    ///
    /// Returns `None` only when parsing fails or no extracted statement
    /// produces a verdict, and `self.rules` has no match either (pure
    /// abstention).
    pub fn check_command(&self, command: &str) -> Option<ApprovalCheck> {
        let structural_verdict = match bash_command_extractor::extract(command) {
            bash_command_extractor::ExtractionResult::ParseFailed => None,
            bash_command_extractor::ExtractionResult::Statements(statements) => {
                let mut aggregate: Option<ApprovalCheck> = None;
                let mut has_unmatched_command = false;
                for statement in &statements {
                    let contribution = match statement {
                        bash_command_extractor::ExtractedStatement::Command(extracted) => {
                            let verdict = self.evaluate_command(extracted);
                            has_unmatched_command |= verdict.is_none();
                            verdict
                        }
                        bash_command_extractor::ExtractedStatement::Unanalysable => Some(builtin(
                            ApprovalVerdict::UserApprovalRequired,
                            UNANALYSABLE_STATEMENT_RULE,
                        )),
                    };
                    aggregate = Self::worse_verdict(aggregate, contribution);
                }
                aggregate.map(|winner| match winner.verdict {
                    ApprovalVerdict::AutoApproved if has_unmatched_command => builtin(
                        ApprovalVerdict::UserApprovalRequired,
                        UNMATCHED_STATEMENT_RULE,
                    ),
                    ApprovalVerdict::AutoApproved if statements.len() > 1 => {
                        builtin(ApprovalVerdict::AutoApproved, ALL_STATEMENTS_APPROVED_RULE)
                    }
                    ApprovalVerdict::AutoDenied { .. } | ApprovalVerdict::UserApprovalRequired
                        if statements.len() > 1
                            && winner.rule_origin
                                != super::RuleOrigin::BuiltIn(UNANALYSABLE_STATEMENT_RULE) =>
                    {
                        ApprovalCheck {
                            verdict: winner.verdict.clone(),
                            decision_maker: DecisionMaker::BashSandbox,
                            rule_origin: super::RuleOrigin::BuiltIn(WORST_STATEMENT_RULE),
                            parent: Some(Box::new(winner)),
                        }
                    }
                    _ => winner,
                })
            }
        };

        // `self.rules` (e.g. --auto-approve-*'s catch-all) is always
        // evaluated last, matched against the raw command string, so it wins
        // regardless of the structural verdict above.
        let mut verdict = structural_verdict;
        for (rule, origin) in &self.rules {
            if rule.pattern.matches(command) {
                verdict = Some(ApprovalCheck::new(
                    rule.verdict.clone(),
                    DecisionMaker::BashSandbox,
                    origin.clone(),
                ));
            }
        }

        tracing::debug!(
            command = %command,
            sandbox_verdict = match verdict.as_ref().map(|check| &check.verdict) {
                None => "abstain".to_string(),
                Some(ApprovalVerdict::AutoApproved) => "auto-approved".to_string(),
                Some(ApprovalVerdict::AutoDenied { reason }) => format!("auto-denied: {reason}"),
                Some(ApprovalVerdict::UserApprovalRequired) => "user-approval-required".to_string(),
            },
            "sandbox check_command"
        );

        verdict
    }

    /// Judge every top-level statement of `command` separately, for grant
    /// evaluation. `None` when the script does not parse. A command the
    /// policy has no opinion on is reported with `None` (it needs approval).
    pub(crate) fn analyse_command_statements(
        &self,
        command: &str,
    ) -> Option<Vec<StatementAnalysis>> {
        match bash_command_extractor::extract(command) {
            bash_command_extractor::ExtractionResult::ParseFailed => None,
            bash_command_extractor::ExtractionResult::Statements(statements) => Some(
                statements
                    .into_iter()
                    .map(|statement| match statement {
                        bash_command_extractor::ExtractedStatement::Command(extracted) => {
                            let verdict = self.evaluate_command(&extracted);
                            StatementAnalysis::Command { extracted, verdict }
                        }
                        bash_command_extractor::ExtractedStatement::Unanalysable => {
                            StatementAnalysis::Unanalysable
                        }
                    })
                    .collect(),
            ),
        }
    }

    /// Rank an [`ApprovalCheck`] for worst-case combination:
    /// `AutoDenied` > `UserApprovalRequired` > `AutoApproved`.
    fn approval_check_rank(check: &ApprovalCheck) -> u8 {
        match check.verdict {
            ApprovalVerdict::AutoDenied { .. } => 2,
            ApprovalVerdict::UserApprovalRequired => 1,
            ApprovalVerdict::AutoApproved => 0,
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
                let mut argument_analysis = None;
                for (rule, origin) in configured {
                    match rule {
                        ResolvedRule::Named {
                            rule: named,
                            verdict: rule_verdict,
                            target: _,
                        } => {
                            if Self::evaluate_command_named_rule(*named, command) {
                                verdict = Some(ApprovalCheck::new(
                                    rule_verdict.clone(),
                                    DecisionMaker::BashSandbox,
                                    origin.clone(),
                                ));
                            }
                        }
                        ResolvedRule::CommandGlob {
                            command: pattern,
                            conditions,
                            verdict: rule_verdict,
                        } => {
                            if pattern.matches(&command.argv) {
                                if let Some(conditions) = conditions {
                                    if command.has_variable_assignment_prefix {
                                        continue;
                                    }
                                    let analysis = argument_analysis
                                        .get_or_insert_with(|| command_arguments::parse(command));
                                    if !conditions.matches(analysis) {
                                        continue;
                                    }
                                }
                                verdict = Some(ApprovalCheck::new(
                                    rule_verdict.clone(),
                                    DecisionMaker::BashSandbox,
                                    origin.clone(),
                                ));
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
            | NamedSandboxRule::NetworkRequest
            | NamedSandboxRule::NetworkPrivateAddress
            | NamedSandboxRule::SkillSidecar => false,
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
                Some(builtin(
                    ApprovalVerdict::AutoDenied {
                        reason: BACKGROUNDED_REASON.to_string(),
                    },
                    BACKGROUNDED_RULE,
                )),
            );
        }
        if command.has_variable_assignment_prefix {
            verdict = Self::worse_verdict(
                verdict,
                Some(builtin(
                    ApprovalVerdict::UserApprovalRequired,
                    VARIABLE_ASSIGNMENT_PREFIX_RULE,
                )),
            );
        }
        if command
            .argv
            .iter()
            .any(bash_command_extractor::token_has_unquoted_glob)
        {
            verdict = Self::worse_verdict(
                verdict,
                Some(builtin(
                    ApprovalVerdict::UserApprovalRequired,
                    UNQUOTED_GLOB_RULE,
                )),
            );
        }
        if command
            .argv
            .iter()
            .any(bash_command_extractor::token_has_unquoted_brace_expansion)
        {
            verdict = Self::worse_verdict(
                verdict,
                Some(builtin(
                    ApprovalVerdict::UserApprovalRequired,
                    UNQUOTED_BRACE_EXPANSION_RULE,
                )),
            );
        }
        verdict
    }
}

#[cfg(test)]
mod tests;
