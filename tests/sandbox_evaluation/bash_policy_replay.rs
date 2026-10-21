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

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ylva::config::ToolSandboxConfig;
use ylva::{ApprovalCheck, Sandbox, SandboxContext};

const SCHEMA_VERSION: u32 = 1;

#[derive(Parser)]
#[command(about = "Replay JSON Bash commands through Ylva's policy without executing them")]
struct Arguments {
    /// JSON corpus containing schema_version and commands.
    corpus: PathBuf,
    /// TOML file containing explicit [tools.bash] rules; omit for the default floor.
    #[arg(long, value_name = "PROFILE_TOML")]
    rules: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: u32,
    description: Option<String>,
    source: Option<Value>,
    commands: Vec<ReplayCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayCase {
    id: String,
    command: String,
    expected_verdict: Option<Verdict>,
    source: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    AutoApproved,
    AutoDenied,
    UserApprovalRequired,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    description: Option<String>,
    source: Option<Value>,
    summary: Summary,
    results: Vec<CaseResult>,
}

#[derive(Default, Serialize)]
struct Summary {
    total: usize,
    auto_approved: usize,
    user_approval_required: usize,
    auto_denied: usize,
    abstained: usize,
    expectations: usize,
    expectation_mismatches: usize,
}

#[derive(Serialize)]
struct CaseResult {
    id: String,
    command: String,
    sandbox_verdict: Option<Verdict>,
    verdict: Verdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    denial_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_verdict: Option<Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matches_expectation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<Value>,
}

fn policy_context(rules_source: Option<&str>) -> Result<SandboxContext> {
    let context = SandboxContext::new(Arc::new(RwLock::new(Sandbox::new())));
    let Some(rules_source) = rules_source else {
        return Ok(context);
    };
    let profile: toml::Value = toml::from_str(rules_source)?;
    let bash = profile
        .get("tools")
        .and_then(|tools| tools.get("bash"))
        .context("rules file must contain [tools.bash] with an explicit rules array")?;
    let table = bash.as_table().context("[tools.bash] must be a table")?;
    for key in table.keys() {
        if key != "rules" {
            bail!("unknown [tools.bash] field '{key}'");
        }
    }
    if !table.contains_key("rules") {
        bail!("[tools.bash] must contain an explicit rules array");
    }
    let configuration: ToolSandboxConfig = bash.clone().try_into()?;
    Ok(context.with_tool_rules("bash", configuration.rules)?)
}

fn replay(corpus: Corpus, context: &SandboxContext) -> Result<Report> {
    if corpus.schema_version != SCHEMA_VERSION {
        bail!(
            "unsupported corpus schema_version {}; expected {SCHEMA_VERSION}",
            corpus.schema_version
        );
    }
    if corpus.commands.is_empty() {
        bail!("corpus must contain at least one command");
    }
    let mut identifiers = HashSet::new();
    for case in &corpus.commands {
        if case.id.trim().is_empty() {
            bail!("command id must not be blank");
        }
        if !identifiers.insert(&case.id) {
            bail!("duplicate command id '{}'", case.id);
        }
        if case.command.trim().is_empty() {
            bail!("command '{}' must not be blank", case.id);
        }
    }
    let mut summary = Summary {
        total: corpus.commands.len(),
        ..Summary::default()
    };
    let mut results = Vec::with_capacity(corpus.commands.len());
    for case in corpus.commands {
        let (sandbox_verdict, denial_reason) = match context.check_command(&case.command) {
            Some(ApprovalCheck::AutoApproved) => (Some(Verdict::AutoApproved), None),
            Some(ApprovalCheck::AutoDenied { reason }) => (Some(Verdict::AutoDenied), Some(reason)),
            Some(ApprovalCheck::UserApprovalRequired) => {
                (Some(Verdict::UserApprovalRequired), None)
            }
            None => (None, None),
        };
        // Match BashTool's fallback while retaining abstention in the report.
        let verdict = sandbox_verdict.unwrap_or(Verdict::UserApprovalRequired);
        match verdict {
            Verdict::AutoApproved => summary.auto_approved += 1,
            Verdict::AutoDenied => summary.auto_denied += 1,
            Verdict::UserApprovalRequired => summary.user_approval_required += 1,
        }
        summary.abstained += usize::from(sandbox_verdict.is_none());
        let matches_expectation = case.expected_verdict.map(|expected| expected == verdict);
        summary.expectations += usize::from(matches_expectation.is_some());
        summary.expectation_mismatches += usize::from(matches_expectation == Some(false));
        results.push(CaseResult {
            id: case.id,
            command: case.command,
            sandbox_verdict,
            verdict,
            denial_reason,
            expected_verdict: case.expected_verdict,
            matches_expectation,
            source: case.source,
        });
    }
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        description: corpus.description,
        source: corpus.source,
        summary,
        results,
    })
}

