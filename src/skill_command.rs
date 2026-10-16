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

//! Provider-independent skill listing and process discovery reporting.

use std::io::{self, Write};

use crossterm::style::Stylize;

use crate::cli::SkillCommand;
use crate::home_relative_path::display_path;
use crate::llm::token_estimate::format_token_estimate;
use crate::llm::TokenEstimator;
use crate::skills::{terminal_safe, SkillCatalog};

/// Presentation settings, independent of process terminal/environment detection.
#[derive(Debug, Clone, Copy)]
pub struct SkillListOptions {
    /// Emit ANSI bold names and underlined labels when enabled.
    pub color: bool,
    /// Total output columns available for wrapping descriptions.
    pub width: usize,
}

impl Default for SkillListOptions {
    fn default() -> Self {
        Self {
            color: false,
            width: 80,
        }
    }
}

const DESCRIPTION_LABEL: &str = "Description:";
const BODY_TOKENS_LABEL: &str = "Body tokens:";
const SOURCE_LABEL: &str = "Source:";
const SHADOWED_LABEL: &str = "Shadowed:";
const DUPLICATE_LABEL: &str = "Duplicate:";
const FIELD_INDENTATION: &str = "  ";
const FIELD_VALUE_SEPARATOR: &str = " ";

fn label_width() -> usize {
    [
        DESCRIPTION_LABEL,
        BODY_TOKENS_LABEL,
        SOURCE_LABEL,
        SHADOWED_LABEL,
        DUPLICATE_LABEL,
    ]
    .iter()
    .map(|label| label.len())
    .max()
    .expect("skill listing defines field labels")
}

pub fn discover() -> io::Result<SkillCatalog> {
    let launch = std::env::current_dir()?;
    let workspace = crate::discover_workspace_root(&launch);
    SkillCatalog::discover(
        workspace.as_deref(),
        &launch,
        dirs::home_dir().as_deref(),
        crate::ylva_home().as_deref(),
    )
}

pub fn report_diagnostics(catalog: &SkillCatalog, output: &mut dyn Write) -> io::Result<()> {
    for diagnostic in &catalog.diagnostics {
        let path = terminal_safe(&diagnostic.path.to_string_lossy());
        let reason = terminal_safe(&diagnostic.reason);
        tracing::warn!(path, reason, kind = ?diagnostic.kind, "skill discovery diagnostic");
        writeln!(
            output,
            "warning: skill {}: {reason}",
            terminal_safe(&display_path(&diagnostic.path)),
        )?;
    }
    Ok(())
}

pub fn list(catalog: &SkillCatalog, output: &mut dyn Write) -> io::Result<()> {
    list_with_duplicates(catalog, false, output)
}

pub fn list_with_duplicates(
    catalog: &SkillCatalog,
    with_duplicates: bool,
    output: &mut dyn Write,
) -> io::Result<()> {
    list_with_options(
        catalog,
        with_duplicates,
        SkillListOptions::default(),
        output,
    )
}

/// Render a catalog with explicit styling and description-wrap width.
pub fn list_with_options(
    catalog: &SkillCatalog,
    with_duplicates: bool,
    options: SkillListOptions,
    output: &mut dyn Write,
) -> io::Result<()> {
    if catalog.skills.is_empty() {
        return writeln!(output, "No skills discovered.");
    }
    let content_indent = FIELD_INDENTATION.len() + label_width() + FIELD_VALUE_SEPARATOR.len();
    for (index, (name, skill)) in catalog.skills.iter().enumerate() {
        if index > 0 {
            writeln!(output)?;
        }
        let name = terminal_safe(name);
        if options.color {
            writeln!(output, "{}", name.bold())?;
        } else {
            writeln!(output, "{name}")?;
        }
        let description = skill
            .description
            .lines()
            .map(terminal_safe)
            .collect::<Vec<_>>()
            .join("\n");
        let width = options.width.saturating_sub(content_indent).max(1);
        for (line_index, line) in textwrap::wrap(&description, width).iter().enumerate() {
            if line_index == 0 {
                write_field(output, DESCRIPTION_LABEL, line, options)?;
            } else if line.is_empty() {
                writeln!(output)?;
            } else {
                writeln!(output, "{:content_indent$}{line}", "")?;
            }
        }
        write_field(
            output,
            SOURCE_LABEL,
            &terminal_safe(&display_path(&skill.source.canonical_path)),
            options,
        )?;
        write_field(
            output,
            BODY_TOKENS_LABEL,
            &format!(
                "~{}",
                format_token_estimate(TokenEstimator::Tiktoken.estimate_tokens(&skill.body))
            ),
            options,
        )?;
        for source in &skill.shadowed {
            write_field(
                output,
                SHADOWED_LABEL,
                &terminal_safe(&display_path(&source.canonical_path)),
                options,
            )?;
        }
        if with_duplicates {
            for source in &skill.duplicates {
                let path = if source.canonical_path == skill.source.canonical_path {
                    &source.logical_path
                } else {
                    &source.canonical_path
                };
                write_field(
                    output,
                    DUPLICATE_LABEL,
                    &terminal_safe(&display_path(path)),
                    options,
                )?;
            }
        }
    }
    Ok(())
}

fn write_field(
    output: &mut dyn Write,
    label: &str,
    value: &str,
    options: SkillListOptions,
) -> io::Result<()> {
    let label_width = label_width();
    let padding = label_width.saturating_sub(label.len());
    if options.color {
        writeln!(
            output,
            "{FIELD_INDENTATION}{}{:padding$}{FIELD_VALUE_SEPARATOR}{value}",
            label.underlined(),
            ""
        )
    } else {
        writeln!(
            output,
            "{FIELD_INDENTATION}{label:<label_width$}{FIELD_VALUE_SEPARATOR}{value}"
        )
    }
}

pub fn run(
    command: &SkillCommand,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> io::Result<u8> {
    run_with_options(command, SkillListOptions::default(), output, diagnostics)
}

/// Execute listing with presentation settings resolved by the host.
pub fn run_with_options(
    command: &SkillCommand,
    options: SkillListOptions,
    output: &mut dyn Write,
    diagnostics: &mut dyn Write,
) -> io::Result<u8> {
    let catalog = discover()?;
    report_diagnostics(&catalog, diagnostics)?;
    match command {
        SkillCommand::List { with_duplicates } => {
            list_with_options(&catalog, *with_duplicates, options, output)?
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_aligns_values_after_the_longest_label() {
        let labels = [
            DESCRIPTION_LABEL,
            BODY_TOKENS_LABEL,
            SOURCE_LABEL,
            SHADOWED_LABEL,
            DUPLICATE_LABEL,
        ];
        for color in [false, true] {
            for label in labels {
                let mut output = Vec::new();
                write_field(
                    &mut output,
                    label,
                    "value",
                    SkillListOptions {
                        color,
                        ..SkillListOptions::default()
                    },
                )
                .unwrap();
                let output = String::from_utf8(output)
                    .unwrap()
                    .replace("\x1b[4m", "")
                    .replace("\x1b[0m", "");
                let value_column = output.find("value").expect("field value");
                let label_columns =
                    value_column - FIELD_INDENTATION.len() - FIELD_VALUE_SEPARATOR.len();
                assert_eq!(label_columns, label_width());
                assert!(labels.iter().all(|other| label_columns >= other.len()));
                assert!(labels.iter().any(|other| label_columns == other.len()));
            }
        }
    }
}
