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

//! Parsing for user-authored agent-type definition files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::profile::ToolSandboxConfig;
use super::{AgentConfig, ConfigError};

/// The parsed configuration supplied by one agent-type file.
#[derive(Debug, Clone)]
pub struct AgentTypeDefinition {
    /// Agent settings deserialized from the file.
    pub config: AgentConfig,
}

/// Parse one agent-type file from disk.
pub fn parse_agent_type_file(path: &Path) -> Result<AgentTypeDefinition, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    parse_agent_type_contents(path, &contents)
}

/// Parse one agent-type file's contents according to its extension.
pub fn parse_agent_type_contents(
    path: &Path,
    contents: &str,
) -> Result<AgentTypeDefinition, ConfigError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("toml") => parse_toml(path, contents),
        Some("md") => parse_markdown(path, contents),
        Some(extension) => Err(malformed(
            path,
            format!("unsupported agent-type file extension '.{extension}'"),
        )),
        None => Err(malformed(path, "agent-type file has no extension")),
    }
}

/// Load all agent-type files from one scope directory.
pub(crate) fn discover_agent_type_files(
    directory: &Path,
    scope: AgentTypeScope,
) -> Result<HashMap<String, AgentTypeFile>, ConfigError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HashMap::new());
        }
        Err(source) => {
            return Err(ConfigError::Unreadable {
                path: directory.to_path_buf(),
                source,
            });
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigError::Unreadable {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let extension = path.extension().and_then(|extension| extension.to_str());
        if matches!(extension, Some("md" | "toml")) {
            paths.push(path);
        }
    }
    paths.sort();

    let mut files = HashMap::<String, AgentTypeFile>::new();
    for path in paths {
        let agent_type = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| {
                malformed(
                    &path,
                    "agent-type file name must contain a valid UTF-8 stem",
                )
            })?
            .to_string();
        if let Some(previous) = files.get(&agent_type) {
            return Err(ConfigError::ConflictingAgentTypeFiles {
                agent_type,
                scope: scope.label().to_string(),
                first_path: previous.path.clone(),
                second_path: path,
            });
        }
        let definition = parse_agent_type_file(&path)?;
        files.insert(
            agent_type.clone(),
            AgentTypeFile {
                agent_type,
                definition,
                path,
                scope,
            },
        );
    }
    Ok(files)
}

fn parse_toml(path: &Path, contents: &str) -> Result<AgentTypeDefinition, ConfigError> {
    let config = toml::from_str::<AgentConfig>(contents)
        .map_err(|source| malformed(path, format!("invalid TOML: {source}")))?;
    Ok(AgentTypeDefinition { config })
}

fn parse_markdown(path: &Path, contents: &str) -> Result<AgentTypeDefinition, ConfigError> {
    let mut lines = contents.split_inclusive('\n');
    let opening_line = lines.next().map(trim_line_ending).unwrap_or_default();
    let format = match opening_line {
        "+++" => AgentFrontmatterFormat::Toml,
        "---" => AgentFrontmatterFormat::Yaml,
        _ => {
            return Err(malformed(
                path,
                "Markdown agent-type files must start with either a '+++' TOML or '---' YAML frontmatter line",
            ));
        }
    };
    let delimiter = format.delimiter();

    let mut frontmatter = String::new();
    let mut body = None;
    for line in lines.by_ref() {
        if trim_line_ending(line) == delimiter {
            body = Some(lines.collect::<String>());
            break;
        }
        frontmatter.push_str(line);
    }

    let Some(body) = body else {
        return Err(malformed(
            path,
            format!("Markdown agent-type files must close their '{delimiter}' frontmatter block"),
        ));
    };

    let mut config = match format {
        AgentFrontmatterFormat::Toml => toml::from_str::<AgentConfig>(&frontmatter)
            .map_err(|source| malformed(path, format!("invalid frontmatter TOML: {source}")))?,
        AgentFrontmatterFormat::Yaml => serde_yaml::from_str::<AgentConfig>(&frontmatter)
            .map_err(|source| malformed(path, format!("invalid frontmatter YAML: {source}")))?,
    };
    // The body *is* the persona: it overrides whatever `persona` value the
    // frontmatter carried (both deserialize through the same standard
    // `AgentConfig`), so a Markdown file has exactly one prompt source. A
    // body-less Markdown file has nothing to say — its settings belong in a
    // `.toml` file.
    let persona = body.trim();
    if persona.is_empty() {
        return Err(malformed(
            path,
            "Markdown agent-type file has no body; the body is the agent persona (use a `.toml` \
             file for settings-only definitions)",
        ));
    }
    config.persona = Some(persona.to_string());
    Ok(AgentTypeDefinition { config })
}

