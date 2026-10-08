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

use super::*;

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
