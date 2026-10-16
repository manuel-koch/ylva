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
use super::{bash_command_extractor, ApprovalCheck, NamedSandboxRule, SandboxContext};

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
            | NamedSandboxRule::NetworkRequest
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
mod tests;
