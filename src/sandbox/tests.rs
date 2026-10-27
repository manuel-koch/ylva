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
use std::path::Path;

#[test]
fn context_clones_observe_the_same_updated_sandbox_root() {
    let temporary_directory = make_visible_temporary_directory().expect("temporary directory");
    let first_workspace = temporary_directory.path().join("first");
    let second_workspace = temporary_directory.path().join("second");
    std::fs::create_dir_all(&first_workspace).expect("first workspace created");
    std::fs::create_dir_all(&second_workspace).expect("second workspace created");
    let sandbox = make_sandbox(&first_workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let other_context = context.clone();
    let first_path = first_workspace.join("new-file.txt");
    let second_path = second_workspace.join("new-file.txt");
    assert_eq!(context.check_path(&first_path), None);
    assert_eq!(
        other_context.check_path(&second_path),
        Some(ApprovalCheck::UserApprovalRequired)
    );

    sandbox.write().expect("sandbox writable").sandbox_root =
        Sandbox::with_root(second_workspace).sandbox_root;
    for current_context in [&context, &other_context] {
        assert_eq!(current_context.check_path(&second_path), None);
        assert_eq!(
            current_context.check_path(&first_path),
            Some(ApprovalCheck::UserApprovalRequired)
        );
    }
}

#[test]
fn rule_replacement_preserves_appended_precedence_across_policy_owners() {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let context = SandboxContext::new(make_sandbox(temporary_directory.path()))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: Some("dot path".to_string()),
                target: None,
            },
            SandboxRuleConfig::CommandGlob {
                with_arguments: None,
                only_arguments: None,
                allowed_scripts: None,
                command: vec!["git".to_string(), "status".to_string()],
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: ApprovalVerdictConfig::UserApprovalRequired,
                reason: None,
                target: Some("https://docs.rs".to_string()),
            },
        ])
        .expect("valid sandbox fixture");
    assert_eq!(
        context.check_path(Path::new(".env")),
        Some(ApprovalCheck::AutoDenied {
            reason: "dot path".to_string(),
        })
    );
    assert_eq!(
        context.check_command("git status"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(context.check_command("echo hello"), None);
    assert_eq!(
        context.check_network_request("https://docs.rs"),
        Some(ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(context.check_network_request("https://example.com"), None);

    let appended_context = context.clone().with_rule(SandboxRule {
        verdict: ApprovalCheck::AutoApproved,
        pattern: glob::Pattern::new("**").expect("catch-all pattern"),
    });
    let replaced_context = appended_context
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::Backgrounded,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::NetworkRequest,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert_eq!(
        replaced_context.check_path(Path::new(".env")),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(
        replaced_context.check_command("echo hello &"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(
        replaced_context.check_network_request("https://docs.rs"),
        Some(ApprovalCheck::AutoApproved)
    );
    assert_eq!(
        context.check_network_request("https://docs.rs"),
        Some(ApprovalCheck::UserApprovalRequired),
        "replacing the clone's rules must not change the original context"
    );
}

pub(super) fn make_sandbox(root: &Path) -> Arc<RwLock<Sandbox>> {
    Arc::new(RwLock::new(Sandbox::with_root(root.to_path_buf())))
}

pub(super) fn make_visible_temporary_directory() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("ylva-").tempdir()
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
    assert_eq!(sandbox.sandbox_root, canonical_cwd);
}
