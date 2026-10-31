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

//! Static argv-shape extraction for a `bash` tool call's `command` string,
//! used by [`super::SandboxContext::check_command`] to evaluate
//! [`super::SandboxRuleConfig::CommandGlob`]/named command rules without
//! executing anything.
//!
//! **Scanning scope is fixed, not organically expanded**: `list`, `pipeline`,
//! `command` and redirected statements with only the closed harmless redirect
//! set below are walked. Other constructs (subshells, unsafe redirections,
//! command/parameter substitution, control-flow constructs, function
//! definitions, heredocs) make the enclosing statement
//! [`ExtractedStatement::Unanalysable`], never partially supported.
//!
//! # Grammar facts (empirically confirmed against `tree-sitter-bash` v0.25.1)
//!
//! - There is no `simple_command` node type; the real node name is
//!   `command`, with a `name` field (a `command_name` node wrapping a single
//!   `word`) and zero or more `argument`-field children
//!   (`word`/`number`/`string`/`raw_string`/`concatenation` — a purely
//!   numeric-looking argument, e.g. the `10` in `sleep 10`, parses as its
//!   own `number` node rather than `word`, but is otherwise an equivalent
//!   unquoted literal), plus zero or more leading `variable_assignment`
//!   children (unnamed field, ordered before `name`).
//! - `list` nodes are produced only for `&&`/`||`; their exactly two named
//!   children are the left/right operands (each a `command`/`list`/
//!   `pipeline`/other statement node) — the operator token itself is
//!   anonymous and excluded by `named_child`/`named_children`.
//! - `pipeline` nodes represent `|`-chains; their named children are the
//!   piped `command` nodes (or nested constructs), the `|` tokens are
//!   anonymous.
//! - `;`, `&` and newlines are lexical terminators between sibling
//!   top-level statements: they are **not** wrapped in any node that groups
//!   their neighbours — a `;`/`&`/newline-separated pair of statements are
//!   direct siblings of their enclosing node (typically `program`).
//!   Newlines produce no node at all; `;` and `&` each surface as their own
//!   anonymous single-character node (kind `";"` / `"&"`) between the
//!   sibling statement nodes — this is how a trailing/inline `&`
//!   (backgrounding) is detected: an `&` node immediately following a
//!   statement in sibling order.
//! - Double-quoted string content is a `string_content` node inside a
//!   `string` node; single-quoted text is a `raw_string` node with no
//!   further structure (fully literal, no interpolation ever). A
//!   doubly-quoted string containing an expansion (`"$FOO"`, `"$(cmd)"`)
//!   nests a `simple_expansion`/`command_substitution` node inside the
//!   `string` node instead of a bare `string_content` child — this makes
//!   the containing command un-analysable (the value is dynamic).
//! - An unquoted brace expansion (`{a,b}`) parses as a `concatenation` node
//!   whose children are plain `word` nodes for the `{`, `a,b`, `}` pieces;
//!   a quoted brace expansion (`"{a,b}"`) instead parses as an ordinary
//!   `string` node (no `concatenation`) — so a `concatenation` composed
//!   entirely of `word` children is unambiguous evidence of an unquoted
//!   construct.
//! - `tree.root_node().has_error()` reliably flags a script-wide parse
//!   failure (e.g. a dangling trailing `|`).
//! - A redirect wraps the command in a `redirected_statement`, with the
//!   command under field `body` and one or more `file_redirect` children under
//!   field `redirect`. A `file_redirect` has an optional `descriptor` field
//!   (`file_descriptor`), an anonymous operator token (`>`, `>>`, `<`, `>&`,
//!   `<&`, `&>`, `&>>`, `>&-` or `<&-`), and an optional `destination` field
//!   (`word` or `number`). Only fd duplication/closing with literal digits or
//!   `-`, and the operators `>`, `>>`, `&>`, `&>>`, `<` with the exact bare
//!   word destination `/dev/null`, are ignored for extraction. Quoted strings,
//!   file destinations and all other redirect nodes remain unanalysable.
//!   `<<<` is a `herestring_redirect`, and heredocs are distinct redirect
//!   nodes; neither is included in the harmless set.

