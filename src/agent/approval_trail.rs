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

//! Safe projection of live tool policy checks and their dispositions.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::layered::ConfigLayer;
use crate::grants::GrantReference;
use crate::sandbox::{ApprovalCheck, ApprovalVerdict, DecisionMaker, RuleListSource, RuleOrigin};

pub(super) type RecordId = u64;

#[derive(Debug)]
pub(super) struct PublishedApproval {
    pub check: ApprovalCheck,
    pub record_id: Option<RecordId>,
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    /// The queue's initial approval check before execution or an approval prompt.
    Admission,
    /// The queue revisits a call that was already waiting for approval.
    QueueRecheck,
    /// A grant change triggers a fresh policy and grant check for a pending call.
    GrantRecheck,
    /// The non-interactive CLI reevaluates policy to resolve a pending call.
    CliRecheck,
    /// `load_skill` rechecks policy during execution; only an automatic denial blocks it.
    SkillRecheck,
    /// `web_fetch` checks a redirect to a different target; no new approval prompt is shown.
    NetworkHop,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::QueueRecheck => "queue_recheck",
            Self::GrantRecheck => "grant_recheck",
            Self::CliRecheck => "cli_recheck",
            Self::SkillRecheck => "skill_recheck",
            Self::NetworkHop => "network_hop",
        }
    }

    fn enforcement(self, verdict: &ApprovalVerdict) -> Option<&'static str> {
        match self {
            Self::SkillRecheck | Self::NetworkHop => Some(match verdict {
                ApprovalVerdict::AutoDenied { .. } => "deny",
                ApprovalVerdict::AutoApproved | ApprovalVerdict::UserApprovalRequired => "continue",
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Disposition {
    /// The user approved this call without saving a grant.
    ApproveOnce,
    /// The user explicitly denied this call.
    HumanDeny,
    /// The user saved a grant and approved this call.
    HumanGrant,
    /// Saved grants covered a call that otherwise required user approval.
    GrantMatch,
    /// The call was denied because it still needed user approval and the CLI could not prompt.
    CliUnavailable,
    /// The CLI rechecked policy and found the call could run without user approval.
    CliPolicyApproved,
    /// The CLI rechecked policy and found an explicit denial.
    CliPolicyDenied,
    /// A runtime policy recheck auto-approved a call that was waiting for user approval.
    RecheckApproved,
    /// A queue policy recheck auto-denied a call that was waiting for user approval.
    RecheckDenied,
}

impl Disposition {
    fn fields(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::ApproveOnce => ("approve_once", "human", "host.approve_once"),
            Self::HumanDeny => ("human_denied", "human", "host.deny"),
            Self::HumanGrant => ("grant_and_approve", "human", "host.grant_and_approve"),
            Self::GrantMatch => ("grant_matched", "grants", "grants.covered"),
            Self::CliUnavailable => ("cli_unable_to_ask", "cli", "cli.no_interaction"),
            Self::CliPolicyApproved => ("cli_policy_approved", "cli", "cli.policy_recheck"),
            Self::CliPolicyDenied => ("cli_policy_denied", "cli", "cli.policy_recheck"),
            Self::RecheckApproved => (
                "policy_recheck_approved",
                "runtime",
                "runtime.policy_recheck",
            ),
            Self::RecheckDenied => ("policy_recheck_denied", "runtime", "runtime.policy_recheck"),
        }
    }
}

fn decision_maker(maker: DecisionMaker) -> &'static str {
    match maker {
        DecisionMaker::PathSandbox => "sandbox.path",
        DecisionMaker::BashSandbox => "sandbox.bash",
        DecisionMaker::NetworkSandbox => "sandbox.network",
        DecisionMaker::Tool => "tool",
        DecisionMaker::Registry => "registry",
        DecisionMaker::Schema => "schema",
        DecisionMaker::SkillPolicy => "skills.policy",
    }
}

fn safe_identifier(value: &str) -> Cow<'_, str> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Cow::Borrowed(value)
    } else {
        const IDENTIFIER_DIGEST_BYTES: usize = 8;
        let digest = Sha256::digest(value.as_bytes());
        Cow::Owned(format!(
            "sha256-64:{}",
            crate::hexadecimal::hex_digest(&digest[..IDENTIFIER_DIGEST_BYTES])
        ))
    }
}