#[derive(Debug, Clone, Copy)]
enum AgentFrontmatterFormat {
    Toml,
    Yaml,
}

impl AgentFrontmatterFormat {
    const fn delimiter(self) -> &'static str {
        match self {
            Self::Toml => "+++",
            Self::Yaml => "---",
        }
    }
}

fn trim_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

fn malformed(path: &Path, message: impl Into<String>) -> ConfigError {
    ConfigError::MalformedAgentType {
        path: path.to_path_buf(),
        message: message.into(),
    }
}

/// A parsed file together with its path and source scope.
#[derive(Debug, Clone)]
pub(crate) struct AgentTypeFile {
    pub agent_type: String,
    pub definition: AgentTypeDefinition,
    pub path: PathBuf,
    pub scope: AgentTypeScope,
}

/// The file source's precedence tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTypeScope {
    Global,
    Profile,
    Project,
}

impl AgentTypeScope {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Profile => "profile",
            Self::Project => "project",
        }
    }
}

/// Partial values accepted by an inline `[agents.<type>]` table.
///
/// Inline profile tables are field-level overrides on top of a file-sourced
/// definition, so every field must retain the distinction between omitted and
/// explicitly configured.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AgentConfigOverrides {
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_or_string")]
    pub timeout_secs: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_usize_or_string")]
    pub max_tool_iterations: Option<usize>,
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool_or_string")]
    pub can_spawn: Option<bool>,
    #[serde(default)]
    pub tools: Option<HashMap<String, ToolSandboxConfig>>,
}

fn deserialize_optional_u64_or_string<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum TimeoutValue {
        Number(u64),
        Text(String),
    }

    match Option::<TimeoutValue>::deserialize(deserializer)? {
        Some(TimeoutValue::Number(value)) => Ok(Some(value)),
        Some(TimeoutValue::Text(value)) => value.trim().parse::<u64>().map(Some).map_err(|error| {
            serde::de::Error::custom(format!("invalid timeout_secs '{value}': {error}"))
        }),
        None => Ok(None),
    }
}

fn deserialize_optional_usize_or_string<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum IterationLimitValue {
        Number(usize),
        Text(String),
    }

    match Option::<IterationLimitValue>::deserialize(deserializer)? {
        Some(IterationLimitValue::Number(value)) => Ok(Some(value)),
        Some(IterationLimitValue::Text(value)) => {
            value.trim().parse::<usize>().map(Some).map_err(|error| {
                serde::de::Error::custom(format!("invalid max_tool_iterations '{value}': {error}"))
            })
        }
        None => Ok(None),
    }
}