use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::{Node, Parser};

/// A single extracted argv token (either the command name or one argument).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArgvToken {
    /// The token's literal (dequoted) text.
    pub text: String,
    /// Whether the token was entirely quoted (`"..."`/`'...'`) in source.
    pub quoted: bool,
    /// The token as the shell sees it before quote removal, with every quoted
    /// piece replaced by a neutral placeholder. Glob and brace-expansion
    /// checks inspect only this, so quoted pieces of a partly quoted token
    /// (`--format='%h*'`) are never mistaken for expansions while bare pieces
    /// still are (`a*'b'`).
    pub unquoted_text: String,
}

/// Stands in for a quoted piece in [`ArgvToken::unquoted_text`].
const QUOTED_PIECE_PLACEHOLDER: char = 'q';

/// One statically-extracted `command` node, ready for rule evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ExtractedCommand {
    /// Argv tokens, index 0 is the command name.
    pub argv: Vec<ArgvToken>,
    /// Whether this command has one or more leading `variable_assignment`
    /// children (e.g. `FOO=bar git status`).
    pub has_variable_assignment_prefix: bool,
    /// Whether this command is immediately followed by a bare `&`
    /// (backgrounding), per the sibling-token scan in
    /// [`collect_top_level_statements`].
    pub backgrounded: bool,
}

/// One top-level statement extracted from a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExtractedStatement {
    /// A fully-analysable `command` node.
    Command(ExtractedCommand),
    /// A statement containing a node type outside the fixed scanning scope
    /// (subshell, non-harmless redirection, substitution, control flow,
    /// function definition, heredoc, or any other unsupported construct).
    Unanalysable,
}

/// The result of extracting a whole `command` script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExtractionResult {
    /// The script did not parse at all (e.g. a syntax error); no per-command
    /// verdicts can be produced.
    ParseFailed,
    /// One entry per top-level statement, in source order.
    Statements(Vec<ExtractedStatement>),
}

/// Parse `source` as a bash script and extract its top-level statements.
pub(super) fn extract(source: &str) -> ExtractionResult {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("tree-sitter-bash grammar loads");
    let Some(tree) = parser.parse(source, None) else {
        return ExtractionResult::ParseFailed;
    };
    let root = tree.root_node();
    if root.has_error() {
        return ExtractionResult::ParseFailed;
    }
    ExtractionResult::Statements(collect_top_level_statements(root, source))
}

fn node_text<'a>(node: Node, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

/// Walk `root`'s direct children in source order, splitting on `;`/`&`
/// (anonymous terminator nodes) to discover sibling top-level statements,
/// and marking a statement `backgrounded` when immediately followed by `&`.
fn collect_top_level_statements(root: Node, source: &str) -> Vec<ExtractedStatement> {
    let mut statements: Vec<ExtractedStatement> = Vec::new();
    let mut most_recent_indices: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return statements;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            ";" => most_recent_indices.clear(),
            "&" => {
                for index in &most_recent_indices {
                    if let ExtractedStatement::Command(command) = &mut statements[*index] {
                        command.backgrounded = true;
                    }
                }
                most_recent_indices.clear();
            }
            _ if !child.is_named() => {
                // Any other anonymous token at program level (unexpected by
                // today's grammar) is ignored defensively rather than
                // panicking.
            }
            _ => {
                let start = statements.len();
                collect_statement(child, source, &mut statements);
                most_recent_indices = (start..statements.len()).collect();
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    statements
}

/// Classify one statement-level node, expanding `list`/`pipeline` into their
/// nested statements (each evaluated independently), or recording an
/// unsupported node type as [`ExtractedStatement::Unanalysable`].
fn collect_statement(node: Node, source: &str, out: &mut Vec<ExtractedStatement>) {
    match node.kind() {
        "command" => out.push(extract_command(node, source)),
        "list" | "pipeline" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect_statement(child, source, out);
            }
        }
        "redirected_statement" => {
            let body = node.child_by_field_name("body");
            let mut has_redirect = false;
            let mut all_redirects_harmless = true;
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "file_redirect" => {
                        has_redirect = true;
                        all_redirects_harmless &= is_harmless_redirect(child, source);
                    }
                    "command" | "list" | "pipeline" => {}
                    _ => all_redirects_harmless = false,
                }
            }
            if let Some(body) = body.filter(|body| {
                matches!(body.kind(), "command" | "list" | "pipeline")
                    && has_redirect
                    && all_redirects_harmless
            }) {
                collect_statement(body, source, out);
            } else {
                out.push(ExtractedStatement::Unanalysable);
            }
        }
        _ => out.push(ExtractedStatement::Unanalysable),
    }
}

