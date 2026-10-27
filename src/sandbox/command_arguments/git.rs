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

// https://git-scm.com/docs/git-ls-files (reviewed 2026-10-21)
// Omitted: -x/--exclude, -X/--exclude-from, --exclude-per-directory (read
// files), --recurse-submodules, --sparse.
const LS_FILES: &[OptionSpec] = &[
    flag(&["-c", "--cached"]),
    flag(&["-o", "--others"]),
    flag(&["-i", "--ignored"]),
    flag(&["-m", "--modified"]),
    flag(&["-d", "--deleted"]),
    flag(&["-s", "--stage"]),
    flag(&["-u", "--unmerged"]),
    flag(&["-z"]),
    flag(&["-t"]),
    flag(&["-v"]),
    flag(&["--exclude-standard"]),
    flag(&["--full-name"]),
    flag(&["--error-unmatch"]),
    flag(&["--deduplicate"]),
    flag(&["--eol"]),
    optional(&["--abbrev"]),
];

// https://git-scm.com/docs/git-blame (reviewed 2026-10-21)
// Omitted: --contents, -S, --ignore-revs-file (read files); -M/-C stay
// unsupported like for diff.
const BLAME: &[OptionSpec] = &[
    required(&["-L"]),
    flag(&["-l"]),
    flag(&["-t"]),
    flag(&["-s"]),
    flag(&["-e", "--show-email"]),
    flag(&["-w"]),
    flag(&["-p", "--porcelain"]),
    flag(&["--line-porcelain"]),
    flag(&["--incremental"]),
    flag(&["-f", "--show-name"]),
    flag(&["-n", "--show-number"]),
];

// https://git-scm.com/docs/git-rev-parse (reviewed 2026-10-21)
// Omitted: --parseopt, --sq-quote and other stdin modes.
const REV_PARSE: &[OptionSpec] = &[
    flag(&["--git-dir"]),
    flag(&["--show-toplevel"]),
    flag(&["--show-cdup"]),
    flag(&["--show-prefix"]),
    flag(&["--is-inside-work-tree"]),
    flag(&["--is-bare-repository"]),
    optional(&["--abbrev-ref"]),
    optional(&["--short"]),
    flag(&["--verify"]),
    flag(&["--quiet"]),
    flag(&["--symbolic"]),
    flag(&["--symbolic-full-name"]),
];

// https://git-scm.com/docs/git-grep (reviewed 2026-10-21)
// Omitted: -O/--open-files-in-pager (runs a program), -f (reads a file).
const GREP: &[OptionSpec] = &[
    flag(&["-n", "--line-number"]),
    flag(&["-i", "--ignore-case"]),
    flag(&["-v", "--invert-match"]),
    flag(&["-w", "--word-regexp"]),
    flag(&["-F", "--fixed-strings"]),
    flag(&["-E", "--extended-regexp"]),
    required(&["-e"]),
    flag(&["-l", "--files-with-matches"]),
    flag(&["-L", "--files-without-match"]),
    flag(&["-c", "--count"]),
    required(&["-A", "--after-context"]),
    required(&["-B", "--before-context"]),
    required(&["-C", "--context"]),
    flag(&["--no-index"]),
    flag(&["--untracked"]),
    flag(&["--cached"]),
    flag(&["-I"]),
    flag(&["-a", "--text"]),
    flag(&["-h"]),
    flag(&["-H"]),
    required(&["--max-depth"]),
    flag(&["-q", "--quiet"]),
    flag(&["-o", "--only-matching"]),
    flag(&["--heading"]),
    flag(&["--break"]),
    flag(&["-p", "--show-function"]),
];

struct Subcommand {
    name: &'static str,
    identity: &'static [&'static str],
    scopes: &'static [&'static [OptionSpec]],
    /// Required nested subcommands (e.g. `stash list`); `None` when not nested.
    nested: Option<&'static [Subcommand]>,
}

const fn subcommand(
    name: &'static str,
    identity: &'static [&'static str],
    scopes: &'static [&'static [OptionSpec]],
) -> Subcommand {
    Subcommand {
        name,
        identity,
        scopes,
        nested: None,
    }
}

// Single table of supported subcommands: identity and option scopes.
const SUBCOMMANDS: &[Subcommand] = &[
    subcommand("check-ignore", &["check-ignore"], &[CHECK_IGNORE]),
    subcommand("status", &["status"], &[STATUS]),
    subcommand("log", &["log"], &[HISTORY, DIFF_OUTPUT]),
    subcommand("diff", &["diff"], &[DIFF, DIFF_OUTPUT]),
    subcommand("show", &["show"], &[HISTORY, DIFF_OUTPUT]),
    subcommand("ls-files", &["ls-files"], &[LS_FILES]),
    subcommand("blame", &["blame"], &[BLAME]),
    subcommand("rev-parse", &["rev-parse"], &[REV_PARSE]),
    subcommand("grep", &["grep"], &[GREP]),
    Subcommand {
        name: "stash",
        identity: &["stash"],
        scopes: &[],
        nested: Some(&[
            subcommand("list", &["list"], &[HISTORY, DIFF_OUTPUT]),
            subcommand("show", &["show"], &[DIFF_OUTPUT]),
        ]),
    },
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
    let Some(subcommand) = SUBCOMMANDS
        .iter()
        .find(|subcommand| subcommand.name == token.text)
    else {
        analysis.complete = false;
        return;
    };
    analysis.identify(subcommand.identity);
    position += 1;
    let mut scopes = subcommand.scopes;
    if let Some(nested) = subcommand.nested {
        // Bare `git stash` is `push`; only read-only nested forms are known.
        let nested_name = arguments.get(position).map(|nested| nested.text.as_str());
        let Some(nested_subcommand) = nested
            .iter()
            .find(|candidate| Some(candidate.name) == nested_name)
        else {
            analysis.complete = false;
            return;
        };
        analysis.identify(nested_subcommand.identity);
        scopes = nested_subcommand.scopes;
        position += 1;
    }
    let mut terminated = false;
    while position < arguments.len() {
        let text = arguments[position].text.as_str();
        if !terminated && text == "--" {
            terminated = true;
            position += 1;
        } else if !terminated && subcommand.identity[0] == "log" && is_numeric_count_option(text) {
            analysis.identify(&["-n", "--max-count"]);
            analysis.option_values.push(position);
            position += 1;
        } else if !terminated && text.starts_with('-') && text != "-" {
            let Some(options) = scoped_options(scopes, text) else {
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
    if subcommand.identity[0] == "check-ignore" && analysis.operands.is_empty() {
        analysis.complete = false;
    }
}

fn valid_global_spelling(text: &str) -> bool {
    text.starts_with("--")
        || GLOBAL.iter().any(|option| option.aliases.contains(&text))
        || (text.len() > 2 && (text.starts_with("-c") || text.starts_with("-C")))
}

fn scoped_options(scopes: &[&'static [OptionSpec]], text: &str) -> Option<&'static [OptionSpec]> {
    let spelling = text.split_once('=').map_or(text, |(name, _)| name);
    let candidates = scopes;
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