fn deserialize_optional_bool_or_string<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BooleanValue {
        Native(bool),
        Text(String),
    }

    match Option::<BooleanValue>::deserialize(deserializer)? {
        Some(BooleanValue::Native(value)) => Ok(Some(value)),
        Some(BooleanValue::Text(value)) => {
            value.trim().parse::<bool>().map(Some).map_err(|error| {
                serde::de::Error::custom(format!("invalid boolean value '{value}': {error}"))
            })
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_toml_agent_type_files_parse_settings_and_persona() {
        let definition = parse_agent_type_contents(
            Path::new("research.toml"),
            r#"
allowed_tools = ["read", "search"]
timeout_secs = "120"
can_spawn = false
persona = "Investigate the evidence."
"#,
        )
        .expect("pure TOML parses");

        assert_eq!(
            definition.config.allowed_tools,
            Some(vec!["read".to_string(), "search".to_string()])
        );
        assert_eq!(definition.config.timeout_secs, Some(120));
        assert!(!definition.config.can_spawn);
        assert_eq!(
            definition.config.persona.as_deref(),
            Some("Investigate the evidence.")
        );
    }

    #[test]
    fn markdown_agent_type_files_parse_frontmatter_and_trimmed_body() {
        let definition = parse_agent_type_contents(
            Path::new("research.md"),
            "+++\nallowed_tools = [\"read\"]\ncan_spawn = false\n+++\n\nInvestigate the evidence.\n\n",
        )
        .expect("Markdown frontmatter parses");

        assert_eq!(
            definition.config.allowed_tools,
            Some(vec!["read".to_string()])
        );
        assert!(!definition.config.can_spawn);
        assert_eq!(
            definition.config.persona.as_deref(),
            Some("Investigate the evidence.")
        );
    }

    #[test]
    fn markdown_agent_type_files_parse_yaml_frontmatter_and_trimmed_body() {
        let definition = parse_agent_type_contents(
            Path::new("research.md"),
            "---\nallowed_tools:\n  - read\ntimeout_secs: 120\ncan_spawn: false\n---\n\nInvestigate the evidence.\n\n",
        )
        .expect("YAML frontmatter parses");

        assert_eq!(
            definition.config.allowed_tools,
            Some(vec!["read".to_string()])
        );
        assert_eq!(definition.config.timeout_secs, Some(120));
        assert!(!definition.config.can_spawn);
        assert_eq!(
            definition.config.persona.as_deref(),
            Some("Investigate the evidence.")
        );
    }

    #[test]
    fn agent_type_files_parse_max_tool_iterations_in_every_format() {
        let pure_toml =
            parse_agent_type_contents(Path::new("research.toml"), "max_tool_iterations = 7\n")
                .expect("pure TOML parses");
        assert_eq!(pure_toml.config.max_tool_iterations, Some(7));

        let toml_frontmatter = parse_agent_type_contents(
            Path::new("research.md"),
            "+++\nmax_tool_iterations = \"8\"\n+++\nInvestigate the evidence.\n",
        )
        .expect("TOML frontmatter parses");
        assert_eq!(toml_frontmatter.config.max_tool_iterations, Some(8));

        let yaml_frontmatter = parse_agent_type_contents(
            Path::new("research.md"),
            "---\nmax_tool_iterations: 9\n---\n\nInvestigate the evidence.\n\n",
        )
        .expect("YAML frontmatter parses");
        assert_eq!(yaml_frontmatter.config.max_tool_iterations, Some(9));

        let absent = parse_agent_type_contents(Path::new("research.toml"), "can_spawn = true\n")
            .expect("pure TOML parses");
        assert_eq!(absent.config.max_tool_iterations, None);
    }

    #[test]
    fn a_markdown_body_overrides_a_frontmatter_persona() {
        let definition = parse_agent_type_contents(
            Path::new("research.md"),
            "+++\npersona = \"frontmatter persona\"\n+++\nBody persona.\n",
        )
        .expect("Markdown file parses");

        assert_eq!(
            definition.config.persona.as_deref(),
            Some("Body persona."),
            "the body is the persona; a frontmatter `persona` never wins"
        );
    }

    #[test]
    fn a_bodyless_markdown_agent_type_file_is_rejected() {
        let error = parse_agent_type_contents(
            Path::new("research.md"),
            "+++\nallowed_tools = [\"read\"]\npersona = \"frontmatter persona\"\n+++\n",
        )
        .expect_err("a Markdown agent-type file must have a body");

        match error {
            ConfigError::MalformedAgentType { path, message } => {
                assert_eq!(path, Path::new("research.md"));
                assert!(
                    message.contains("no body"),
                    "the error names the missing body: {message}"
                );
            }
            other => panic!("expected a malformed agent-type error, got {other}"),
        }
    }

    #[test]
    fn a_whitespace_only_markdown_body_is_rejected() {
        let error = parse_agent_type_contents(Path::new("research.md"), "+++\n+++\n   \n\t\n")
            .expect_err("a whitespace-only body counts as no body");

        assert!(
            matches!(error, ConfigError::MalformedAgentType { .. }),
            "expected a malformed agent-type error, got {error}"
        );
    }

    #[test]
    fn pure_toml_agent_type_files_parse_tools_sandbox_rules() {
        let definition = parse_agent_type_contents(
            Path::new("research.toml"),
            r#"
allowed_tools = ["read", "write"]

[tools.write]
rules = [
  { rule = "outside_workspace", verdict = "auto_denied" },
  { pattern = "**/build/**", verdict = "auto_approved" },
]
"#,
        )
        .expect("pure TOML with tools table parses");

        let write_rules = &definition
            .config
            .tools
            .get("write")
            .expect("write tool entry present")
            .rules;
        assert_eq!(write_rules.len(), 2);
    }

    #[test]
    fn toml_frontmatter_agent_type_files_parse_tools_sandbox_rules() {
        let definition = parse_agent_type_contents(
            Path::new("research.md"),
            "+++\nallowed_tools = [\"write\"]\n\n[tools.write]\nrules = [{ rule = \"outside_workspace\", verdict = \"auto_denied\" }]\n+++\n\nInvestigate the evidence.\n",
        )
        .expect("TOML frontmatter with tools table parses");

        let write_rules = &definition
            .config
            .tools
            .get("write")
            .expect("write tool entry present")
            .rules;
        assert_eq!(write_rules.len(), 1);
    }

    #[test]
    fn yaml_frontmatter_agent_type_files_parse_tools_sandbox_rules() {
        let definition = parse_agent_type_contents(
            Path::new("research.md"),
            "---\nallowed_tools:\n  - write\ntools:\n  write:\n    rules:\n      - rule: outside_workspace\n        verdict: auto_denied\n---\n\nInvestigate the evidence.\n",
        )
        .expect("YAML frontmatter with tools table parses");

        let write_rules = &definition
            .config
            .tools
            .get("write")
            .expect("write tool entry present")
            .rules;
        assert_eq!(write_rules.len(), 1);
    }

    #[test]
    fn markdown_without_leading_frontmatter_is_a_named_parse_error() {
        let error =
            parse_agent_type_contents(Path::new("/tmp/research.md"), "Investigate the evidence.")
                .expect_err("plain Markdown must not be accepted");

        let message = error.to_string();
        assert!(message.contains("/tmp/research.md"), "{message}");
        assert!(message.contains("+++"), "{message}");
        assert!(message.contains("---"), "{message}");
    }

    #[test]
    fn markdown_without_a_closing_frontmatter_delimiter_is_a_parse_error() {
        let error = parse_agent_type_contents(Path::new("research.md"), "+++\ncan_spawn = false\n")
            .expect_err("an unclosed frontmatter block must fail");

        assert!(error.to_string().contains("close"), "{error}");
    }

    #[test]
    fn unsupported_and_extensionless_agent_type_files_are_parse_errors() {
        let unsupported = parse_agent_type_contents(Path::new("research.txt"), "body")
            .expect_err("unsupported extensions fail");
        assert!(
            unsupported.to_string().contains("unsupported"),
            "{unsupported}"
        );

        let extensionless = parse_agent_type_contents(Path::new("research"), "body")
            .expect_err("extensionless files fail");
        assert!(
            extensionless.to_string().contains("no extension"),
            "{extensionless}"
        );
    }

    #[test]
    fn reading_a_missing_agent_type_file_is_an_error() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        let missing_path = temporary_directory.path().join("agent-type.toml");
        let error = parse_agent_type_file(&missing_path).expect_err("missing files fail");
        assert!(matches!(error, ConfigError::Unreadable { .. }), "{error:?}");
    }

    #[test]
    fn duplicate_agent_type_file_extensions_are_rejected_with_scope_and_paths() {
        let temporary_directory = tempfile::tempdir().expect("temporary directory");
        std::fs::write(
            temporary_directory.path().join("research.md"),
            "+++\n+++\nMarkdown body\n",
        )
        .expect("write Markdown definition");
        std::fs::write(
            temporary_directory.path().join("research.toml"),
            "persona = \"TOML persona\"\n",
        )
        .expect("write TOML definition");

        let error = discover_agent_type_files(temporary_directory.path(), AgentTypeScope::Global)
            .expect_err("duplicate definitions fail");
        let message = error.to_string();
        assert!(message.contains("research"), "{message}");
        assert!(message.contains("global"), "{message}");
        assert!(message.contains("research.md"), "{message}");
        assert!(message.contains("research.toml"), "{message}");
    }
}
