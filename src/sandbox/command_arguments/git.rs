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

use super::{
    consume_option, equals, flag, is_numeric_count_option, optional, required, ArgumentAnalysis,
    ArgvToken, OptionSpec,
};

// Reviewed 2026-10-21 against the Git manual (2.x). Explicit subcommand
// scopes prevent global option values and path/revision operands from
// masquerading as subcommands. Numeric -N is recognized for log only.
// Deliberately omit short bundles and ambiguous optional-short-value forms
// (-M/-C/-U).
// https://git-scm.com/docs/git
// https://git-scm.com/docs/gitcli
const GLOBAL: &[OptionSpec] = &[
    required(&["-c"]),
    required(&["-C"]),
    equals(&["--config-env"]),
    required(&["--git-dir"]),
    required(&["--work-tree"]),
    required(&["--namespace"]),
    optional(&["--exec-path"]),
    flag(&["-p", "--paginate"]),
    flag(&["-P", "--no-pager"]),
    flag(&["--no-replace-objects"]),
    flag(&["--bare"]),
    flag(&["--literal-pathspecs"]),
    flag(&["--glob-pathspecs"]),
    flag(&["--noglob-pathspecs"]),
    flag(&["--icase-pathspecs"]),
    flag(&["--no-optional-locks"]),
    equals(&["--attr-source"]),
    flag(&["--no-lazy-fetch"]),
    flag(&["--no-advice"]),
];

// https://git-scm.com/docs/git-check-ignore
// Explicit path operands only; stdin and other reporting modes stay unsupported.
const CHECK_IGNORE: &[OptionSpec] = &[flag(&["-v", "--verbose"])];

// https://git-scm.com/docs/git-status
const STATUS: &[OptionSpec] = &[
    flag(&["-s", "--short"]),
    flag(&["-b", "--branch"]),
    flag(&["--show-stash"]),
    optional(&["--porcelain"]),
    flag(&["--long"]),
    flag(&["-v", "--verbose"]),
    optional(&["-u", "--untracked-files"]),
    optional(&["--ignore-submodules"]),
    optional(&["--ignored"]),
    flag(&["-z", "--null"]),
    flag(&["--no-renames"]),
    flag(&["--renames"]),
    optional(&["--find-renames"]),
    optional(&["--column"]),
    flag(&["--no-column"]),
    flag(&["--ahead-behind"]),
    flag(&["--no-ahead-behind"]),
];

// Shared diff-output grammar for log/diff/show only, never git status.
// https://git-scm.com/docs/git-diff
// https://git-scm.com/docs/diff-options
const DIFF_OUTPUT: &[OptionSpec] = &[
    flag(&["-p", "-u", "--patch"]),
    flag(&["-s", "--no-patch"]),
    flag(&["--raw"]),
    flag(&["--patch-with-raw"]),
    flag(&["--numstat"]),
    flag(&["--shortstat"]),
    optional(&["--stat"]),
    flag(&["--name-only"]),
    flag(&["--name-status"]),
    flag(&["--summary"]),
    flag(&["--check"]),
    flag(&["--binary"]),
    flag(&["--full-index"]),
    flag(&["--no-color"]),
    optional(&["--color"]),
    flag(&["--no-ext-diff"]),
    flag(&["--ext-diff"]),
    flag(&["--no-textconv"]),
    flag(&["--textconv"]),
    required(&["--output"]),
    required(&["--diff-algorithm"]),
    required(&["--src-prefix"]),
    required(&["--dst-prefix"]),
    flag(&["--no-prefix"]),
    flag(&["-w", "--ignore-all-space"]),
    flag(&["-b", "--ignore-space-change"]),
    flag(&["--ignore-space-at-eol"]),
    flag(&["--ignore-blank-lines"]),
    flag(&["--minimal"]),
    flag(&["--patience"]),
    flag(&["--histogram"]),
    flag(&["-a", "--text"]),
    flag(&["-R"]),
    flag(&["--exit-code"]),
    flag(&["--quiet"]),
    flag(&["-z"]),
    optional(&["--abbrev"]),
    optional(&["--find-renames"]),
    optional(&["--find-copies"]),
    flag(&["--no-renames"]),
    flag(&["--find-copies-harder"]),
    optional(&["--word-diff"]),
    required(&["--word-diff-regex"]),
    required(&["--unified"]),
    required(&["--inter-hunk-context"]),
];