/// Return whether a `file_redirect` belongs to the literal harmless set.
fn is_harmless_redirect(node: Node, source: &str) -> bool {
    let mut operator = None;
    let mut descriptor = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "file_descriptor" => descriptor = Some(node_text(child, source)),
            kind if !child.is_named() => operator = Some(kind),
            _ => {}
        }
    }
    let destination = node.child_by_field_name("destination");
    let mut cursor = node.walk();
    if node
        .named_children(&mut cursor)
        .any(|child| !matches!(child.kind(), "file_descriptor" | "word" | "number"))
    {
        return false;
    }
    if operator.is_none_or(|kind| {
        !matches!(
            kind,
            ">" | ">>" | "<" | ">&" | "<&" | "&>" | "&>>" | ">&-" | "<&-"
        )
    }) {
        return false;
    }
    let descriptor_is_digits = descriptor
        .is_none_or(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()));
    if !descriptor_is_digits {
        return false;
    }
    match operator {
        Some(">&") => match destination {
            Some(destination) => {
                destination.kind() == "number"
                    && !node_text(destination, source).is_empty()
                    && node_text(destination, source)
                        .bytes()
                        .all(|byte| byte.is_ascii_digit())
            }
            None => false,
        },
        Some("<&") if descriptor.is_some() => match destination {
            Some(destination) => {
                destination.kind() == "number"
                    && !node_text(destination, source).is_empty()
                    && node_text(destination, source)
                        .bytes()
                        .all(|byte| byte.is_ascii_digit())
            }
            None => false,
        },
        Some(">&-") => destination.is_none(),
        Some(">") => destination.is_some_and(|destination| {
            destination.kind() == "word" && node_text(destination, source) == "/dev/null"
        }),
        Some(">>") if descriptor.is_none() => destination.is_some_and(|destination| {
            destination.kind() == "word" && node_text(destination, source) == "/dev/null"
        }),
        Some("&>" | "&>>" | "<") if descriptor.is_none() => {
            destination.is_some_and(|destination| {
                destination.kind() == "word" && node_text(destination, source) == "/dev/null"
            })
        }
        _ => false,
    }
}

/// Extract a single `command` node's argv, variable-assignment-prefix flag.
/// Any child node type outside the supported argument shapes
/// (`word`/`string`/`raw_string`/`concatenation`) makes the whole command
/// [`ExtractedStatement::Unanalysable`].
fn extract_command(node: Node, source: &str) -> ExtractedStatement {
    let mut argv = Vec::new();
    let mut has_variable_assignment_prefix = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "variable_assignment" => has_variable_assignment_prefix = true,
            "command_name" => match child
                .named_child(0)
                .and_then(|inner| extract_argv_token(inner, source))
            {
                Some(token) => argv.push(token),
                None => return ExtractedStatement::Unanalysable,
            },
            "word" | "number" | "string" | "raw_string" | "concatenation" => {
                match extract_argv_token(child, source) {
                    Some(token) => argv.push(token),
                    None => return ExtractedStatement::Unanalysable,
                }
            }
            _ => return ExtractedStatement::Unanalysable,
        }
    }
    ExtractedStatement::Command(ExtractedCommand {
        argv,
        has_variable_assignment_prefix,
        backgrounded: false,
    })
}