fn rule_locator(origin: &RuleOrigin) -> String {
    match origin {
        RuleOrigin::BuiltIn(rule) => format!("builtin:{rule}"),
        RuleOrigin::CliFlag(flag) => format!("cli:{flag}"),
        RuleOrigin::Configured {
            source,
            tool,
            index,
        } => {
            let tool = safe_identifier(tool);
            match source.as_ref() {
                RuleListSource::Profile { layer, agent_type } => {
                    let layer = match layer {
                        ConfigLayer::Workspace => "workspace",
                        ConfigLayer::Home => "ylva_home",
                        ConfigLayer::BuiltIn => "builtin",
                    };
                    match agent_type {
                        Some(agent) => format!(
                            "{layer}:agents.{}.tools.{tool}.rules[{index}]",
                            safe_identifier(agent)
                        ),
                        None => format!("{layer}:tools.{tool}.rules[{index}]"),
                    }
                }
                RuleListSource::AgentTypeFile {
                    scope,
                    agent_type,
                    format,
                } => format!(
                    "agent_file:{scope}:{}.{format}:tools.{tool}.rules[{index}]",
                    safe_identifier(agent_type)
                ),
            }
        }
        RuleOrigin::ProgrammaticRule(index) => format!("programmatic:rules[{index}]"),
        RuleOrigin::Programmatic => "programmatic:override".into(),
    }
}

struct TrailEvent<'a> {
    phase: &'static str,
    outcome: &'static str,
    decision_maker: &'static str,
    rule_locator: &'a str,
    enforcement: Option<&'static str>,
    parent_id: Option<RecordId>,
    grants: Option<&'a str>,
}

fn should_record(tool: &str) -> bool {
    tool != super::tools::clarify::ClarifyTool::NAME && tracing::enabled!(tracing::Level::INFO)
}

fn emit(tool: &str, call_id: Uuid, event: TrailEvent<'_>) -> RecordId {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let record_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    tracing::info!(
        tool = %safe_identifier(tool),
        tool_call_id = %call_id,
        phase = event.phase,
        outcome = event.outcome,
        decision_maker = event.decision_maker,
        rule_locator = event.rule_locator,
        record_id,
        parent_id = event.parent_id,
        grants = event.grants,
        enforcement = event.enforcement,
        "tool approval trail"
    );
    record_id
}

pub(super) fn check(
    tool: &str,
    call_id: Uuid,
    approval: &ApprovalCheck,
    phase: Phase,
    previous: Option<RecordId>,
) -> Option<RecordId> {
    if !should_record(tool) {
        return None;
    }
    let parent_id = approval.parent.as_ref().map_or(previous, |parent| {
        check(tool, call_id, parent, phase, previous)
    });
    Some(emit(
        tool,
        call_id,
        TrailEvent {
            phase: phase.label(),
            outcome: match approval.verdict {
                ApprovalVerdict::AutoApproved => "auto_approved",
                ApprovalVerdict::AutoDenied { .. } => "auto_denied",
                ApprovalVerdict::UserApprovalRequired => "user_approval_required",
            },
            decision_maker: decision_maker(approval.decision_maker),
            rule_locator: &rule_locator(&approval.rule_origin),
            enforcement: phase.enforcement(&approval.verdict),
            parent_id,
            grants: None,
        },
    ))
}

pub(super) fn disposition(
    call: &dyn super::tools::ToolCall,
    disposition: Disposition,
    parent_id: Option<RecordId>,
    grants: &[GrantReference],
) {
    if !should_record(call.name()) {
        return;
    }
    let (outcome, decision_maker, rule_locator) = disposition.fields();
    let grants = grants
        .iter()
        .map(GrantReference::as_str)
        .collect::<Vec<_>>()
        .join(",");
    emit(
        call.name(),
        call.id(),
        TrailEvent {
            phase: "disposition",
            outcome,
            decision_maker,
            rule_locator,
            enforcement: None,
            parent_id,
            grants: (!grants.is_empty()).then_some(grants.as_str()),
        },
    );
}

#[cfg(test)]
pub(super) fn prime_trail_capture() {
    // Prime the shared callsites under the capture subscriber before rebuilding interest.
    let _ = should_record("trail_probe");
    emit(
        "trail_probe",
        Uuid::nil(),
        TrailEvent {
            phase: "probe",
            outcome: "probe",
            decision_maker: "probe",
            rule_locator: "probe",
            enforcement: None,
            parent_id: None,
            grants: None,
        },
    );
    tracing::callsite::rebuild_interest_cache();
}

#[cfg(test)]
pub(super) fn captured_trail_line(line: &str) -> bool {
    line.contains("tool approval trail") && !line.contains("trail_probe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_locator_shortens_hashed_names_without_changing_simple_names() {
        let origin = RuleOrigin::Configured {
            source: RuleListSource::Profile {
                layer: ConfigLayer::Workspace,
                agent_type: Some("team/reviewer".into()),
            }
            .into(),
            tool: "web_fetch".into(),
            index: 1,
        };

        assert_eq!(
            rule_locator(&origin),
            "workspace:agents.sha256-64:632deafee6810518.tools.web_fetch.rules[1]"
        );
    }
}
