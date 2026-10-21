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
use crate::sandbox::tests::make_sandbox;
use crate::sandbox::{ApprovalVerdictConfig, SandboxRule, SandboxRuleConfig};
use glob::Pattern;
use std::sync::Arc;

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
fn check_command_unmatched_command_in_list_blocks_auto_approval() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let sandbox = make_sandbox(temporary_dir.path());
    let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
        SandboxRuleConfig::CommandGlob {
            command: vec!["cat".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        },
    ]);

    for command in ["rm -rf foo; cat x", "cat x && rm -rf foo", "cat x | sh"] {
        assert_eq!(
            context.check_command(command),
            Some(ApprovalCheck::UserApprovalRequired),
            "an unmatched command must block auto-approval of {command:?}"
        );
    }
}

#[test]
fn check_command_all_commands_auto_approved_list_is_auto_approved() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let sandbox = make_sandbox(temporary_dir.path());
    let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
        SandboxRuleConfig::CommandGlob {
            command: vec!["cat".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        },
    ]);

    assert_eq!(
        context.check_command("cat a; cat b"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(context.check_command("rm -rf foo"), None);
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
