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

use super::*;
use crate::sandbox::bash_command_extractor::{extract, ExtractedStatement, ExtractionResult};

fn command(arguments: &[&str]) -> ExtractedCommand {
    ExtractedCommand {
        argv: arguments
            .iter()
            .map(|text| ArgvToken {
                text: (*text).to_owned(),
                quoted: false,
                unquoted_text: (*text).to_owned(),
            })
            .collect(),
        ..ExtractedCommand::default()
    }
}

fn canonical(analysis: &ArgumentAnalysis) -> Vec<&'static str> {
    analysis
        .identities
        .iter()
        .map(|aliases| aliases[0])
        .collect()
}

#[test]
fn useful_reads_have_complete_option_and_subcommand_analyses() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["cat", "file"], &[]),
        (&["cat", "-bn", "file"], &["-b", "-n"]),
        (&["cat", "--number", "file"], &["-n"]),
        (&["ls", "-lah", "."], &["-l", "-a", "-h"]),
        (&["ls", "--color=auto", "."], &["--color"]),
        (&["head", "-n", "10", "file"], &["-n"]),
        (&["tail", "-n+5", "file"], &["-n"]),
        (&["wc", "-lw", "file"], &["-l", "-w"]),
        (&["grep", "-n", "-e", "needle", "file"], &["-n", "-e"]),
        (&["rg", "-n", "-e", "needle", "."], &["-n", "-e"]),
        (&["rg", "needle", ".", "-n"], &["-n"]),
        (&["sort", "-nr", "file"], &["-n", "-r"]),
        (
            &["find", ".", "-name", "*.rs", "-type", "f", "-print"],
            &["-name", "-type", "-print"],
        ),
        (&["git", "status", "--short"], &["status", "-s"]),
        (&["git", "log", "--oneline"], &["log", "--oneline"]),
        (&["git", "diff", "--stat"], &["diff", "--stat"]),
        (&["git", "show", "HEAD"], &["show"]),
    ];
    for (arguments, expected) in cases {
        let analysis = parse(&command(arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, expected.to_vec()),
            "{arguments:?}: {analysis:?}"
        );
    }
}

#[test]
fn equivalent_short_and_long_options_share_all_aliases() {
    for (name, short, long, value) in [
        ("cat", "-n", "--number", None),
        ("ls", "-a", "--all", None),
        ("head", "-n", "--lines", Some("3")),
        ("tail", "-c", "--bytes", Some("3")),
        ("wc", "-l", "--lines", None),
        ("grep", "-e", "--regexp", Some("pattern")),
        ("rg", "-e", "--regexp", Some("pattern")),
        ("sort", "-o", "--output", Some("result")),
        ("sort", "-c", "--check", None),
    ] {
        let mut short_arguments = vec![name, short];
        let mut long_arguments = vec![name, long];
        if let Some(value) = value {
            short_arguments.push(value);
            long_arguments.push(value);
        }
        assert_eq!(
            parse(&command(&short_arguments)).identities,
            parse(&command(&long_arguments)).identities,
            "{name}: {short} / {long}"
        );
    }
}

#[test]
fn legacy_head_and_tail_counts_share_the_attached_lines_option_analysis() {
    for name in ["head", "tail"] {
        for count in ["0", "1", "00010", "184467440737095516160"] {
            let legacy = format!("-{count}");
            let modern = format!("-n{count}");
            let analysis = parse(&command(&[name, &legacy, "first", "second"]));
            assert!(analysis.complete, "{name} {legacy}: {analysis:?}");
            assert_eq!(
                analysis,
                parse(&command(&[name, &modern, "first", "second"])),
                "{name} {legacy}"
            );
            assert_eq!(analysis.operands, vec![2, 3]);
            assert_eq!(analysis.option_values, vec![1]);
        }
    }
}

