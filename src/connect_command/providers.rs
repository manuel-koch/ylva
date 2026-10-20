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

//! Provider-specific [`ConnectFlow`] implementations.

use super::{ConnectFlow, ConnectIo, NewConnection};
use crate::llm::copilot_provider::auth::validate_copilot_github_token;
use crate::llm::copilot_provider::device_flow::authorize_with_copilot_device_flow;

const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";

/// Name of the `.env` variable holding the secret of connection
/// `connection_name`. The default connection (named like the provider kind)
/// uses `prefix` as is; named connections append a sanitized suffix.
fn secret_variable(kind: &str, prefix: &str, connection_name: &str) -> String {
    if connection_name == kind {
        return prefix.to_string();
    }
    let suffix: String = connection_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}_{suffix}")
}

fn api_key_reference(variable: &str) -> toml_edit::Item {
    toml_edit::value(format!("${{{variable}}}"))
}

/// Logs in with the GitHub device flow and stores the long-lived token.
pub(super) struct CopilotConnect;

impl CopilotConnect {
    const KIND: &'static str = "copilot";
    const SECRET_VARIABLE_PREFIX: &'static str = "COPILOT_GITHUB_TOKEN";

    fn secret_variable(connection_name: &str) -> String {
        secret_variable(Self::KIND, Self::SECRET_VARIABLE_PREFIX, connection_name)
    }

    fn connection(connection_name: &str, token: String) -> anyhow::Result<NewConnection> {
        validate_copilot_github_token(&token)?;
        let variable = Self::secret_variable(connection_name);
        let mut settings = toml_edit::Table::new();
        settings["api_key"] = api_key_reference(&variable);
        Ok(NewConnection {
            settings,
            env_entries: vec![(variable, token)],
        })
    }
}

impl ConnectFlow for CopilotConnect {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    // Declared so a clash is reported before the browser login.
    fn env_variables(&self, connection_name: &str) -> Vec<String> {
        vec![Self::secret_variable(connection_name)]
    }

    fn connect(
        &self,
        connection_name: &str,
        io: &mut ConnectIo<'_>,
    ) -> anyhow::Result<NewConnection> {
        let runtime = tokio::runtime::Runtime::new()?;
        let token = runtime.block_on(authorize_with_copilot_device_flow(&mut *io.output))?;
        Self::connection(connection_name, token)
    }
}

/// Asks for the base URL and the API key of an OpenAI-compatible endpoint.
pub(super) struct EndpointConnect {
    kind: &'static str,
    secret_variable_prefix: &'static str,
    default_base_url: &'static str,
    secret_is_optional: bool,
}

impl EndpointConnect {
    pub(super) fn ollama() -> Self {
        Self {
            kind: "ollama",
            secret_variable_prefix: "OLLAMA_API_KEY",
            default_base_url: DEFAULT_OLLAMA_BASE_URL,
            secret_is_optional: true,
        }
    }

    pub(super) fn openrouter() -> Self {
        Self {
            kind: "openrouter",
            secret_variable_prefix: "OPENROUTER_API_KEY",
            default_base_url: crate::llm::provider_registry::DEFAULT_OPENROUTER_BASE_URL,
            secret_is_optional: false,
        }
    }