/// Extract one argv token's literal (dequoted) text and quoted-ness from an
/// argument-shaped node. Returns `None` when the node (or, for
/// `concatenation`, one of its children) is outside the supported shapes —
/// the caller treats that as un-analysable.
fn extract_argv_token(node: Node, source: &str) -> Option<ArgvToken> {
    match node.kind() {
        "word" | "number" => Some(ArgvToken {
            text: node_text(node, source).to_string(),
            quoted: false,
            unquoted_text: node_text(node, source).to_string(),
        }),
        "raw_string" => {
            let text = node_text(node, source);
            let inner = text
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap_or(text);
            Some(ArgvToken {
                text: inner.to_string(),
                quoted: true,
                unquoted_text: QUOTED_PIECE_PLACEHOLDER.to_string(),
            })
        }
        "string" => {
            let mut content = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "\"" => {}
                    "string_content" => content.push_str(node_text(child, source)),
                    // A nested expansion (`simple_expansion`,
                    // `command_substitution`, ...) makes the value dynamic:
                    // un-analysable, not silently ignored.
                    _ => return None,
                }
            }
            Some(ArgvToken {
                text: content,
                quoted: true,
                unquoted_text: QUOTED_PIECE_PLACEHOLDER.to_string(),
            })
        }
        "concatenation" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            if children.is_empty() {
                return None;
            }
            // Bare words, `"..."` and `'...'` pieces are joined as the shell
            // does (`--format='%h %s'` is the one argument `--format=%h %s`).
            // Any other piece (an expansion, ...) is un-analysable. Quote
            // removal never splits the token, so pieces stay one argument.
            let mut text = String::new();
            let mut unquoted_text = String::new();
            let mut quoted = true;
            for child in &children {
                let piece = extract_argv_token(*child, source)?;
                if !piece.quoted {
                    quoted = false;
                }
                text.push_str(&piece.text);
                unquoted_text.push_str(&piece.unquoted_text);
            }
            Some(ArgvToken {
                text,
                quoted,
                unquoted_text,
            })
        }
        _ => None,
    }
}

/// Unquoted glob metacharacters: `*`, `?`, or a `[...]` bracket expression,
/// judged on the token's unquoted pieces only.
pub(super) fn token_has_unquoted_glob(token: &ArgvToken) -> bool {
    token.unquoted_text.contains(['*', '?', '['])
}

static UNQUOTED_BRACE_EXPANSION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{[^{}]*,[^{}]*\}").expect("valid brace-expansion regex"));