#[test]
fn legacy_count_recognition_preserves_terminators_option_values_and_later_ambiguity() {
    for name in ["head", "tail"] {
        let terminated = parse(&command(&[name, "--", "-10"]));
        assert!(terminated.complete && terminated.identities.is_empty());
        assert_eq!(terminated.operands, vec![2]);

        let value = parse(&command(&[name, "-c", "-10", "file"]));
        assert!(value.complete);
        assert_eq!(canonical(&value), vec!["-c"]);
        assert_eq!(value.option_values, vec![2]);

        let incomplete = parse(&command(&[name, "-10", "--unknown", "-c", "3"]));
        assert!(!incomplete.complete);
        assert_eq!(canonical(&incomplete), vec!["-n"]);
    }
}

#[test]
fn unknown_arity_preserves_only_earlier_confident_evidence() {
    for (arguments, expected) in [
        (vec!["cat", "-n", "--unknown", "-b"], vec!["-n"]),
        (
            vec!["find", ".", "-name", "x", "-weird", "-exec"],
            vec!["-name"],
        ),
        (vec!["sort", "-r", "--unknown", "-o", "out"], vec!["-r"]),
        (vec!["rg", "-n", "--unknown", "--pre", "cmd"], vec!["-n"]),
        (vec!["git", "--no-pager", "--unknown", "status"], vec!["-P"]),
        (
            vec!["git", "diff", "--unknown", "--output", "out"],
            vec!["diff"],
        ),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (false, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn unknown_bundle_suffix_provides_no_bundle_evidence() {
    for arguments in [
        vec!["cat", "-nz", "-b"],
        vec!["cat", "-n☃", "-b"],
        vec!["ls", "-laI", "-r"],
        vec!["sort", "-rZ", "out"],
        vec!["git", "-pP", "status"],
        vec!["git", "status", "-sb"],
    ] {
        let analysis = parse(&command(&arguments));
        let expected = if arguments[0] == "git" && arguments[1] == "status" {
            vec!["status"]
        } else {
            Vec::new()
        };
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (false, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn missing_required_values_keep_the_option_but_are_incomplete() {
    for arguments in [
        vec!["head", "-n"],
        vec!["tail", "--bytes"],
        vec!["grep", "-e"],
        vec!["rg", "--pre"],
        vec!["sort", "-o"],
        vec!["find", ".", "-name"],
        vec!["find", ".", "-fprintf", "out"],
        vec!["git", "-c"],
        vec!["git", "--config-env"],
        vec!["git", "diff", "--output"],
        vec!["git", "log", "--format"],
    ] {
        let analysis = parse(&command(&arguments));
        assert!(
            !analysis.complete && !analysis.identities.is_empty(),
            "{arguments:?}: {analysis:?}"
        );
    }
}

#[test]
fn terminators_leave_option_looking_operands_out_of_identities() {
    for arguments in [
        vec!["cat", "--", "-n", "--number"],
        vec!["ls", "--", "--all"],
        vec!["grep", "--", "-e", "-n"],
        vec!["rg", "--", "--pre", "-n"],
        vec!["sort", "--", "-o", "result"],
        vec!["find", "--", "./-exec", "status"],
    ] {
        let analysis = parse(&command(&arguments));
        assert!(
            analysis.complete && analysis.identities.is_empty(),
            "{arguments:?}: {analysis:?}"
        );
    }
    let analysis = parse(&command(&["git", "status", "--", "--output", "status"]));
    assert_eq!(
        (analysis.complete, canonical(&analysis)),
        (true, vec!["status"])
    );
}

#[test]
fn values_cannot_impersonate_options_or_subcommands() {
    for (arguments, expected) in [
        (vec!["grep", "-e", "-n", "status"], vec!["-e"]),
        (vec!["grep", "-f", "--recursive", "file"], vec!["-f"]),
        (vec!["rg", "--glob=--pre", "pattern"], vec!["-g"]),
        (vec!["rg", "-e--pre", "."], vec!["-e"]),
        (vec!["sort", "-o", "--compress-program"], vec!["-o"]),
        (vec!["find", ".", "-name", "-exec"], vec!["-name"]),
        (vec!["git", "-C", "status", "diff"], vec!["-C", "diff"]),
        (
            vec!["git", "log", "--grep", "--output"],
            vec!["log", "--grep"],
        ),
        (vec!["git", "show", "status"], vec!["show"]),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn git_global_values_do_not_select_a_subcommand() {
    for arguments in [
        vec!["git", "-c", "status"],
        vec!["git", "-Cstatus"],
        vec!["git", "--git-dir=status"],
        vec!["git", "--config-env=status"],
        vec!["git", "--exec-path=status"],
        vec!["git", "--work-tree", "status"],
        vec!["git", "fetch", "status"],
        vec!["git", "--", "status"],
    ] {
        let analysis = parse(&command(&arguments));
        assert!(
            !analysis.complete
                && !analysis
                    .identities
                    .iter()
                    .any(|aliases| aliases.contains(&"status")),
            "{arguments:?}: {analysis:?}"
        );
    }
}

#[test]
fn git_log_numeric_counts_share_the_attached_max_count_option_analysis() {
    for count in ["0", "5", "00010", "184467440737095516160"] {
        let numeric = format!("-{count}");
        let modern = format!("-n{count}");
        for prefix in [
            vec!["git", "log"],
            vec!["git", "log", "--oneline"],
            vec!["git", "log", "HEAD"],
            vec!["git", "-C", "workspace", "log"],
        ] {
            let mut numeric_arguments = prefix.clone();
            numeric_arguments.extend([numeric.as_str(), "HEAD"]);
            let mut modern_arguments = prefix;
            modern_arguments.extend([modern.as_str(), "HEAD"]);
            let analysis = parse(&command(&numeric_arguments));
            assert!(analysis.complete, "{numeric_arguments:?}: {analysis:?}");
            assert_eq!(
                analysis,
                parse(&command(&modern_arguments)),
                "{numeric_arguments:?}"
            );
        }
    }
}

#[test]
fn git_log_numeric_counts_preserve_value_terminator_and_ambiguity_boundaries() {
    for (arguments, complete, identities) in [
        (vec!["git", "log", "--", "-5"], true, vec!["log"]),
        (
            vec!["git", "log", "--grep", "-5"],
            true,
            vec!["log", "--grep"],
        ),
        (vec!["git", "-C", "-5", "log"], true, vec!["-C", "log"]),
        (
            vec!["git", "log", "-5", "--unknown", "--oneline"],
            false,
            vec!["log", "-n"],
        ),
        (vec!["git", "log", "--unknown", "-5"], false, vec!["log"]),
        (vec!["git", "log", "-5x", "--oneline"], false, vec!["log"]),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (complete, identities),
            "{arguments:?}"
        );
    }
}

#[test]
fn git_check_ignore_verbose_aliases_preserve_path_placement() {
    let short = parse(&command(&["git", "check-ignore", "-v", "first", "second"]));
    let long = parse(&command(&[
        "git",
        "check-ignore",
        "--verbose",
        "first",
        "second",
    ]));
    assert_eq!(short, long);
    assert!(short.complete);
    assert_eq!(canonical(&short), vec!["check-ignore", "-v"]);
    assert_eq!(short.operands, vec![3, 4]);
    assert!(short.option_values.is_empty());
}

#[test]
fn git_check_ignore_requires_explicit_paths() {
    for arguments in [
        vec!["git", "check-ignore"],
        vec!["git", "check-ignore", "-v"],
        vec!["git", "check-ignore", "--"],
        vec!["git", "check-ignore", "-v", "--"],
        vec!["git", "-C", "workspace", "check-ignore", "-v"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn git_check_ignore_preserves_terminator_and_ambiguity_boundaries() {
    let terminated = parse(&command(&[
        "git",
        "check-ignore",
        "-v",
        "--",
        "--verbose",
        "log",
        "--output=output",
    ]));
    assert!(terminated.complete);
    assert_eq!(canonical(&terminated), vec!["check-ignore", "-v"]);
    assert_eq!(terminated.operands, vec![4, 5, 6]);

    for (arguments, expected) in [
        (
            vec!["git", "check-ignore", "-v", "--unknown", "file"],
            vec!["check-ignore", "-v"],
        ),
        (
            vec!["git", "check-ignore", "--unknown", "-v", "file"],
            vec!["check-ignore"],
        ),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (false, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn git_option_scopes_are_not_interchangeable() {
    for arguments in [
        vec!["git", "check-ignore", "--short", "file"],
        vec!["git", "check-ignore", "--stat", "file"],
        vec!["git", "check-ignore", "-5", "file"],
        vec!["git", "check-ignore", "--stdin"],
        vec!["git", "check-ignore", "--no-index", "file"],
        vec!["git", "check-ignore", "-q", "file"],
        vec!["git", "check-ignore", "-z", "file"],
        vec!["git", "check-ignore", "-n", "file"],
        vec!["git", "-5", "log"],
        vec!["git", "status", "-5"],
        vec!["git", "diff", "-5"],
        vec!["git", "show", "-5"],
        vec!["git", "log", "--5"],
        vec!["git", "log", "-5.0"],
        vec!["git", "log", "-\u{0665}"],
        vec!["git", "--oneline", "log"],
        vec!["git", "status", "--output", "out"],
        vec!["git", "status", "--ext-diff"],
        vec!["git", "status", "--textconv"],
        vec!["git", "diff", "--branch"],
        vec!["git", "show", "-c", "status"],
        vec!["git", "diff", "--no-pager"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn git_options_after_revisions_remain_options_until_the_terminator() {
    let analysis = parse(&command(&["git", "diff", "HEAD", "--output=result"]));
    assert_eq!(
        (analysis.complete, canonical(&analysis)),
        (true, vec!["diff", "--output"])
    );
}

#[test]
fn dangerous_execution_and_output_forms_are_recognized_without_a_verdict() {
    for (arguments, expected) in [
        (vec!["sort", "-oresult", "file"], vec!["-o"]),
        (vec!["sort", "--output=result", "file"], vec!["-o"]),
        (
            vec!["sort", "--compress-program=command", "file"],
            vec!["--compress-program"],
        ),
        (vec!["rg", "--pre=command", "pattern"], vec!["--pre"]),
        (vec!["rg", "pattern", "--pre", "command"], vec!["--pre"]),
        (vec!["find", ".", "-delete"], vec!["-delete"]),
        (vec!["find", ".", "-fprint", "out"], vec!["-fprint"]),
        (vec!["find", ".", "-fprint0", "out"], vec!["-fprint0"]),
        (vec!["find", ".", "-fprintf", "out", "%p"], vec!["-fprintf"]),
        (
            vec!["git", "-c", "alias.x=command", "status"],
            vec!["-c", "status"],
        ),
        (
            vec!["git", "--config-env=key=VARIABLE", "status"],
            vec!["--config-env", "status"],
        ),
        (
            vec!["git", "--exec-path=directory", "status"],
            vec!["--exec-path", "status"],
        ),
        (
            vec!["git", "diff", "--ext-diff", "--textconv", "--output=out"],
            vec!["diff", "--ext-diff", "--textconv", "--output"],
        ),
        (
            vec!["git", "log", "--output", "out"],
            vec!["log", "--output"],
        ),
        (
            vec!["git", "show", "--textconv", "HEAD"],
            vec!["show", "--textconv"],
        ),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn find_execution_consumes_nested_argv_through_structural_terminators() {
    for primary in ["-exec", "-execdir", "-ok", "-okdir"] {
        let arguments = [
            "find", ".", primary, "command", "--output", "{}", ";", "-print",
        ];
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, vec![primary, "-print"]),
            "{arguments:?}"
        );
    }
    for primary in ["-exec", "-execdir"] {
        let analysis = parse(&command(&[
            "find", ".", primary, "command", "{}", "+", "-print",
        ]));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, vec![primary, "-print"])
        );
    }
}

#[test]
fn find_unterminated_or_malformed_execution_is_incomplete() {
    for arguments in [
        vec!["find", ".", "-exec"],
        vec!["find", ".", "-exec", ";"],
        vec!["find", ".", "-exec", "command", "{}", "-delete"],
        vec!["find", ".", "-exec", "command", "+"],
        vec!["find", ".", "-exec", "{}", "+"],
        vec!["find", ".", "-exec", "command", "{}", "{}", "+"],
        vec!["find", ".", "-exec", "command{}", "{}", "+"],
        vec!["find", ".", "-ok", "command", "{}", "+"],
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (false, vec![arguments[2]]),
            "{arguments:?}"
        );
    }
}

#[test]
fn find_plus_not_after_placeholder_is_an_execution_argument() {
    let analysis = parse(&command(&[
        "find", ".", "-exec", "command", "+", "-delete", ";", "-print",
    ]));
    assert_eq!(
        (analysis.complete, canonical(&analysis)),
        (true, vec!["-exec", "-print"])
    );
}

#[test]
fn find_paths_and_primary_values_are_separate_from_identities() {
    let analysis = parse(&command(&[
        "find", "./-exec", "status", "-name", "-exec", "-print",
    ]));
    assert_eq!(
        (
            analysis.complete,
            canonical(&analysis),
            analysis.operands,
            analysis.option_values,
        ),
        (true, vec!["-name", "-print"], vec![1, 2], vec![4])
    );
}

#[test]
fn find_boolean_expressions_are_balanced_and_have_operands() {
    for arguments in [
        vec!["find", "."],
        vec!["find", "-L", ".", "-type", "f"],
        vec![
            "find", ".", "(", "-name", "a", "-o", "-name", "b", ")", "-print",
        ],
        vec!["find", ".", "!", "-type", "d", "-a", "-print"],
        vec![
            "find", ".", "-not", "(", "-name", "a", "-and", "-type", "f", ")",
        ],
    ] {
        assert!(parse(&command(&arguments)).complete, "{arguments:?}");
    }
    for arguments in [
        vec!["find"],
        vec!["find", "-L"],
        vec!["find", "-name", "x"],
        vec!["find", ".", "("],
        vec!["find", ".", "(", ")"],
        vec!["find", ".", "-print", ")"],
        vec!["find", ".", "-print", "-o"],
        vec!["find", ".", "-a", "-print"],
        vec!["find", "-L", ".", "-a", "-print"],
        vec!["find", ".", "-print", "-a", "-o", "-delete"],
        vec!["find", ".", "!", "-and", "-delete"],
        vec!["find", ".", "-print", "filename"],
        vec!["find", ".", "-H", "-delete"],
        vec!["find", ".", "-name=x", "-delete"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn optional_attached_values_do_not_consume_the_next_token() {
    for (arguments, expected) in [
        (vec!["ls", "--color", "status"], vec!["--color"]),
        (vec!["grep", "--color", "pattern"], vec!["--color"]),
        (
            vec!["git", "status", "--porcelain", "status"],
            vec!["status", "--porcelain"],
        ),
        (
            vec!["git", "status", "-uno", "--short"],
            vec!["status", "-u", "-s"],
        ),
        (
            vec!["git", "status", "--ignored=matching"],
            vec!["status", "--ignored"],
        ),
    ] {
        let analysis = parse(&command(&arguments));
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn conflicting_arities_and_unsupported_forms_are_incomplete() {
    for arguments in [
        vec!["ls", "-I", "pattern", "-a"],
        vec!["ls", "-T", "4", "-a"],
        vec!["tail", "-f", "file"],
        vec!["tail", "-fname", "file"],
        vec!["grep", "-C", "pattern"],
        vec!["head", "-10c", "file"],
        vec!["tail", "-10f", "file"],
        vec!["head", "-10.5", "file"],
        vec!["tail", "--10", "file"],
        vec!["head", "-\u{0661}\u{0660}", "file"],
        vec!["head", "-q", "-10", "file"],
        vec!["tail", "-q", "-10", "file"],
        vec!["head", "file", "-10"],
        vec!["tail", "file", "-10"],
        vec!["cat", "-10", "file"],
        vec!["cat", "--number=value"],
        vec!["cat", "--num"],
        vec!["sed", "-i", "program", "file"],
        vec!["awk", "program"],
        vec!["git", "checkout", "status"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn bsd_gnu_operand_order_ambiguity_stops_later_option_evidence() {
    for arguments in [
        vec!["cat", "-n", "file", "-b"],
        vec!["ls", ".", "-l"],
        vec!["head", "file", "-n", "3"],
        vec!["sort", "file", "-o", "out"],
    ] {
        let analysis = parse(&command(&arguments));
        let expected = if arguments[0] == "cat" {
            vec!["-n"]
        } else {
            vec![]
        };
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (false, expected),
            "{arguments:?}"
        );
    }
}

#[test]
fn quoted_literals_are_analyzed_as_argv_without_running_the_shell() {
    for (source, expected) in [
        ("cat '-n' 'file name'", vec!["-n"]),
        ("head '-10' 'file name'", vec!["-n"]),
        ("tail '-10' 'file name'", vec!["-n"]),
        ("git log '-5' HEAD", vec!["log", "-n"]),
        (
            "git check-ignore --verbose 'file name'",
            vec!["check-ignore", "-v"],
        ),
        ("cat -- '-n'", vec![]),
        ("grep -e '--recursive' 'file name'", vec!["-e"]),
        ("find . -name '-exec'", vec!["-name"]),
        ("find . -exec command '{}' ';'", vec!["-exec"]),
        (
            "git -C 'status' diff '--output=result name'",
            vec!["-C", "diff", "--output"],
        ),
    ] {
        let ExtractionResult::Statements(statements) = extract(source) else {
            panic!("fixture must be extractable: {source}");
        };
        let [ExtractedStatement::Command(command)] = statements.as_slice() else {
            panic!("fixture must contain one command: {source}");
        };
        let analysis = parse(command);
        assert_eq!(
            (analysis.complete, canonical(&analysis)),
            (true, expected),
            "{source}"
        );
    }
}

#[test]
fn empty_argv_and_unsupported_commands_are_incomplete() {
    for arguments in [vec![], vec![""], vec!["./cat"], vec!["env", "cat"]] {
        let analysis = parse(&command(&arguments));
        assert!(
            !analysis.complete && analysis.identities.is_empty(),
            "{arguments:?}"
        );
    }
}

#[test]
fn grep_and_ripgrep_require_a_pattern_except_standalone_modes() {
    for arguments in [
        vec!["grep"],
        vec!["grep", "-n"],
        vec!["grep", "--"],
        vec!["rg"],
        vec!["rg", "-n"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
    for arguments in [
        vec!["grep", "--help"],
        vec!["rg", "--files"],
        vec!["rg", "--type-list"],
        vec!["rg", "--version"],
        vec!["grep", "-e", ""],
    ] {
        assert!(parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn generated_unknown_arity_never_completes_or_exposes_later_evidence() {
    let commands = [
        "cat", "ls", "head", "tail", "wc", "grep", "rg", "sort", "find", "git", "sed", "printf",
        "cd",
    ];
    let suffixes = [
        "",
        "-n",
        "--pre",
        "-exec",
        "--output=out",
        "status",
        "--",
        "☃",
        "{}",
    ];
    for name in commands {
        for length in 0..24 {
            let unknown = format!("--ylva-unknown-{}", "x".repeat(length));
            for suffix in suffixes {
                let mut arguments = vec![name];
                if name == "find" {
                    arguments.push(".");
                }
                arguments.extend([unknown.as_str(), suffix]);
                let analysis = parse(&command(&arguments));
                assert!(
                    !analysis.complete && analysis.identities.is_empty(),
                    "{arguments:?}: {analysis:?}"
                );
            }
        }
    }
}

#[test]
fn generated_bundles_do_not_panic_or_admit_unknown_flag_suffixes() {
    for unknown in ['?', '☃', 'é', '=', '\0'] {
        for prefix in ["", "n", "bn", "bns"] {
            let bundle = format!("-{prefix}{unknown}");
            let analysis = parse(&command(&["cat", &bundle, "-v"]));
            assert!(
                !analysis.complete && analysis.identities.is_empty(),
                "{bundle:?}: {analysis:?}"
            );
        }
    }
}

fn analyze(arguments: &[&str]) -> (bool, Vec<&'static str>) {
    let analysis = parse(&command(arguments));
    (analysis.complete, canonical(&analysis))
}

#[test]
fn sed_read_only_forms_are_complete() {
    let cases: &[(&[&str], &[&str])] = &[
        (&["sed", "-n", "20,150p", "f"], &["-n"]),
        (&["sed", "-n", "-e", "1,5p", "f"], &["-n", "-e"]),
        (&["sed", "-ne", "$p", "f"], &["-n", "-e"]),
        (&["sed", "-ne1p", "f"], &["-n", "-e"]),
        (&["sed", "--quiet", "--expression=2~3p", "f"], &["-n", "-e"]),
        (&["sed", "-E", "-n", "/foo/p", "f"], &["-E", "-n"]),
        (&["sed", "-n", "-s", "1p", "a", "b"], &["-n", "-s"]),
        (&["sed", "-n", "1p"], &["-n"]),
        (&["sed", "-n", "--", "1p", "f"], &["-n"]),
        (&["sed", "--version"], &["--version"]),
    ];
    for (arguments, expected) in cases {
        assert_eq!(
            analyze(arguments),
            (true, expected.to_vec()),
            "{arguments:?}"
        );
    }
}

#[test]
fn sed_writing_executing_or_unknown_forms_are_incomplete() {
    for arguments in [
        vec!["sed", "-i", "1p", "f"],
        vec!["sed", "-i.bak", "1p", "f"],
        vec!["sed", "--in-place", "1p", "f"],
        vec!["sed", "-f", "script", "f"],
        vec!["sed", "--file=script", "f"],
        vec!["sed", "-z", "1p", "f"],
        vec!["sed", "-n", "w out", "f"],
        vec!["sed", "-n", "1p;w out", "f"],
        vec!["sed", "-n", "1p\nw out", "f"],
        vec!["sed", "-n", "1e id", "f"],
        vec!["sed", "-n", "r x", "f"],
        vec!["sed", "s/a/b/e", "f"],
        vec!["sed", "s/a/b/", "f"],
        vec!["sed", "-e", "w out", "f"],
        vec!["sed", "-n", "-e", "1p", "-e", "w out", "f"],
        vec!["sed", "-n", "{p}", "f"],
        vec!["sed", "y/a/b/", "f"],
        vec!["sed", "-n"],
        vec!["sed"],
        vec!["sed", "-e"],
        vec!["sed", "1p", "f", "-n"],
        vec!["sed", "-n", "garbage", "f"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn git_read_only_subcommands_are_recognized() {
    let cases: &[(&[&str], &[&str])] = &[
        (
            &["git", "ls-files", "--others", "--exclude-standard"],
            &["ls-files", "-o", "--exclude-standard"],
        ),
        (&["git", "ls-files", "--", "src"], &["ls-files"]),
        (
            &["git", "-C", "dir", "blame", "-L", "1,5", "f"],
            &["-C", "blame", "-L"],
        ),
        (&["git", "blame", "-L1,5", "--", "f"], &["blame", "-L"]),
        (
            &["git", "rev-parse", "--show-toplevel"],
            &["rev-parse", "--show-toplevel"],
        ),
        (
            &["git", "rev-parse", "--abbrev-ref", "HEAD"],
            &["rev-parse", "--abbrev-ref"],
        ),
        (
            &["git", "grep", "-n", "-e", "needle", "--", "src"],
            &["grep", "-n", "-e"],
        ),
        (&["git", "grep", "-A3", "needle"], &["grep", "-A"]),
        (&["git", "stash", "list"], &["stash", "list"]),
        (
            &["git", "stash", "list", "--oneline"],
            &["stash", "list", "--oneline"],
        ),
        (
            &["git", "stash", "show", "--stat", "stash@{0}"],
            &["stash", "show", "--stat"],
        ),
    ];
    for (arguments, expected) in cases {
        assert_eq!(
            analyze(arguments),
            (true, expected.to_vec()),
            "{arguments:?}"
        );
    }
}

#[test]
fn git_dangerous_or_unknown_forms_stay_incomplete() {
    for arguments in [
        vec!["git", "grep", "-O", "needle"],
        vec!["git", "grep", "--open-files-in-pager=vim", "needle"],
        vec!["git", "stash"],
        vec!["git", "stash", "push"],
        vec!["git", "stash", "pop"],
        vec!["git", "stash", "drop"],
        vec!["git", "stash", "--", "list"],
        vec!["git", "ls-files", "-x", "pattern"],
        vec!["git", "ls-files", "--exclude-from=file"],
        vec!["git", "blame", "--contents", "file", "f"],
        vec!["git", "blame", "--ignore-revs-file", "f", "x"],
        vec!["git", "blame", "-S", "revs", "x"],
        vec!["git", "rev-parse", "--parseopt"],
        vec!["git", "stash", "list", "--ylva-unknown"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn git_values_do_not_impersonate_stash_subcommands() {
    let analysis = parse(&command(&["git", "log", "stash", "list"]));
    assert_eq!(canonical(&analysis), vec!["log"]);
}

#[test]
fn only_grep_and_ripgrep_permute_options() {
    assert_eq!(
        analyze(&["grep", "needle", "-A8", "file"]),
        (true, vec!["-A"])
    );
    assert_eq!(
        analyze(&["grep", "-n", "some text", "-A40", "path"]),
        (true, vec!["-n", "-A"])
    );
    for name in ["cat", "ls", "sort", "head", "tail", "wc"] {
        assert!(!parse(&command(&[name, "file", "-n"])).complete, "{name}");
    }
}

#[test]
fn printf_accepts_only_the_option_terminator() {
    assert_eq!(analyze(&["printf", "%s\\n", "x"]), (true, vec![]));
    assert_eq!(analyze(&["printf", "--", "%s", "x"]), (true, vec![]));
    assert_eq!(
        analyze(&["printf", "%s\\n", "--- title ---"]),
        (true, vec![])
    );
    for arguments in [
        vec!["printf", "-v", "var", "%s", "x"],
        vec!["printf", "-x"],
        vec!["printf", "-v"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}

#[test]
fn cd_accepts_one_literal_operand_only() {
    for arguments in [
        vec!["cd", "foo"],
        vec!["cd", "--", "foo"],
        vec!["cd", "--", "-x"],
    ] {
        assert_eq!(analyze(&arguments), (true, vec![]), "{arguments:?}");
    }
    for arguments in [
        vec!["cd"],
        vec!["cd", "--"],
        vec!["cd", "-"],
        vec!["cd", "--", "-"],
        vec!["cd", "-P", "foo"],
        vec!["cd", "-L", "foo"],
        vec!["cd", "-x"],
        vec!["cd", "a", "b"],
        vec!["cd", "--", "a", "b"],
    ] {
        assert!(!parse(&command(&arguments)).complete, "{arguments:?}");
    }
}
