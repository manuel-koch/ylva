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

//! Target-aware network approval and appended-rule precedence.

use super::network_target_pattern::NetworkTargetPattern;
use super::rules::ResolvedRule;
use super::{ApprovalCheck, NamedSandboxRule, SandboxContext};

/// Opaque match target for the rules appended to a network tool's context
/// (e.g. `--auto-approve-web-search`'s `**` catch-all). Network calls have
/// no path or command string to match a glob against, so appended rules are
/// matched against this constant instead — see
/// [`SandboxContext::check_network_request`].
const NETWORK_REQUEST_MATCH_KEY: &str = "network_request";

impl SandboxContext {
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

    /// Whether a connection to a loopback, private or otherwise reserved
    /// address is permitted for `target` (the `scheme://host[:port]` key of the
    /// request or redirect hop). Refused unless the last matching configured
    /// [`NamedSandboxRule::NetworkPrivateAddress`] rule is `auto_approved`.
    /// Appended rules (the `--auto-approve-web-fetch` catch-all) never grant it.
    pub fn check_private_address(&self, target: &str) -> bool {
        let mut allowed = false;
        for rule in self.configured_rules.iter().flatten() {
            if let ResolvedRule::Named {
                rule: NamedSandboxRule::NetworkPrivateAddress,
                verdict,
                target: rule_target,
            } = rule
            {
                if rule_target
                    .as_ref()
                    .is_none_or(|pattern| pattern.matches(target))
                {
                    allowed = matches!(verdict, ApprovalCheck::AutoApproved);
                }
            }
        }
        allowed
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
}

#[cfg(test)]
mod tests;
