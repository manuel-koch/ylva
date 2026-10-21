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

//! Deliberately partial, static argv grammars. Recognition is not approval.
//! Unknown arity ends scanning, retaining only evidence already placed.
//! No executable lookup, environment inspection, or command execution occurs.

use super::bash_command_extractor::{ArgvToken, ExtractedCommand};

mod find;
mod git;
mod utility_options;

#[cfg(test)]
mod tests;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ArgumentAnalysis {
    pub(super) identities: Vec<Vec<&'static str>>,
    pub(super) complete: bool,
    operands: Vec<usize>,
    option_values: Vec<usize>,
}

impl ArgumentAnalysis {
    fn new() -> Self {
        Self {
            identities: Vec::new(),
            complete: true,
            operands: Vec::new(),
            option_values: Vec::new(),
        }
    }

    fn identify(&mut self, aliases: &'static [&'static str]) {
        self.identities.push(aliases.to_vec());
    }
}

/// Whether an option takes a value and how that value may be supplied.
#[derive(Clone, Copy)]
enum Arity {
    /// Takes no value; any following argv token is parsed independently.
    Flag,
    /// Requires a value, attached to the option or in the next argv token.
    Required,
    /// A required long value whose documented spelling requires `=`.
    RequiredEquals,
    /// Optional values must be attached, never the next argv token.
    OptionalAttached,
}

struct OptionSpec {
    aliases: &'static [&'static str],
    arity: Arity,
}

const fn flag(aliases: &'static [&'static str]) -> OptionSpec {
    OptionSpec {
        aliases,
        arity: Arity::Flag,
    }
}

const fn required(aliases: &'static [&'static str]) -> OptionSpec {
    OptionSpec {
        aliases,
        arity: Arity::Required,
    }
}

const fn equals(aliases: &'static [&'static str]) -> OptionSpec {
    OptionSpec {
        aliases,
        arity: Arity::RequiredEquals,
    }
}

const fn optional(aliases: &'static [&'static str]) -> OptionSpec {
    OptionSpec {
        aliases,
        arity: Arity::OptionalAttached,
    }
}

pub(super) fn supports_command(name: &str) -> bool {
    matches!(
        name,
        "cat" | "ls" | "head" | "tail" | "wc" | "grep" | "rg" | "sort" | "find" | "git"
    )
}

pub(super) fn parse(command: &ExtractedCommand) -> ArgumentAnalysis {
    let mut analysis = ArgumentAnalysis::new();
    let Some(executable) = command.argv.first() else {
        analysis.complete = false;
        return analysis;
    };
    match executable.text.as_str() {
        "find" => find::parse(&command.argv, &mut analysis),
        "git" => git::parse(&command.argv, &mut analysis),
        name => match utility_options::options(name) {
            Some(options) => parse_options(name, &command.argv, options, &mut analysis),
            None => analysis.complete = false,
        },
    }
    analysis
}

fn is_numeric_count_option(text: &str) -> bool {
    text.strip_prefix('-')
        .is_some_and(|count| !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()))
}

fn parse_options(
    name: &str,
    arguments: &[ArgvToken],
    options: &'static [OptionSpec],
    analysis: &mut ArgumentAnalysis,
) {
    let mut position = 1;
    let mut terminated = false;
    let mut saw_operand = false;
    let mut supplied_pattern = false;
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        if !terminated && text == "--" {
            terminated = true;
            position += 1;
        } else if position == 1 && matches!(name, "head" | "tail") && is_numeric_count_option(text)
        {
            // GNU legacy forms are first-argument-only; suffixes stay unsupported.
            analysis.identify(&["-n", "--lines"]);
            analysis.option_values.push(position);
            position += 1;
        } else if !terminated && text.starts_with('-') && text != "-" {
            // BSD utilities can stop getopt at the first operand, unlike GNU
            // permutation. Do not guess whether a later token is an option.
            if (saw_operand && name != "rg")
                || !consume_option(arguments, &mut position, options, analysis)
            {
                analysis.complete = false;
                break;
            }
            supplied_pattern |= analysis
                .identities
                .last()
                .is_some_and(|aliases| aliases.contains(&"-e") || aliases.contains(&"-f"));
        } else {
            analysis.operands.push(position);
            saw_operand = true;
            supplied_pattern = true;
            position += 1;
        }
    }
    if matches!(name, "grep" | "rg") && !supplied_pattern {
        // These independent modes do not require a search pattern.
        let standalone = analysis.identities.iter().any(|aliases| {
            aliases.contains(&"--help")
                || aliases.contains(&"--version")
                || (name == "rg"
                    && (aliases.contains(&"--files") || aliases.contains(&"--type-list")))
        });
        analysis.complete &= standalone;
    }
}

struct PlacedOption {
    specification: &'static OptionSpec,
    attached_value: bool,
}

/// Validate a whole bundle before emitting anything: an unknown suffix can
/// otherwise make an apparent earlier flag an argument of another spelling.
fn place_option(text: &str, options: &'static [OptionSpec]) -> Option<Vec<PlacedOption>> {
    let (spelling, long_value) = match text.split_once('=') {
        Some((spelling, value)) if text.starts_with("--") => (spelling, Some(value)),
        _ => (text, None),
    };
    if let Some(specification) = options
        .iter()
        .find(|option| option.aliases.contains(&spelling))
    {
        if long_value.is_some() && matches!(specification.arity, Arity::Flag) {
            return None;
        }
        return Some(vec![PlacedOption {
            specification,
            attached_value: long_value.is_some(),
        }]);
    }
    if !text.starts_with('-') || text.starts_with("--") || text.len() < 3 {
        return None;
    }
    let mut placed = Vec::new();
    for (offset, character) in text[1..].char_indices() {
        let specification = options.iter().find(|option| {
            option.aliases.iter().any(|alias| {
                let mut characters = alias.chars();
                characters.next() == Some('-')
                    && characters.next() == Some(character)
                    && characters.next().is_none()
            })
        })?;
        let remainder = &text[1 + offset + character.len_utf8()..];
        let takes_value = !matches!(specification.arity, Arity::Flag);
        placed.push(PlacedOption {
            specification,
            attached_value: takes_value && !remainder.is_empty(),
        });
        if takes_value {
            break;
        }
    }
    Some(placed)
}

fn consume_option(
    arguments: &[ArgvToken],
    position: &mut usize,
    options: &'static [OptionSpec],
    analysis: &mut ArgumentAnalysis,
) -> bool {
    let Some(placed) = place_option(&arguments[*position].text, options) else {
        return false;
    };
    for option in placed {
        analysis.identify(option.specification.aliases);
        if option.attached_value {
            analysis.option_values.push(*position);
        } else {
            match option.specification.arity {
                Arity::Required => {
                    *position += 1;
                    if *position == arguments.len() {
                        analysis.complete = false;
                        return false;
                    }
                    analysis.option_values.push(*position);
                }
                Arity::RequiredEquals => {
                    analysis.complete = false;
                    return false;
                }
                Arity::Flag | Arity::OptionalAttached => {}
            }
        }
    }
    *position += 1;
    true
}
