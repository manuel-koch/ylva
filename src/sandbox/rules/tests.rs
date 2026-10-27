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
        with_arguments: None,
        only_arguments: None,
        allowed_scripts: None,
        command: vec!["git".to_string(), "*".to_string(), "status".to_string()],
        verdict: ApprovalVerdictConfig::AutoApproved,
        reason: None,
    };
    assert!(rule.has_misplaced_command_glob_wildcard());
}

#[test]
fn has_misplaced_command_glob_wildcard_detects_non_trailing_multi_wildcard() {
    let rule = SandboxRuleConfig::CommandGlob {
        with_arguments: None,
        only_arguments: None,
        allowed_scripts: None,
        command: vec!["git".to_string(), "**".to_string(), "status".to_string()],
        verdict: ApprovalVerdictConfig::AutoApproved,
        reason: None,
    };
    assert!(rule.has_misplaced_command_glob_wildcard());
}

#[test]
fn has_misplaced_command_glob_wildcard_abstains_on_trailing_wildcard() {
    let single = SandboxRuleConfig::CommandGlob {
        with_arguments: None,
        only_arguments: None,
        allowed_scripts: None,
        command: vec!["git".to_string(), "status".to_string(), "*".to_string()],
        verdict: ApprovalVerdictConfig::AutoApproved,
        reason: None,
    };
    let multi = SandboxRuleConfig::CommandGlob {
        with_arguments: None,
        only_arguments: None,
        allowed_scripts: None,
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
fn argument_conditions_reject_invalid_selectors_and_patterns() {
    for source in [
        r#"command = ["awk", "**"]
        only_arguments = ["-n"]"#,
        r#"command = ["awk", "**"]
        only_arguments = []"#,
        r#"command = ["/bin/cat", "**"]
        only_arguments = []"#,
        r#"command = ["c*", "**"]
        only_arguments = []"#,
        r#"command = ["env", "cat", "**"]
        only_arguments = []"#,
        r#"command = []
        only_arguments = []"#,
        r#"command = ["cat", "**"]
        with_arguments = []"#,
        r#"command = ["cat", "**"]
        only_arguments = ["["]"#,
        r#"command = ["cat", "**"]
        with_arguments = [""]"#,
        r#"command = ["git", "**"]
        with_arguments = ["--output=file"]"#,
        r#"command = ["git", "**"]
        with_arguments = ["log --oneline"]"#,
        r#"command = ["sed", "**"]
        only_arguments = ["-n"]"#,
        r#"command = ["sed", "**"]
        only_arguments = ["-n"]
        allowed_scripts = []"#,
        r#"command = ["sed", "**"]
        allowed_scripts = ["("]"#,
        r#"command = ["sed", "**"]
        allowed_scripts = [""]"#,
        r#"command = ["cat", "**"]
        only_arguments = ["-n"]
        allowed_scripts = ["p"]"#,
    ] {
        let rule: SandboxRuleConfig =
            toml::from_str(&format!("{source}\nverdict = \"auto_denied\"")).unwrap();
        assert!(rule.validate().is_err(), "{source}");
    }
}

#[test]
fn argument_fields_are_strict_command_only_lists() {
    for source in [
        r#"rule = "backgrounded"
        only_arguments = []"#,
        r#"pattern = "**"
        with_arguments = ["-n"]"#,
        r#"command = ["cat", "**"]
        only_argument = []"#,
        r#"command = ["cat", "**"]
        only_arguments = "-n""#,
        r#"command = ["cat", "**"]
        with_arguments = [1]"#,
    ] {
        assert!(
            toml::from_str::<SandboxRuleConfig>(&format!("{source}\nverdict = \"auto_denied\""))
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn malformed_direct_veto_rejects_policy_after_unconditional_allow() {
    for restriction in [
        r#"{ command = ["cat", "**"], with_arguments = [], verdict = "auto_denied" }"#,
        r#"{ command = ["cat", "**"], only_arguments = ["["], verdict = "auto_denied" }"#,
        r#"{ pattern = "[", verdict = "auto_denied" }"#,
    ] {
        #[derive(Deserialize)]
        struct Rules {
            rules: Vec<SandboxRuleConfig>,
        }
        let rules: Rules = toml::from_str(&format!(
            r#"rules = [{{ command = ["cat", "**"], verdict = "auto_approved" }}, {restriction}]"#
        ))
        .unwrap();
        let sandbox = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::Sandbox::with_root("/workspace".into()),
        ));
        assert!(
            super::super::SandboxContext::new(sandbox)
                .with_configured_rules(rules.rules)
                .is_err(),
            "{restriction}"
        );
    }
}
