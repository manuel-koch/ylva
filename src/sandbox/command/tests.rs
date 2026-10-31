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
fn harmless_redirects_preserve_default_floor_and_configured_command_verdicts() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let sandbox = make_sandbox(temporary_dir.path());
    let default_context = SandboxContext::new(Arc::clone(&sandbox));
    for (bare, redirected) in [
        ("git status", "git status 2>&1"),
        ("git log", "git log 2>/dev/null"),
    ] {
        assert_eq!(
            default_context.check_command(redirected),
            default_context.check_command(bare),
            "default floor should treat {redirected:?} as {bare:?}"
        );
    }

    let configured_context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["git".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");
    for (bare, redirected) in [
        ("git status", "git status 2>&1"),
        ("git log", "git log 2>/dev/null"),
    ] {
        assert_eq!(
            configured_context.check_command(redirected),
            configured_context.check_command(bare),
            "configured command rule should treat {redirected:?} as {bare:?}"
        );
        assert_eq!(
            configured_context.check_command(redirected),
            Some(ApprovalCheck::AutoApproved)
        );
    }
    assert_eq!(
        configured_context.check_command("git status > out.txt"),
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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["git".to_string(), "status".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["cat".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["cat".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["git".to_string(), "*".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["git".to_string(), "reflog".to_string(), "*".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
fn with_configured_rules_rejects_a_command_glob_rule_with_a_misplaced_wildcard() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let sandbox = make_sandbox(temporary_dir.path());
    let context = SandboxContext::new(Arc::clone(&sandbox)).with_configured_rules(vec![
        SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["git".to_string(), "*".to_string(), "status".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        },
    ]);
    assert!(
        context.is_err(),
        "invalid direct policies must fail construction"
    );
}

fn conditional_context(rules: &str) -> SandboxContext {
    #[derive(serde::Deserialize)]
    struct Rules {
        rules: Vec<SandboxRuleConfig>,
    }
    let rules: Rules = toml::from_str(rules).unwrap();
    SandboxContext::new(Arc::new(std::sync::RwLock::new(
        crate::sandbox::Sandbox::with_root(std::path::PathBuf::from("/workspace")),
    )))
    .with_configured_rules(rules.rules)
    .unwrap()
}

#[test]
fn migrated_sample_uses_argument_filters_and_explicit_structural_protection() {
    #[derive(serde::Deserialize)]
    struct Sample {
        tools: std::collections::HashMap<String, crate::config::ToolSandboxConfig>,
    }
    let sample: Sample = toml::from_str(include_str!(
        "../../../tests/sandbox_evaluation/sample.config.toml"
    ))
    .unwrap();
    let context = SandboxContext::new(Arc::new(std::sync::RwLock::new(
        crate::sandbox::Sandbox::with_root("/workspace".into()),
    )))
    .with_tool_rules("bash", sample.tools["bash"].rules.clone())
    .unwrap();
    for command in [
        "true",
        "false",
        "cat -n file",
        "ls -lah file",
        "ls -ldRt .",
        "head -n 5 file",
        "head -20 file",
        "tail -n 5 file",
        "tail -20 file",
        "head -20 file | tail -5",
        "wc -l file",
        "grep -n -e pattern file",
        "grep -rvH -A8 -B2 -m5 --include '*.rs' --exclude 'generated*' --exclude-dir=target -e needle .",
        "grep -l needle file",
        "grep -c needle file",
        "grep -o needle file",
        "grep -e needle -n file",
        "rg -n pattern file",
        "rg -g '*.rs' -C3 -A2 -B1 needle .",
        "rg --files -g '*.rs'",
        "rg needle . -A2",
        "sort -nr file",
        "sort -k2,2 -nr file",
        "find . -name '*.rs' -print",
        "find . -maxdepth 3 -mindepth 1 -depth -iname '*.rs' -a -path './src/*' -o ! -ipath './target/*' -print",
        "find . '(' -name '*.rs' -or -iname '*.md' ')' -and -not -path './target/*' -print",
        "git status",
        "git status -s -b",
        "git --no-pager status --short --branch",
        "git check-ignore file",
        "git check-ignore -v first second",
        "git --no-pager check-ignore --verbose 'file name'",
        "git check-ignore -v -- --output=output",
        "grep --include='*.rs' -e needle .",
        "git log -5",
        "git log -n5",
        "git log --max-count=5",
        "git diff --stat",
        "git --no-pager diff --cached --check --stat --numstat --name-only --unified=3 HEAD",
        "git diff --staged --unified 3",
        "git --no-pager log --oneline --stat --check --numstat --name-only --max-count=5 --unified=3 HEAD",
        "git --no-pager show --oneline --stat --check --numstat --name-only --max-count=1 --unified=3 HEAD",
        "nl --unknown file",
        "cut --unknown file",
        "tr --unknown value",
        "echo --unknown",
        "printf -- '%s' x",
        "sed -n 20,150p file",
        "sed -n -e 1,5p file",
        "sed -n -e 1,5p -e '$p' file",
        "sed -n '1,5p;/needle/p;$p' file",
        "git ls-files --others --exclude-standard",
        "git blame -L 1,5 file",
        "git rev-parse --show-toplevel",
        "git grep -n -e needle",
        "git stash list",
        "git stash show --stat",
        "grep needle -A8 file",
        "grep -r needle --exclude-dir=target .",
        "cd foo",
        "cd -- foo",
        "test --unknown",
    ] {
        assert_eq!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
    for command in [
        "sort -o output file",
        "sort -k2,2 -o output file",
        "sort -k2,2 --compress-program=command file",
        "sort -k",
        "rg --pre command pattern",
        "rg --files --pre=command",
        "rg --files --hostname-bin=command",
        "find . -delete",
        "find . -name '*.rs' -exec command '{}' ';'",
        "find . -name '*.rs' -execdir command '{}' ';'",
        "find . -fprint output",
        "find . -fprint0 output",
        "find . -fprintf output '%p'",
        "printf -v var x",
        "printf --unknown",
        "sed -i 1p file",
        "sed -n 'w out' file",
        "sed -n '1e id' file",
        "sed -f script file",
        "sed -n -e 1,5p -e 'w out' file",
        "sed -n '1,5p;7d' file",
        "git grep -O needle",
        "git stash",
        "git stash push",
        "git ls-files -x pattern",
        "git blame --contents f file",
        "grep needle file -z",
        "grep -A",
        "git diff --output=output",
        "git log -5 --output=output",
        "git log -5x",
        "git --no-pager -c core.pager=command status --short",
        "git --no-pager --config-env=core.pager=VALUE log -5",
        "git --no-pager --exec-path=/tmp log -5",
        "git --no-pager log -5 --output=output",
        "git --no-pager diff --ext-diff",
        "git --no-pager show --textconv HEAD",
        "git --no-pager checkout status",
        "git status --oneline",
        "git log --cached",
        "git diff --max-count=5",
        "git show --branch",
        "git check-ignore",
        "git check-ignore -v",
        "git check-ignore --",
        "git check-ignore --stdin",
        "git check-ignore --no-index file",
        "git check-ignore --unknown -v file",
        "git check-ignore -v --output=output file",
        "git check-ignore --short file",
        "git -c core.excludesFile=other check-ignore -v file",
        "git --config-env=core.excludesFile=VALUE check-ignore file",
        "git --exec-path=/tmp check-ignore file",
        "git -C workspace check-ignore file",
        "git status -v",
        "cat --unknown",
        "cd",
        "cd -",
        "cd -P somewhere",
        "head -20c file",
        "tail -20f file",
        "head -q -20 file",
    ] {
        assert_ne!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
    for command in [
        "cat *.rs",
        "cat {a,b}",
        "FOO=bar cat file",
        "grep -r needle *.rs",
        "rg --files -g *.rs",
        "find . -name *.rs -print",
        "git check-ignore -v *.rs",
        "VALUE=other git check-ignore -v file",
    ] {
        assert_eq!(
            context.check_command(command),
            Some(ApprovalCheck::UserApprovalRequired),
            "{command}"
        );
    }
    assert!(matches!(
        context.check_command("cat file &"),
        Some(ApprovalCheck::AutoDenied { .. })
    ));
}

#[test]
fn conditional_allow_lists_approve_only_complete_recognized_forms() {
    let context = conditional_context(
        r#"
rules = [
  { command = ["cat", "**"], only_arguments = ["-n", "-b"], verdict = "auto_approved" },
  { command = ["ls", "**"], only_arguments = ["-l", "-a", "-h"], verdict = "auto_approved" },
  { command = ["head", "**"], only_arguments = ["-n"], verdict = "auto_approved" },
  { command = ["tail", "**"], only_arguments = ["-n"], verdict = "auto_approved" },
  { command = ["wc", "**"], only_arguments = ["-l"], verdict = "auto_approved" },
  { command = ["grep", "**"], only_arguments = ["-n", "-e"], verdict = "auto_approved" },
  { command = ["rg", "**"], only_arguments = ["-n", "-e"], verdict = "auto_approved" },
  { command = ["sort", "**"], only_arguments = ["-n", "-r"], verdict = "auto_approved" },
  { command = ["find", "**"], only_arguments = ["-name", "-type", "-print"], verdict = "auto_approved" },
  { command = ["git", "**"], only_arguments = ["status", "log", "diff", "show", "--oneline", "--stat"], verdict = "auto_approved" },
]
"#,
    );
    for command in [
        "cat file",
        "cat -bn file",
        "cat --number file",
        "ls -lah file",
        "head -n 10 file",
        "head -10 file",
        "tail -n 5 file",
        "tail -5 file",
        "wc -l file",
        "grep -n -e status file",
        "rg -n -e status file",
        "sort -nr file",
        "find . -name '*.rs' -type f -print",
        "git status",
        "git log --oneline",
        "git diff --stat",
        "git show HEAD",
        "cat file | wc -l",
        "cat 'literal *' && git status",
    ] {
        assert_eq!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
    for command in [
        "sort -o output file",
        "sort file --output=output",
        "rg --pre command pattern",
        "find . -exec rm '{}' +",
        "find . -execdir rm '{}' +",
        "find . -delete",
        "find . -fprint output",
        "find . -fprint0 output",
        "find . -fprintf output '%p'",
        "git -c core.pager=command log",
        "git --config-env=core.pager=VALUE log",
        "git --exec-path=/tmp status",
        "git diff --output=output",
        "git diff --ext-diff",
        "git show --textconv HEAD",
        "git status --made-up",
        "git checkout status",
        "git -C status checkout",
        "git log --format status",
        "git log -5",
        "sed -i '' 's/x/y/' file",
        "awk 'system(\"command\")' file",
        "cat --unknown -n file",
        "head -n",
        "head -10c file",
        "tail -10f file",
        "cat -nz file",
        "find . -weird -exec",
        "git",
        "cat",
        "cat file; unknown",
        "cat file > output",
        "cat $(unknown)",
        "(cat file)",
    ] {
        assert_ne!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
}

#[test]
fn git_log_numeric_counts_obey_short_and_long_count_allow_lists() {
    for alias in ["-n", "--max-count"] {
        let context = conditional_context(&format!(
            r#"
rules = [
  {{ command = ["git", "log", "**"], only_arguments = ["log", "{alias}"], verdict = "auto_approved" }},
]
"#
        ));
        for command in ["git log -5", "git log HEAD -5", "git log '-0005'"] {
            assert_eq!(
                context.check_command(command),
                Some(ApprovalCheck::AutoApproved),
                "{alias}: {command}"
            );
        }
        for command in [
            "git log -5x",
            "git log -5 --output=output",
            "git log --unknown -5",
            "git log --grep -5",
        ] {
            assert_ne!(
                context.check_command(command),
                Some(ApprovalCheck::AutoApproved),
                "{alias}: {command}"
            );
        }
    }
}

#[test]
fn presence_matches_aliases_and_preserves_only_confident_evidence() {
    let context = conditional_context(
        r#"
rules = [
  { command = ["find", "**"], with_arguments = ["-exec*", "-delete", "-fprint*"], verdict = "auto_denied" },
  { command = ["sort", "**"], with_arguments = ["--output"], verdict = "auto_denied" },
  { command = ["cat", "**"], with_arguments = ["--num*"], verdict = "user_approval_required" },
]
"#,
    );
    for command in [
        "find . -delete -unknown",
        "find . -exec command ';'",
        "find . -execdir command +",
        "find . -fprint output",
        "sort -o output file",
        "sort --output=output file",
    ] {
        assert!(
            matches!(
                context.check_command(command),
                Some(ApprovalCheck::AutoDenied { .. })
            ),
            "{command}"
        );
    }
    for command in [
        "find . -name -exec",
        "find ./-exec",
        "find status",
        "find . -weird -exec",
        "sort --unknown -o output",
        "sort -- -o",
        "cat -- -n",
    ] {
        assert_eq!(context.check_command(command), None, "{command}");
    }
    assert_eq!(
        context.check_command("cat -n --unknown"),
        Some(ApprovalCheck::UserApprovalRequired)
    );
}

#[test]
fn composed_conditions_empty_allow_lists_and_argv_shapes_are_independent() {
    let context = conditional_context(
        r#"
rules = [
  { command = ["cat", "**"], only_arguments = [], verdict = "auto_approved" },
  { command = ["cat", "**"], with_arguments = ["--number"], only_arguments = ["-n"], verdict = "auto_denied" },
  { command = ["git"], only_arguments = ["status"], verdict = "auto_approved" },
]
"#,
    );
    assert_eq!(
        context.check_command("cat -- file"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(
        context.check_command("cat -- -n"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert!(matches!(
        context.check_command("cat -n file"),
        Some(ApprovalCheck::AutoDenied { .. })
    ));
    assert_eq!(
        context.check_command("cat -nb file"),
        None,
        "both filters must match"
    );
    assert_eq!(context.check_command("cat --unknown"), None);
    assert_eq!(
        context.check_command("cat"),
        None,
        "** still requires one argument"
    );
    assert_eq!(
        context.check_command("git status"),
        None,
        "literal selectors do not become prefixes"
    );
}

#[test]
fn conditional_rules_do_not_unwrap_assignments_paths_wrappers_or_shell_state() {
    let context = conditional_context(
        r#"
rules = [{ command = ["cat", "**"], only_arguments = [], verdict = "auto_approved" }]
"#,
    );
    for command in [
        "FOO=bar cat file",
        "./cat file",
        "/tmp/cat file",
        "env cat file",
        "command cat file",
        "sudo cat file",
        "time cat file",
        "cd somewhere",
        "export FOO=bar",
        "set -e",
        "alias cat=other",
        "unalias cat",
    ] {
        assert_ne!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
        assert_eq!(
            context.check_command(&format!("cat file; {command}")),
            Some(ApprovalCheck::UserApprovalRequired),
            "{command}"
        );
    }
    let unconditional = conditional_context(
        r#"
rules = [
  { command = ["cat", "**"], verdict = "auto_approved" },
  { command = ["./cat", "**"], verdict = "auto_approved" },
  { command = ["env", "**"], verdict = "auto_approved" },
  { command = ["sed", "**"], verdict = "auto_approved" },
  { command = ["cd", "**"], verdict = "auto_approved" },
]
"#,
    );
    for command in [
        "FOO=bar cat file",
        "./cat file",
        "env cat file",
        "sed -i command file",
        "cd somewhere",
    ] {
        assert_eq!(
            unconditional.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
}

#[test]
fn conditional_vetoes_keep_last_match_and_cli_override_precedence() {
    let context = conditional_context(
        r#"
rules = [
  { command = ["find", "**"], verdict = "auto_approved" },
  { command = ["find", "**"], with_arguments = ["-delete"], verdict = "auto_denied" },
  { command = ["find", "**"], with_arguments = ["-print"], verdict = "user_approval_required" },
]
"#,
    );
    assert!(matches!(
        context.check_command("find . -delete"),
        Some(ApprovalCheck::AutoDenied { .. })
    ));
    assert_eq!(
        context.check_command("find . -delete -print"),
        Some(ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(
        context.check_command("find . -unknown"),
        Some(ApprovalCheck::AutoApproved),
        "grammar does not revoke explicit unconditional approvals"
    );
    assert_eq!(
        context.check_command("find . -name -delete"),
        Some(ApprovalCheck::AutoApproved),
        "values are not identities"
    );
    assert_eq!(
        context.check_command("find .; unmatched"),
        Some(ApprovalCheck::UserApprovalRequired)
    );
    let context = context.with_rule(SandboxRule {
        verdict: ApprovalCheck::AutoApproved,
        pattern: Pattern::new("**").unwrap(),
    });
    for command in ["find . -delete", "FOO=bar cat file", "(unknown)", "unknown"] {
        assert_eq!(
            context.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
}

#[test]
fn check_command_glob_rule_trailing_single_wildcard_abstains_on_unquoted_glob_tail() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let sandbox = make_sandbox(temporary_dir.path());
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["cat".to_string(), "*".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::CommandGlob {
            with_arguments: None,
            only_arguments: None,
            allowed_scripts: None,
            command: vec!["find".to_string(), "**".to_string()],
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
        }])
        .expect("valid sandbox fixture");

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
fn sed_rules_require_allowed_scripts_and_match_each_script_command() {
    let rules = |condition: &str| {
        format!(
            "rules = [{{ command = [\"sed\", \"**\"], only_arguments = [\"-n\", \"-e\"], {condition}verdict = \"auto_approved\" }}]"
        )
    };
    let narrow = conditional_context(&rules(r"allowed_scripts = ['\d+p'], "));
    for (command, approved) in [
        ("sed -n 5p file", true),
        ("sed -n -e 5p -e 7p file", true),
        ("sed -n '5p;7p' file", true),
        ("sed -n 5,7p file", false),
        ("sed -n '5p;7d' file", false),
        ("sed -n 'w out' file", false),
        ("sed -n -e 5p -e 'w out' file", false),
    ] {
        assert_eq!(
            narrow.check_command(command) == Some(ApprovalCheck::AutoApproved),
            approved,
            "{command}"
        );
    }
    // A permissive pattern cannot widen the built-in floor.
    let permissive = conditional_context(&rules("allowed_scripts = ['.*'], "));
    for command in [
        "sed -n 'w out' f",
        "sed -n '1e id' f",
        "sed 's/a/b/e' f",
        "sed -n -f s f",
    ] {
        assert_ne!(
            permissive.check_command(command),
            Some(ApprovalCheck::AutoApproved),
            "{command}"
        );
    }
    assert_eq!(
        permissive.check_command("sed -n 1p f"),
        Some(ApprovalCheck::AutoApproved)
    );
}