fn run(arguments: &Arguments, mut output: impl Write) -> Result<ExitCode> {
    let corpus_source = std::fs::read_to_string(&arguments.corpus)
        .with_context(|| format!("reading corpus {}", arguments.corpus.display()))?;
    let corpus = serde_json::from_str(&corpus_source)
        .with_context(|| format!("parsing corpus {}", arguments.corpus.display()))?;
    let rules_source = arguments
        .rules
        .as_ref()
        .map(|path| {
            std::fs::read_to_string(path)
                .with_context(|| format!("reading rules {}", path.display()))
        })
        .transpose()?;
    let context = policy_context(rules_source.as_deref()).with_context(|| {
        arguments.rules.as_ref().map_or_else(
            || "building default Bash policy".to_string(),
            |path| format!("building Bash policy from {}", path.display()),
        )
    })?;
    let report = replay(corpus, &context)?;
    serde_json::to_writer_pretty(&mut output, &report)?;
    writeln!(output)?;
    output.flush()?;
    Ok(if report.summary.expectation_mismatches == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn main() -> Result<ExitCode> {
    run(&Arguments::parse(), io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RULES: &str = r#"
[tools.bash]
rules = [
    { command = ["cat", "**"], only_arguments = ["-n"], verdict = "auto_approved" },
    { command = ["sort", "**"], only_arguments = ["-n"], verdict = "auto_approved" },
    { rule = "backgrounded", verdict = "auto_denied", reason = "no background execution" },
]
"#;

    #[test]
    fn replay_uses_production_argument_rules_and_whole_script_aggregation() {
        let corpus = serde_json::from_value(json!({
            "schema_version": SCHEMA_VERSION,
            "commands": [
                {"id": "read", "command": "cat -n README.md", "expected_verdict": "auto_approved"},
                {"id": "write-option", "command": "sort -o output.txt input.txt", "expected_verdict": "user_approval_required"},
                {"id": "mixed", "command": "cat README.md && unknown-command", "expected_verdict": "user_approval_required"},
                {"id": "background", "command": "cat README.md &", "expected_verdict": "auto_denied"}
            ]
        })).unwrap();
        let report = replay(corpus, &policy_context(Some(RULES)).unwrap()).unwrap();

        assert_eq!(
            serde_json::to_value(&report.summary).unwrap(),
            json!({
                "total": 4,
                "auto_approved": 1,
                "user_approval_required": 2,
                "auto_denied": 1,
                "abstained": 1,
                "expectations": 4,
                "expectation_mismatches": 0
            })
        );
        assert_eq!(
            report.results[3].denial_reason.as_deref(),
            Some("no background execution")
        );
    }

    #[test]
    fn replay_treats_abstention_as_approval_required_and_reports_expectation_mismatch() {
        let corpus = serde_json::from_value(json!({
            "schema_version": SCHEMA_VERSION,
            "commands": [{"id": "plain", "command": "cat README.md", "expected_verdict": "auto_approved"}]
        })).unwrap();
        let report = replay(corpus, &policy_context(None).unwrap()).unwrap();

        assert_eq!(report.results[0].sandbox_verdict, None);
        assert_eq!(report.results[0].verdict, Verdict::UserApprovalRequired);
        assert_eq!(report.results[0].matches_expectation, Some(false));
        assert_eq!(report.summary.expectation_mismatches, 1);
    }

    #[test]
    fn replay_never_executes_recorded_commands() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("must-not-exist");
        let command = format!("printf replay > '{}'", marker.display());
        let corpus = serde_json::from_value(json!({
            "schema_version": SCHEMA_VERSION,
            "commands": [{"id": "redirect", "command": command}]
        }))
        .unwrap();

        let report = replay(corpus, &policy_context(Some(RULES)).unwrap()).unwrap();

        assert_eq!(report.results[0].verdict, Verdict::UserApprovalRequired);
        assert!(!marker.exists());
    }

    #[test]
    fn invalid_corpora_are_rejected_instead_of_producing_empty_or_ambiguous_reports() {
        let invalid = [
            (
                json!({"schema_version": 2, "commands": [{"id": "case", "command": "cat file"}]}),
                "unsupported corpus schema_version",
            ),
            (
                json!({"schema_version": SCHEMA_VERSION, "commands": []}),
                "at least one command",
            ),
            (
                json!({"schema_version": SCHEMA_VERSION, "commands": [{"id": " ", "command": "cat file"}]}),
                "id must not be blank",
            ),
            (
                json!({"schema_version": SCHEMA_VERSION, "commands": [{"id": "case", "command": "\n "}]}),
                "must not be blank",
            ),
            (
                json!({"schema_version": SCHEMA_VERSION, "commands": [
                    {"id": "duplicate", "command": "cat first"},
                    {"id": "duplicate", "command": "cat second"}
                ]}),
                "duplicate command id",
            ),
        ];
        let context = policy_context(None).unwrap();
        for (value, expected_error) in invalid {
            let corpus = serde_json::from_value(value).unwrap();
            let error = replay(corpus, &context).err().unwrap();
            assert!(error.to_string().contains(expected_error), "{error}");
        }
        let misspelled_expectation = json!({
            "schema_version": SCHEMA_VERSION,
            "commands": [{"id": "case", "command": "cat file", "expected_verdit": "auto_approved"}]
        });
        assert!(serde_json::from_value::<Corpus>(misspelled_expectation).is_err());
    }

    #[test]
    fn rules_files_require_an_explicit_valid_bash_policy() {
        for source in [
            "[tools.read]\nrules = []",
            "[tools.bash]\n",
            "[tools.bash]\nrules = []\nrulse = []",
            r#"[tools.bash]
rules = [{command = ["python3", "**"], only_arguments = [], verdict = "auto_approved"}]"#,
            r#"[tools.bash]
rules = [{rule = "outside_workspace", verdict = "auto_approved"}]"#,
        ] {
            assert!(policy_context(Some(source)).is_err(), "{source}");
        }
    }

    #[test]
    fn run_writes_a_report_preserves_provenance_and_fails_on_unmet_expectations() {
        let directory = tempfile::tempdir().unwrap();
        let corpus_path = directory.path().join("commands.json");
        let rules_path = directory.path().join("rules.toml");
        let corpus = json!({
            "schema_version": SCHEMA_VERSION,
            "description": "extendable corpus",
            "source": {"session_count": 1},
            "commands": [
                {"id": "read", "command": "cat README.md", "expected_verdict": "auto_approved", "source": {"event_line": 42}}
            ]
        });
        std::fs::write(&corpus_path, serde_json::to_vec(&corpus).unwrap()).unwrap();
        std::fs::write(&rules_path, RULES).unwrap();
        let arguments = Arguments {
            corpus: corpus_path,
            rules: Some(rules_path),
        };
        let mut output = Vec::new();

        assert_eq!(run(&arguments, &mut output).unwrap(), ExitCode::SUCCESS);
        let report: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["description"], corpus["description"]);
        assert_eq!(report["source"], corpus["source"]);
        assert_eq!(
            report["results"][0]["source"],
            corpus["commands"][0]["source"]
        );

        let default_arguments = Arguments {
            rules: None,
            ..arguments
        };
        output.clear();
        assert_eq!(
            run(&default_arguments, &mut output).unwrap(),
            ExitCode::FAILURE
        );
        let report: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["summary"]["expectation_mismatches"], 1);
    }
}