/// Unquoted brace expansion (`{a,b}`), judged on the token's unquoted pieces
/// only.
pub(super) fn token_has_unquoted_brace_expansion(token: &ArgvToken) -> bool {
    UNQUOTED_BRACE_EXPANSION_PATTERN.is_match(&token.unquoted_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_single_command(source: &str) -> ExtractedCommand {
        match extract(source) {
            ExtractionResult::Statements(mut statements) => {
                assert_eq!(statements.len(), 1, "expected exactly one statement");
                match statements.remove(0) {
                    ExtractedStatement::Command(command) => command,
                    ExtractedStatement::Unanalysable => {
                        panic!("expected an analysable command for {source:?}")
                    }
                }
            }
            ExtractionResult::ParseFailed => panic!("expected a parse for {source:?}"),
        }
    }

    fn argv_texts(command: &ExtractedCommand) -> Vec<&str> {
        command
            .argv
            .iter()
            .map(|token| token.text.as_str())
            .collect()
    }

    // --- "in scope" examples ---

    #[test]
    fn executable_paths_and_wrappers_are_not_normalized_or_unwrapped() {
        for (source, expected) in [
            ("./cat file", vec!["./cat", "file"]),
            ("/tmp/cat file", vec!["/tmp/cat", "file"]),
            ("env cat file", vec!["env", "cat", "file"]),
            ("command cat file", vec!["command", "cat", "file"]),
            ("sudo cat file", vec!["sudo", "cat", "file"]),
        ] {
            let command = extract_single_command(source);
            assert_eq!(argv_texts(&command), expected, "{source}");
        }
        let command = extract_single_command("FOO=bar cat file");
        assert!(command.has_variable_assignment_prefix);
        assert_eq!(argv_texts(&command), vec!["cat", "file"]);
        assert_eq!(extract_single_command("cat ~/file").argv[1].text, "~/file");
    }

    #[test]
    fn extracts_semicolon_separated_commands() {
        let statements = match extract(r#"find . -type f ; echo "Foo" ; cat foo.txt"#) {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements.len(), 3);
        for statement in &statements {
            assert!(matches!(statement, ExtractedStatement::Command(_)));
        }
        let ExtractedStatement::Command(first) = &statements[0] else {
            unreachable!()
        };
        assert_eq!(argv_texts(first), vec!["find", ".", "-type", "f"]);
        let ExtractedStatement::Command(second) = &statements[1] else {
            unreachable!()
        };
        assert_eq!(argv_texts(second), vec!["echo", "Foo"]);
        let ExtractedStatement::Command(third) = &statements[2] else {
            unreachable!()
        };
        assert_eq!(argv_texts(third), vec!["cat", "foo.txt"]);
    }

    #[test]
    fn extracts_piped_commands() {
        let statements = match extract(r#"find . -type f | sort | cut -d "_" -f2"#) {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements.len(), 3);
        let ExtractedStatement::Command(third) = &statements[2] else {
            panic!("expected all three pipeline segments to be analysable")
        };
        assert_eq!(argv_texts(third), vec!["cut", "-d", "_", "-f2"]);
    }

    #[test]
    fn extracts_and_or_list_commands() {
        let statements = match extract("git fetch && git rebase origin/main || echo failed") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements.len(), 3);
        for statement in &statements {
            assert!(matches!(statement, ExtractedStatement::Command(_)));
        }
    }

    // --- un-analysable constructs ---

    #[test]
    fn subshell_is_unanalysable() {
        let statements = match extract("(cd /tmp && ls)") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements, vec![ExtractedStatement::Unanalysable]);
    }

    #[test]
    fn command_substitution_is_unanalysable() {
        for source in ["echo $(whoami)", "echo `whoami`"] {
            let statements = match extract(source) {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse for {source:?}"),
            };
            assert_eq!(
                statements,
                vec![ExtractedStatement::Unanalysable],
                "expected {source:?} to be un-analysable"
            );
        }
    }

    #[test]
    fn bare_variable_reference_is_unanalysable() {
        for source in ["echo $FOO", "echo ${FOO}"] {
            let statements = match extract(source) {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse for {source:?}"),
            };
            assert_eq!(
                statements,
                vec![ExtractedStatement::Unanalysable],
                "expected {source:?} to be un-analysable"
            );
        }
    }

    #[test]
    fn control_flow_constructs_are_unanalysable() {
        for source in [
            "if true; then echo hi; fi",
            "for x in a b; do echo $x; done",
            "while true; do echo hi; done",
            "case $x in a) echo hi ;; esac",
        ] {
            let statements = match extract(source) {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse for {source:?}"),
            };
            assert_eq!(
                statements,
                vec![ExtractedStatement::Unanalysable],
                "expected {source:?} to be un-analysable"
            );
        }
    }

    #[test]
    fn function_definition_is_unanalysable() {
        let statements = match extract("func() { echo hi; }") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements, vec![ExtractedStatement::Unanalysable]);
    }

    #[test]
    fn heredoc_is_unanalysable() {
        let statements = match extract("cat <<EOF\nhi\nEOF") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements, vec![ExtractedStatement::Unanalysable]);
    }

    #[test]
    fn harmless_redirects_extract_the_wrapped_command() {
        for source in [
            "git status 2>&1",
            "git log 2>/dev/null",
            "cmd >/dev/null 2>&1",
            "cmd </dev/null",
            "cmd >&2",
            "cmd 3<&0",
            "cmd >&-",
            "cmd 2>&-",
            "cmd 3>&4",
            "cmd >>/dev/null",
            "cmd &>/dev/null",
            "cmd &>>/dev/null",
            "cmd 9>/dev/null",
        ] {
            let command = extract_single_command(source);
            assert_eq!(
                command.argv[0].text,
                source.split_whitespace().next().unwrap(),
                "{source}"
            );
        }
    }

    #[test]
    fn harmless_redirects_are_supported_in_compound_commands() {
        let statements =
            match extract("cmd 2>&1 | grep x && git status 2>/dev/null || git log >/dev/null") {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse"),
            };
        assert_eq!(statements.len(), 4);
        assert!(statements
            .iter()
            .all(|statement| matches!(statement, ExtractedStatement::Command(_))));
    }

    #[test]
    fn unsafe_and_quoted_redirects_remain_unanalysable() {
        for source in [
            "cmd > file",
            "cmd >> file",
            "cmd 2> file",
            "cmd &> file",
            "cmd >&file",
            "cmd > $VAR",
            "cmd > \"/dev/null\"",
            "cmd <<EOF\nx\nEOF",
            "cmd <<< hi",
            "cmd > /dev/nullx",
            "cmd > ../dev/null",
            "cmd >&$FD",
            "cmd <(echo hi)",
            "cmd > >(cat)",
            "cmd >/dev/null > file",
            "cmd 2>&1 > file",
            "cmd 9>>/dev/null",
            "cmd <&0",
            "cmd 2<&-",
            "cmd >\"/dev/null\"",
        ] {
            let statements = match extract(source) {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse for {source:?}"),
            };
            assert_eq!(
                statements,
                vec![ExtractedStatement::Unanalysable],
                "{source}"
            );
        }
    }

    #[test]
    fn harmless_redirect_does_not_hide_backgrounding() {
        let command = extract_single_command("sleep 10 2>&1 &");
        assert!(command.backgrounded);
    }

    // --- backgrounding ---

    #[test]
    fn trailing_ampersand_flags_backgrounded() {
        let command = extract_single_command("sleep 10 &");
        assert!(command.backgrounded);
    }

    #[test]
    fn ampersand_separated_siblings_are_each_flagged() {
        let statements = match extract("cmd1 & cmd2") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        assert_eq!(statements.len(), 2);
        let ExtractedStatement::Command(first) = &statements[0] else {
            panic!("expected a command")
        };
        assert!(first.backgrounded, "cmd1 is backgrounded by the `&`");
        let ExtractedStatement::Command(second) = &statements[1] else {
            panic!("expected a command")
        };
        assert!(!second.backgrounded, "cmd2 is not itself followed by `&`");
    }

    #[test]
    fn semicolon_separated_command_is_not_backgrounded() {
        let statements = match extract("cmd1 ; cmd2") {
            ExtractionResult::Statements(statements) => statements,
            ExtractionResult::ParseFailed => panic!("expected a parse"),
        };
        let ExtractedStatement::Command(first) = &statements[0] else {
            panic!("expected a command")
        };
        assert!(!first.backgrounded);
    }

    // --- variable assignment prefix ---

    #[test]
    fn leading_variable_assignment_is_flagged_and_argv_still_extracted() {
        let command = extract_single_command("FOO=bar git status");
        assert!(command.has_variable_assignment_prefix);
        assert_eq!(argv_texts(&command), vec!["git", "status"]);
    }

    #[test]
    fn plain_command_has_no_variable_assignment_prefix() {
        let command = extract_single_command("git status");
        assert!(!command.has_variable_assignment_prefix);
    }

    // --- parse failure ---

    #[test]
    fn dangling_pipe_is_a_parse_failure() {
        assert_eq!(extract("cmd1 |"), ExtractionResult::ParseFailed);
    }

    // --- quoting ---

    #[test]
    fn double_and_single_quoted_arguments_are_quoted_tokens() {
        let command = extract_single_command(r#"cat "*.txt" '*.md'"#);
        assert_eq!(command.argv[1].text, "*.txt");
        assert!(command.argv[1].quoted);
        assert_eq!(command.argv[2].text, "*.md");
        assert!(command.argv[2].quoted);
    }

    #[test]
    fn unquoted_brace_expansion_is_a_single_unquoted_token() {
        let command = extract_single_command("echo {a,b}");
        assert_eq!(command.argv[1].text, "{a,b}");
        assert!(!command.argv[1].quoted);
    }

    #[test]
    fn quoted_brace_expansion_text_is_a_quoted_token() {
        let command = extract_single_command(r#"echo "{a,b}""#);
        assert_eq!(command.argv[1].text, "{a,b}");
        assert!(command.argv[1].quoted);
    }

    // --- unquoted glob/brace token detection ---

    #[test]
    fn unquoted_glob_is_detected() {
        let command = extract_single_command("cat *.txt");
        assert!(token_has_unquoted_glob(&command.argv[1]));
        assert!(!token_has_unquoted_brace_expansion(&command.argv[1]));
    }

    #[test]
    fn quoted_glob_like_text_is_not_flagged() {
        let command = extract_single_command(r#"cat "*.txt""#);
        assert!(!token_has_unquoted_glob(&command.argv[1]));
        let command = extract_single_command("cat '*.txt'");
        assert!(!token_has_unquoted_glob(&command.argv[1]));
    }

    #[test]
    fn unquoted_brace_expansion_is_detected() {
        let command = extract_single_command("echo {a,b}");
        assert!(token_has_unquoted_brace_expansion(&command.argv[1]));
    }

    #[test]
    fn quoted_brace_expansion_text_is_not_flagged() {
        let command = extract_single_command(r#"echo "{a,b}""#);
        assert!(!token_has_unquoted_brace_expansion(&command.argv[1]));
    }

    #[test]
    fn partly_quoted_token_joins_its_pieces_into_one_argument() {
        let command = extract_single_command("git log --format='%h %s' \"a\"b'c'");
        assert_eq!(command.argv[2].text, "--format=%h %s");
        assert!(!command.argv[2].quoted);
        assert_eq!(command.argv[3].text, "abc");
    }

    #[test]
    fn glob_in_quoted_piece_of_partly_quoted_token_is_not_flagged() {
        let command = extract_single_command("grep --include='*.rs' x");
        assert_eq!(command.argv[1].text, "--include=*.rs");
        assert!(!token_has_unquoted_glob(&command.argv[1]));
    }

    #[test]
    fn glob_or_brace_in_bare_piece_of_partly_quoted_token_is_flagged() {
        let command = extract_single_command("ls a*'b' {a,'b'}");
        assert!(token_has_unquoted_glob(&command.argv[1]));
        assert!(token_has_unquoted_brace_expansion(&command.argv[2]));
    }

    #[test]
    fn partly_quoted_token_with_expansion_is_unanalysable() {
        for source in ["echo --x='a'$Y", "echo a\"$Y\"b"] {
            let statements = match extract(source) {
                ExtractionResult::Statements(statements) => statements,
                ExtractionResult::ParseFailed => panic!("expected a parse"),
            };
            assert_eq!(
                statements,
                vec![ExtractedStatement::Unanalysable],
                "{source}"
            );
        }
    }

    #[test]
    fn token_without_glob_or_brace_characters_is_not_flagged() {
        let command = extract_single_command("git status");
        assert!(!token_has_unquoted_glob(&command.argv[1]));
        assert!(!token_has_unquoted_brace_expansion(&command.argv[1]));
    }

    // --- grammar-assumption tests: fail loudly on a tree-sitter-bash bump
    // that silently changes these shapes (see module doc comment). ---
    mod grammar_assumptions {
        use tree_sitter::Parser;

        fn parse(source: &str) -> tree_sitter::Tree {
            let mut parser = Parser::new();
            parser
                .set_language(&tree_sitter_bash::LANGUAGE.into())
                .unwrap();
            parser.parse(source, None).unwrap()
        }

        #[test]
        fn redirects_wrap_body_and_expose_operator_fields() {
            for (source, operator, destination_kind, descriptor) in [
                ("git status 2>&1", ">&", Some("number"), Some("2")),
                ("cmd >/dev/null", ">", Some("word"), None),
                ("cmd &>/dev/null", "&>", Some("word"), None),
                ("cmd </dev/null", "<", Some("word"), None),
                ("cmd >&2", ">&", Some("number"), None),
                ("cmd 3<&0", "<&", Some("number"), Some("3")),
                ("cmd 2>&-", ">&-", None, Some("2")),
                ("cmd >&file", ">&", Some("word"), None),
                ("cmd > file", ">", Some("word"), None),
            ] {
                let tree = parse(source);
                let statement = tree.root_node().named_child(0).unwrap();
                assert_eq!(statement.kind(), "redirected_statement", "{source}");
                assert_eq!(
                    statement.child_by_field_name("body").unwrap().kind(),
                    "command"
                );
                let redirect = statement.child_by_field_name("redirect").unwrap();
                assert_eq!(redirect.kind(), "file_redirect");
                assert_eq!(
                    redirect
                        .child_by_field_name("destination")
                        .map(|node| node.kind()),
                    destination_kind
                );
                assert_eq!(
                    redirect
                        .child_by_field_name("descriptor")
                        .map(|node| node.utf8_text(source.as_bytes()).unwrap()),
                    descriptor
                );
                let mut cursor = redirect.walk();
                let operators: Vec<&str> = redirect
                    .children(&mut cursor)
                    .filter(|child| !child.is_named())
                    .map(|child| child.kind())
                    .collect();
                assert!(operators.contains(&operator), "{source}: {operators:?}");
            }
            let tree = parse("cmd 2>&1 | grep x");
            let pipeline = tree.root_node().named_child(0).unwrap();
            assert_eq!(pipeline.kind(), "pipeline");
            assert_eq!(
                pipeline.named_child(0).unwrap().kind(),
                "redirected_statement"
            );
        }

        #[test]
        fn no_simple_command_node_type_exists() {
            let tree = parse("echo hi");
            let root = tree.root_node();
            assert_eq!(root.child(0).unwrap().kind(), "command");
        }

        #[test]
        fn list_node_is_produced_only_for_and_or() {
            let tree = parse("a && b");
            assert_eq!(tree.root_node().child(0).unwrap().kind(), "list");
            let tree = parse("a ; b");
            assert_ne!(tree.root_node().child(0).unwrap().kind(), "list");
        }

        #[test]
        fn semicolon_and_ampersand_are_non_grouping_terminator_siblings() {
            let tree = parse("a ; b");
            let root = tree.root_node();
            assert_eq!(root.child_count(), 3);
            assert_eq!(root.child(0).unwrap().kind(), "command");
            assert_eq!(root.child(1).unwrap().kind(), ";");
            assert_eq!(root.child(2).unwrap().kind(), "command");

            let tree = parse("a & b");
            let root = tree.root_node();
            assert_eq!(root.child_count(), 3);
            assert_eq!(root.child(1).unwrap().kind(), "&");
        }

        #[test]
        fn variable_assignment_precedes_command_name_field() {
            let tree = parse("FOO=bar git status");
            let command = tree.root_node().child(0).unwrap();
            assert_eq!(command.kind(), "command");
            let mut cursor = command.walk();
            let kinds: Vec<&str> = command
                .named_children(&mut cursor)
                .map(|child| child.kind())
                .collect();
            assert_eq!(kinds[0], "variable_assignment");
            assert_eq!(kinds[1], "command_name");
        }
    }
}