    fn prompt_endpoint(&self, io: &mut ConnectIo<'_>) -> anyhow::Result<(String, Option<String>)> {
        let default_base_url = self.default_base_url;
        let answer = io
            .prompter
            .ask(&format!("Base URL [{default_base_url}]:"))?;
        let base_url = if answer.is_empty() {
            default_base_url.to_string()
        } else {
            answer
        };
        let parsed = reqwest::Url::parse(&base_url)
            .map_err(|error| anyhow::anyhow!("invalid base URL '{base_url}': {error}"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https") && parsed.has_host(),
            "the base URL must start with http:// or https://"
        );
        let question = if self.secret_is_optional {
            "API key (input hidden, leave empty for none):"
        } else {
            "API key (input hidden):"
        };
        let secret = io.prompter.ask_secret(question)?;
        if secret.is_empty() {
            anyhow::ensure!(self.secret_is_optional, "an API key is required");
            return Ok((base_url, None));
        }
        anyhow::ensure!(
            !secret.chars().any(char::is_whitespace),
            "the API key must not contain whitespace"
        );
        Ok((base_url, Some(secret)))
    }
}

impl ConnectFlow for EndpointConnect {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn connect(
        &self,
        connection_name: &str,
        io: &mut ConnectIo<'_>,
    ) -> anyhow::Result<NewConnection> {
        let (base_url, secret) = self.prompt_endpoint(io)?;
        let mut settings = toml_edit::Table::new();
        settings["api_base_url"] = toml_edit::value(base_url);
        let mut env_entries = Vec::new();
        if let Some(secret) = secret {
            let variable = secret_variable(self.kind, self.secret_variable_prefix, connection_name);
            settings["api_key"] = api_key_reference(&variable);
            env_entries.push((variable, secret));
        }
        Ok(NewConnection {
            settings,
            env_entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect_command::tests::ScriptedPrompter;

    fn connect_endpoint(
        flow: &EndpointConnect,
        connection_name: &str,
        answers: &[&'static str],
    ) -> (anyhow::Result<NewConnection>, ScriptedPrompter) {
        let mut prompter = ScriptedPrompter::new(answers);
        let mut output = Vec::new();
        let result = flow.connect(
            connection_name,
            &mut ConnectIo {
                prompter: &mut prompter,
                output: &mut output,
            },
        );
        (result, prompter)
    }

    #[test]
    fn secret_variable_names_are_sanitized_except_for_the_default() {
        assert_eq!(
            CopilotConnect::secret_variable("copilot"),
            "COPILOT_GITHUB_TOKEN"
        );
        assert_eq!(
            CopilotConnect::secret_variable("work-copilot.2"),
            "COPILOT_GITHUB_TOKEN_WORK_COPILOT_2"
        );
        assert_eq!(
            secret_variable("openrouter", "OPENROUTER_API_KEY", "openrouter"),
            "OPENROUTER_API_KEY"
        );
        assert_eq!(
            secret_variable("ollama", "OLLAMA_API_KEY", "cloud"),
            "OLLAMA_API_KEY_CLOUD"
        );
    }

    #[test]
    fn copilot_connection_references_the_stored_token_and_rejects_pat_tokens() {
        let connection = CopilotConnect::connection("work", "ghu_a".to_string()).expect("ghu");

        assert_eq!(
            connection.env_entries,
            vec![("COPILOT_GITHUB_TOKEN_WORK".to_string(), "ghu_a".to_string())]
        );
        assert_eq!(
            connection.settings["api_key"].as_str(),
            Some("${COPILOT_GITHUB_TOKEN_WORK}")
        );
        assert!(connection.settings.get("api_base_url").is_none());
        assert!(CopilotConnect::connection("copilot", "ghp_x".to_string()).is_err());
    }

    #[test]
    fn endpoint_prompt_uses_the_default_url_and_requires_a_key_only_for_openrouter() {
        let (connection, prompter) = connect_endpoint(
            &EndpointConnect::openrouter(),
            "openrouter",
            &["", "sk-or-1"],
        );
        let connection = connection.expect("openrouter");
        assert_eq!(
            connection.settings["api_base_url"].as_str(),
            Some("https://openrouter.ai")
        );
        assert_eq!(
            connection.env_entries,
            vec![("OPENROUTER_API_KEY".to_string(), "sk-or-1".to_string())]
        );
        assert!(prompter.questions[0].contains("https://openrouter.ai"));

        let (connection, _) = connect_endpoint(
            &EndpointConnect::ollama(),
            "cloud",
            &["http://gpu:11434", ""],
        );
        let connection = connection.expect("ollama");
        assert_eq!(
            connection.settings["api_base_url"].as_str(),
            Some("http://gpu:11434")
        );
        assert!(connection.settings.get("api_key").is_none());
        assert!(connection.env_entries.is_empty());

        let (connection, _) =
            connect_endpoint(&EndpointConnect::openrouter(), "openrouter", &["", ""]);
        assert!(connection.is_err());
    }

    #[test]
    fn endpoint_prompt_stores_an_ollama_key_under_a_named_variable() {
        let (connection, _) = connect_endpoint(
            &EndpointConnect::ollama(),
            "cloud",
            &["http://gpu:11434", "k1"],
        );
        let connection = connection.expect("ollama");

        assert_eq!(
            connection.settings["api_key"].as_str(),
            Some("${OLLAMA_API_KEY_CLOUD}")
        );
        assert_eq!(
            connection.env_entries,
            vec![("OLLAMA_API_KEY_CLOUD".to_string(), "k1".to_string())]
        );
    }

    #[test]
    fn endpoint_prompt_rejects_invalid_urls_and_keys_with_whitespace() {
        for (answers, expected) in [
            (["not a url", "key"], "invalid base URL"),
            (["ftp://host", "key"], "http:// or https://"),
            (["", "two words"], "whitespace"),
        ] {
            let (connection, _) =
                connect_endpoint(&EndpointConnect::openrouter(), "openrouter", &answers);
            let error = connection.err().expect("rejected").to_string();
            assert!(error.contains(expected), "{error}");
        }
    }
}
