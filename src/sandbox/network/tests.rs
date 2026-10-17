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
use crate::sandbox::rules::NETWORK_REQUEST_REASON;
use crate::sandbox::tests::make_sandbox;
use crate::sandbox::{ApprovalVerdictConfig, SandboxRule, SandboxRuleConfig};

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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://docs.rs"),
            ),
        ]);
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://*.example.com"),
            ),
        ]);
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

    let any_depth =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://**.example.com"),
            ),
        ]);
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://github.com"),
            ),
        ]);
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

    let any_port =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("https://github.com:*"),
            ),
        ]);
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoApproved,
                None,
                Some("github.com"),
            ),
        ]);
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoDenied,
                Some("blocked"),
                Some("https://ads.example.com"),
            ),
        ]);
    assert_eq!(
        context.check_network_request("https://docs.rs"),
        None,
        "a non-matching target abstains instead of applying the verdict"
    );
}

#[test]
fn targeted_network_request_rules_follow_last_match_wins() {
    let temporary_dir = tempfile::tempdir().unwrap();
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
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
    let context =
        SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(vec![
            network_request_rule(
                ApprovalVerdictConfig::AutoDenied,
                None,
                Some("https://docs.rs"),
            ),
        ]);
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
    assert!(!network_request_rule(ApprovalVerdictConfig::AutoApproved, None, None).has_target());
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

// ── network_private_address rule ──────────────────────────────────────────

fn private_address_rule(verdict: ApprovalVerdictConfig, target: Option<&str>) -> SandboxRuleConfig {
    SandboxRuleConfig::Named {
        rule: NamedSandboxRule::NetworkPrivateAddress,
        verdict,
        reason: None,
        target: target.map(str::to_string),
    }
}

fn context_with(rules: Vec<SandboxRuleConfig>) -> SandboxContext {
    let temporary_dir = tempfile::tempdir().unwrap();
    SandboxContext::new(make_sandbox(temporary_dir.path())).with_configured_rules(rules)
}

#[test]
fn private_addresses_are_not_allowed_without_a_rule() {
    let context = context_with(vec![network_request_rule(
        ApprovalVerdictConfig::AutoApproved,
        None,
        None,
    )]);
    assert!(!context.check_private_address("http://localhost:8080"));
}

#[test]
fn an_approving_private_address_rule_is_scoped_by_target() {
    let context = context_with(vec![private_address_rule(
        ApprovalVerdictConfig::AutoApproved,
        Some("http://localhost:*"),
    )]);
    assert!(context.check_private_address("http://localhost:8080"));
    assert!(!context.check_private_address("http://127.0.0.1:8080"));
    assert!(!context.check_private_address("https://localhost"));

    let everywhere = context_with(vec![private_address_rule(
        ApprovalVerdictConfig::AutoApproved,
        None,
    )]);
    assert!(everywhere.check_private_address("http://10.0.0.5"));
}

#[test]
fn private_address_last_match_wins_and_only_approval_allows() {
    let denied_later = context_with(vec![
        private_address_rule(ApprovalVerdictConfig::AutoApproved, None),
        private_address_rule(
            ApprovalVerdictConfig::AutoDenied,
            Some("http://localhost:*"),
        ),
    ]);
    assert!(!denied_later.check_private_address("http://localhost:8080"));
    assert!(denied_later.check_private_address("http://10.0.0.5"));

    let needs_approval = context_with(vec![private_address_rule(
        ApprovalVerdictConfig::UserApprovalRequired,
        None,
    )]);
    assert!(!needs_approval.check_private_address("http://localhost"));
}

#[test]
fn private_address_rule_does_not_affect_network_request_and_the_cli_flag_does_not_grant_it() {
    let context = context_with(vec![private_address_rule(
        ApprovalVerdictConfig::AutoApproved,
        None,
    )]);
    assert_eq!(context.check_network_request("http://localhost"), None);

    let flagged = context_with(Vec::new()).with_rule(SandboxRule {
        verdict: ApprovalCheck::AutoApproved,
        pattern: glob::Pattern::new("**").unwrap(),
    });
    assert!(!flagged.check_private_address("http://localhost"));
}