// https://git-scm.com/docs/git-diff
const DIFF: &[OptionSpec] = &[
    flag(&["--cached", "--staged"]),
    flag(&["--no-index"]),
    optional(&["--relative"]),
    required(&["-S"]),
    required(&["-G"]),
    flag(&["--pickaxe-all"]),
    flag(&["--pickaxe-regex"]),
    required(&["--diff-filter"]),
    required(&["-O"]),
    required(&["--rotate-to"]),
    required(&["--skip-to"]),
];

// https://git-scm.com/docs/git-log
// https://git-scm.com/docs/git-show
// https://git-scm.com/docs/rev-list-options
const HISTORY: &[OptionSpec] = &[
    flag(&["--oneline"]),
    optional(&["--pretty"]),
    equals(&["--format"]),
    optional(&["--decorate"]),
    flag(&["--no-decorate"]),
    flag(&["--graph"]),
    flag(&["--all"]),
    optional(&["--branches"]),
    optional(&["--tags"]),
    optional(&["--remotes"]),
    flag(&["--no-walk"]),
    flag(&["--first-parent"]),
    flag(&["--merges"]),
    flag(&["--no-merges"]),
    flag(&["--reverse"]),
    flag(&["--date-order"]),
    flag(&["--topo-order"]),
    flag(&["--follow"]),
    flag(&["--no-notes"]),
    optional(&["--notes"]),
    required(&["-n", "--max-count"]),
    required(&["--skip"]),
    required(&["--since", "--after"]),
    required(&["--until", "--before"]),
    required(&["--author"]),
    required(&["--committer"]),
    required(&["--grep"]),
    required(&["--date"]),
    required(&["--encoding"]),
    required(&["--max-parents"]),
    required(&["--min-parents"]),
];

pub(super) fn parse(arguments: &[ArgvToken], analysis: &mut ArgumentAnalysis) {
    let mut position = 1;
    while position < arguments.len() && arguments[position].text.starts_with('-') {
        let text = arguments[position].text.as_str();
        // git's top-level parser is not a general short-bundle parser.
        if !valid_global_spelling(text)
            || !consume_option(arguments, &mut position, GLOBAL, analysis)
        {
            analysis.complete = false;
            return;
        }
    }
    let Some(token) = arguments.get(position) else {
        analysis.complete = false;
        return;
    };
    let subcommand = match token.text.as_str() {
        "check-ignore" => &["check-ignore"][..],
        "status" => &["status"][..],
        "log" => &["log"][..],
        "diff" => &["diff"][..],
        "show" => &["show"][..],
        _ => {
            analysis.complete = false;
            return;
        }
    };
    analysis.identify(subcommand);
    position += 1;
    let mut terminated = false;
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        if !terminated && text == "--" {
            terminated = true;
            position += 1;
        } else if !terminated && subcommand[0] == "log" && is_numeric_count_option(text) {
            analysis.identify(&["-n", "--max-count"]);
            analysis.option_values.push(position);
            position += 1;
        } else if !terminated && text.starts_with('-') && text != "-" {
            let Some(options) = scoped_options(subcommand[0], text) else {
                analysis.complete = false;
                return;
            };
            if !consume_option(arguments, &mut position, options, analysis) {
                analysis.complete = false;
                return;
            }
        } else {
            analysis.operands.push(position);
            position += 1;
        }
    }
    if subcommand[0] == "check-ignore" && analysis.operands.is_empty() {
        analysis.complete = false;
    }
}

fn valid_global_spelling(text: &str) -> bool {
    text.starts_with("--")
        || GLOBAL.iter().any(|option| option.aliases.contains(&text))
        || (text.len() > 2 && (text.starts_with("-c") || text.starts_with("-C")))
}

fn scoped_options(subcommand: &str, text: &str) -> Option<&'static [OptionSpec]> {
    let spelling = text.split_once('=').map_or(text, |(name, _)| name);
    let candidates = match subcommand {
        "check-ignore" => &[CHECK_IGNORE][..],
        "status" => &[STATUS][..],
        "diff" => &[DIFF, DIFF_OUTPUT][..],
        "log" | "show" => &[HISTORY, DIFF_OUTPUT][..],
        _ => return None,
    };
    candidates.iter().copied().find(|options| {
        options
            .iter()
            .any(|option| option.aliases.contains(&spelling))
            || (!text.starts_with("--")
                && options.iter().any(|option| {
                    option.aliases.iter().any(|alias| {
                        alias.len() == 2
                            && text.starts_with(alias)
                            && matches!(
                                option.arity,
                                super::Arity::Required | super::Arity::OptionalAttached
                            )
                    })
                }))
    })
}
