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

//! `sed` scripts: a fixed safety floor that no rule can widen. Commands that
//! write, execute, read files, or nest (`w W e r R s y a i c { }`), complex
//! addresses, `-i` and `-f` make the analysis incomplete. Each remaining
//! command is exposed in `script_commands`; the `allowed_scripts` rule
//! condition decides which of them a rule approves.

use super::{consume_option, flag, required, ArgumentAnalysis, ArgvToken, OptionSpec};

// Reviewed 2026-10-21. Omitted on purpose: -i/--in-place (writes), -f/--file
// (reads a script file), -l, -z, --sandbox, --debug, --follow-symlinks.
// https://www.gnu.org/software/sed/manual/sed.html
// https://man.freebsd.org/cgi/man.cgi?query=sed&sektion=1
const SED: &[OptionSpec] = &[
    flag(&["-n", "--quiet", "--silent"]),
    flag(&["-E", "-r", "--regexp-extended"]),
    flag(&["-s", "--separate"]),
    flag(&["-u", "--unbuffered"]),
    flag(&["--posix"]),
    flag(&["--help"]),
    flag(&["--version"]),
    required(&["-e", "--expression"]),
];

pub(super) fn parse(arguments: &[ArgvToken], analysis: &mut ArgumentAnalysis) {
    let mut position = 1;
    let mut terminated = false;
    let mut expressions: Vec<String> = Vec::new();
    let mut operands: Vec<usize> = Vec::new();
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        if !terminated && text == "--" {
            terminated = true;
            position += 1;
        } else if !terminated && text.starts_with('-') && text != "-" {
            // Options after the first operand are not guessed (BSD getopt).
            let start = position;
            if !operands.is_empty() || !consume_option(arguments, &mut position, SED, analysis) {
                analysis.complete = false;
                return;
            }
            let is_expression = analysis
                .identities
                .last()
                .is_some_and(|aliases| aliases.contains(&"-e"));
            if is_expression {
                let value_position = *analysis.option_values.last().expect("-e records a value");
                expressions.push(expression_value(arguments, start, value_position));
            }
        } else {
            operands.push(position);
            position += 1;
        }
    }
    let standalone = analysis
        .identities
        .iter()
        .any(|aliases| aliases.contains(&"--help") || aliases.contains(&"--version"));
    let script_operand = usize::from(expressions.is_empty());
    if operands.len() < script_operand {
        analysis.complete &= standalone;
        return;
    }
    let scripts: Vec<&str> = if expressions.is_empty() {
        vec![arguments[operands[0]].text.as_str()]
    } else {
        expressions.iter().map(String::as_str).collect()
    };
    for script in scripts {
        match split_script_commands(script) {
            Some(commands) => analysis.script_commands.extend(commands),
            None => analysis.complete = false,
        }
    }
    analysis.operands = operands;
}

/// Script text of an `-e` option placed at `start` whose value is at `value_position`.
fn expression_value(arguments: &[ArgvToken], start: usize, value_position: usize) -> String {
    let text = arguments[start].text.as_str();
    if value_position != start {
        return arguments[value_position].text.clone();
    }
    if text.starts_with("--") {
        return text
            .split_once('=')
            .map_or("", |(_, value)| value)
            .to_owned();
    }
    // Short bundle such as `-ne1p`: the value follows the first `e`.
    text.split_once('e')
        .map_or("", |(_, value)| value)
        .to_owned()
}

/// Split a script into trimmed commands, or `None` if any command is outside
/// the floor: `[addr[,addr]][!]cmd` with simple addresses and `cmd` in
/// `p d q Q = n N x g G h H D P l z F` (`q`, `Q`, `l` take an optional number).
fn split_script_commands(script: &str) -> Option<Vec<String>> {
    if script.trim().is_empty() {
        return None;
    }
    script
        .split(['\n', ';'])
        .map(|command| {
            let command = command.trim();
            is_floor_command(command).then(|| command.to_owned())
        })
        .collect()
}

fn is_floor_command(command: &str) -> bool {
    let mut rest = command;
    if let Some(after_first) = strip_address(rest) {
        rest = after_first.trim_start();
        if let Some(after_comma) = rest.strip_prefix(',') {
            match strip_address(after_comma.trim_start()) {
                Some(after_second) => rest = after_second.trim_start(),
                None => return false,
            }
        }
        if let Some(after_negation) = rest.strip_prefix('!') {
            rest = after_negation.trim_start();
        }
    }
    let mut characters = rest.chars();
    match characters.next() {
        Some('q' | 'Q' | 'l') => characters.all(|character| character.is_ascii_digit()),
        Some('p' | 'd' | '=' | 'n' | 'N' | 'x' | 'g' | 'G' | 'h' | 'H' | 'D' | 'P' | 'z' | 'F') => {
            characters.next().is_none()
        }
        _ => false,
    }
}

/// Strip one address (`N`, `$`, `first~step`, `/simple-regex/`) from the front.
fn strip_address(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix('$') {
        return Some(rest);
    }
    if let Some(regex_and_rest) = text.strip_prefix('/') {
        let (regex, rest) = regex_and_rest.split_once('/')?;
        let simple = !regex.is_empty() && !regex.contains(['\\', '[', '\n']);
        return simple.then_some(rest);
    }
    let rest = strip_digits(text)?;
    match rest.strip_prefix('~') {
        Some(step) => strip_digits(step),
        None => Some(rest),
    }
}

fn strip_digits(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches(|character: char| character.is_ascii_digit());
    (rest.len() < text.len()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::split_script_commands;

    #[test]
    fn accepted_scripts() {
        for script in [
            "20,150p",
            "$p",
            "1p",
            "2~3p",
            "/foo/p",
            "/foo/,/bar/p",
            "1,5!d",
            " 1p ; 3q ",
            "1p\n2p",
            "=",
            "$!d",
            "5q",
            "1, 3p",
            "/a b/p",
        ] {
            assert!(split_script_commands(script).is_some(), "{script:?}");
        }
    }

    #[test]
    fn rejected_scripts() {
        for script in [
            "",
            " ",
            ";",
            "1p;",
            "w out",
            "1w out",
            "1p;w out",
            "1p\nw out",
            "W out",
            "e",
            "1e id",
            "r file",
            "R file",
            "s/a/b/",
            "s/a/b/e",
            "{p}",
            "1{p}",
            "y/a/b/",
            "a text",
            "i text",
            "c text",
            "garbage",
            "p p",
            "1,p",
            ",3p",
            "//p",
            "/a\\/b/p",
            "/[a]/p",
            "/a/Ip",
            "0x1p",
            "1~p",
            "~2p",
            "/foo",
            "1pw out",
            "q5x",
            "p5",
        ] {
            assert!(!split_script_commands(script).is_some(), "{script:?}");
        }
    }
}

#[test]
fn commands_are_split_and_trimmed() {
    assert_eq!(
        split_script_commands(" 1,5p ; 9q\n$p"),
        Some(vec!["1,5p".into(), "9q".into(), "$p".into()])
    );
}
