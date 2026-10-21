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

use super::{flag, required, ArgumentAnalysis, ArgvToken, Arity, OptionSpec};

// Reviewed 2026-10-21. GNU findutils and BSD/macOS find, selected primaries.
// GNU-only output primaries are intentionally recognized for policy vetoes.
// No short bundles, attached values, or long '=' spellings for primaries.
// https://www.gnu.org/software/findutils/manual/html_node/Run-Commands.html
// https://man.freebsd.org/cgi/man.cgi?query=find&sektion=1
// https://keith.github.io/xcode-man-pages/find.1.html
const GLOBAL_OPTIONS: &[OptionSpec] = &[flag(&["-H"]), flag(&["-L"]), flag(&["-P"])];
const PRIMARIES: &[OptionSpec] = &[
    required(&["-name"]),
    required(&["-iname"]),
    required(&["-path", "-wholename"]),
    required(&["-ipath", "-iwholename"]),
    required(&["-regex"]),
    required(&["-iregex"]),
    required(&["-type"]),
    required(&["-user"]),
    required(&["-group"]),
    required(&["-uid"]),
    required(&["-gid"]),
    required(&["-perm"]),
    required(&["-size"]),
    required(&["-mtime"]),
    required(&["-atime"]),
    required(&["-ctime"]),
    required(&["-mmin"]),
    required(&["-newer"]),
    required(&["-maxdepth"]),
    required(&["-mindepth"]),
    required(&["-fprint"]),
    required(&["-fprint0"]),
    required(&["-printf"]),
    flag(&["-print"]),
    flag(&["-print0"]),
    flag(&["-ls"]),
    flag(&["-delete"]),
    flag(&["-prune"]),
    flag(&["-quit"]),
    flag(&["-empty"]),
    flag(&["-depth"]),
    flag(&["-xdev", "-mount"]),
    flag(&["-true"]),
    flag(&["-false"]),
    flag(&["-follow"]),
    flag(&["-exec"]),
    flag(&["-execdir"]),
    flag(&["-ok"]),
    flag(&["-okdir"]),
    flag(&["-fprintf"]),
];
const AND: &[&str] = &["-a", "-and"];
const OR: &[&str] = &["-o", "-or"];
const NOT: &[&str] = &["!", "-not"];

pub(super) fn parse(arguments: &[ArgvToken], analysis: &mut ArgumentAnalysis) {
    let mut position = 1;
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        let Some(option) = GLOBAL_OPTIONS
            .iter()
            .find(|option| option.aliases.contains(&text))
        else {
            break;
        };
        analysis.identify(option.aliases);
        position += 1;
    }
    if arguments
        .get(position)
        .is_some_and(|token| token.text == "--")
    {
        position += 1;
    }
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        if text.starts_with('-') || matches!(text, "!" | "(" | ")") {
            break;
        }
        analysis.operands.push(position);
        position += 1;
    }
    // GNU permits an implicit '.', whereas BSD requires a path. The common
    // subset requires an explicit path so a missing operand is not guessed.
    if analysis.operands.is_empty() {
        analysis.complete = false;
        return;
    }
    let mut open_parentheses = 0_usize;
    let mut needs_expression = false;
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        match text {
            "(" => {
                open_parentheses += 1;
                needs_expression = true;
                position += 1;
            }
            ")" if open_parentheses > 0 && !needs_expression => {
                open_parentheses -= 1;
                position += 1;
            }
            "!" | "-not" => {
                analysis.identify(NOT);
                needs_expression = true;
                position += 1;
            }
            "-a" | "-and" | "-o" | "-or" if !needs_expression => {
                // An operator needs a left-hand expression, not just a path.
                if analysis.identities.last().is_none_or(|aliases| {
                    GLOBAL_OPTIONS
                        .iter()
                        .any(|option| option.aliases == aliases)
                }) {
                    analysis.complete = false;
                    return;
                }
                analysis.identify(if AND.contains(&text) { AND } else { OR });
                needs_expression = true;
                position += 1;
            }
            _ => {
                let Some(option) = PRIMARIES
                    .iter()
                    .find(|option| option.aliases.contains(&text))
                else {
                    analysis.complete = false;
                    return;
                };
                analysis.identify(option.aliases);
                position += 1;
                let valid = match text {
                    "-exec" | "-execdir" | "-ok" | "-okdir" => {
                        consume_execution(arguments, &mut position, text, analysis)
                    }
                    "-fprintf" => consume_values(arguments, &mut position, 2, analysis),
                    _ if matches!(option.arity, Arity::Required) => {
                        consume_values(arguments, &mut position, 1, analysis)
                    }
                    _ => true,
                };
                if !valid {
                    analysis.complete = false;
                    return;
                }
                needs_expression = false;
            }
        }
    }
    analysis.complete &= open_parentheses == 0 && !needs_expression;
}

fn consume_values(
    arguments: &[ArgvToken],
    position: &mut usize,
    count: usize,
    analysis: &mut ArgumentAnalysis,
) -> bool {
    for _ in 0..count {
        if *position == arguments.len() {
            return false;
        }
        analysis.option_values.push(*position);
        *position += 1;
    }
    true
}

fn consume_execution(
    arguments: &[ArgvToken],
    position: &mut usize,
    primary: &str,
    analysis: &mut ArgumentAnalysis,
) -> bool {
    let start = *position;
    while *position < arguments.len() {
        let text = arguments[*position].text.as_str();
        if text == ";" {
            *position += 1;
            return *position > start + 1;
        }
        // '+' is a terminator only in the documented '{} +' position.
        if text == "+"
            && *position > start
            && arguments[*position - 1].text == "{}"
            && matches!(primary, "-exec" | "-execdir")
        {
            let valid = *position >= start + 2
                && arguments[start..*position - 1]
                    .iter()
                    .all(|token| !token.text.contains("{}"));
            *position += 1;
            return valid;
        }
        analysis.option_values.push(*position);
        *position += 1;
    }
    false
}
