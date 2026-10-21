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

use super::{flag, optional, required, OptionSpec};

// All provenance reviewed 2026-10-21. These are selected spellings, not
// exhaustive OS grammars. GNU long options are accepted only in full.
// BSD/GNU arity conflicts (notably ls -I/-T and tail -f) are omitted.
// Options after operands are incomplete except in ripgrep's documented CLI.
// https://www.gnu.org/software/coreutils/manual/html_node/Common-options.html
// https://www.gnu.org/software/coreutils/manual/html_node/cat-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=cat&sektion=1
// https://keith.github.io/xcode-man-pages/cat.1.html
const CAT: &[OptionSpec] = &[
    flag(&["-b", "--number-nonblank"]),
    flag(&["-n", "--number"]),
    flag(&["-s", "--squeeze-blank"]),
    flag(&["-v", "--show-nonprinting"]),
    flag(&["-E", "--show-ends"]),
    flag(&["-T", "--show-tabs"]),
    flag(&["-A", "--show-all"]),
    flag(&["-e"]),
    flag(&["-t"]),
    flag(&["-u"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

// https://www.gnu.org/software/coreutils/manual/html_node/ls-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=ls&sektion=1
// https://keith.github.io/xcode-man-pages/ls.1.html
const LS: &[OptionSpec] = &[
    flag(&["-a", "--all"]),
    flag(&["-A", "--almost-all"]),
    flag(&["-l"]),
    flag(&["-h", "--human-readable"]),
    flag(&["-d", "--directory"]),
    flag(&["-R", "--recursive"]),
    flag(&["-r", "--reverse"]),
    flag(&["-t"]),
    flag(&["-S"]),
    flag(&["-1"]),
    flag(&["-i", "--inode"]),
    flag(&["-s", "--size"]),
    flag(&["-L", "--dereference"]),
    flag(&["-H", "--dereference-command-line"]),
    flag(&["-F", "--classify"]),
    flag(&["-p"]),
    flag(&["-C"]),
    flag(&["-m"]),
    flag(&["-n", "--numeric-uid-gid"]),
    flag(&["-G"]),
    optional(&["--color"]),
    required(&["--sort"]),
    required(&["--time"]),
    required(&["--format"]),
    required(&["--block-size"]),
    required(&["--ignore"]),
    required(&["--hide"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

// https://www.gnu.org/software/coreutils/manual/html_node/head-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=head&sektion=1
// https://keith.github.io/xcode-man-pages/head.1.html
// First-argument digits-only -N is recognized as -n by parse_options.
const HEAD: &[OptionSpec] = &[
    required(&["-n", "--lines"]),
    required(&["-c", "--bytes"]),
    flag(&["-q", "--quiet", "--silent"]),
    flag(&["-v", "--verbose"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

// https://www.gnu.org/software/coreutils/manual/html_node/tail-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=tail&sektion=1
// https://keith.github.io/xcode-man-pages/tail.1.html
// GNU -f has an optional attached value, BSD -f does not: only the
// unambiguous long --follow spelling is admitted.
// First-argument digits-only -N is recognized as -n by parse_options.
const TAIL: &[OptionSpec] = &[
    required(&["-n", "--lines"]),
    required(&["-c", "--bytes"]),
    optional(&["--follow"]),
    flag(&["-F"]),
    flag(&["--retry"]),
    required(&["--pid"]),
    required(&["--sleep-interval"]),
    flag(&["-q", "--quiet", "--silent"]),
    flag(&["-v", "--verbose"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

// https://www.gnu.org/software/coreutils/manual/html_node/wc-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=wc&sektion=1
// https://keith.github.io/xcode-man-pages/wc.1.html
const WC: &[OptionSpec] = &[
    flag(&["-c", "--bytes"]),
    flag(&["-m", "--chars"]),
    flag(&["-l", "--lines"]),
    flag(&["-w", "--words"]),
    flag(&["-L", "--max-line-length"]),
    required(&["--files0-from"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

// https://www.gnu.org/software/grep/manual/html_node/Command_002dline-Options.html
// https://man.freebsd.org/cgi/man.cgi?query=grep&sektion=1
// https://keith.github.io/xcode-man-pages/grep.1.html
const GREP: &[OptionSpec] = &[
    flag(&["-E", "--extended-regexp"]),
    flag(&["-F", "--fixed-strings"]),
    flag(&["-G", "--basic-regexp"]),
    flag(&["-i", "--ignore-case"]),
    flag(&["-v", "--invert-match"]),
    flag(&["-w", "--word-regexp"]),
    flag(&["-x", "--line-regexp"]),
    flag(&["-n", "--line-number"]),
    flag(&["-H", "--with-filename"]),
    flag(&["-h", "--no-filename"]),
    flag(&["-r", "--recursive"]),
    flag(&["-R", "--dereference-recursive"]),
    flag(&["-l", "--files-with-matches"]),
    flag(&["-L", "--files-without-match"]),
    flag(&["-c", "--count"]),
    flag(&["-o", "--only-matching"]),
    flag(&["-q", "--quiet", "--silent"]),
    flag(&["-s", "--no-messages"]),
    flag(&["-a", "--text"]),
    flag(&["-I"]),
    required(&["-e", "--regexp"]),
    required(&["-f", "--file"]),
    required(&["-m", "--max-count"]),
    required(&["-A", "--after-context"]),
    required(&["-B", "--before-context"]),
    // Leave the legacy short -C forms outside this conservative subset.
    required(&["--context"]),
    required(&["--include"]),
    required(&["--exclude"]),
    required(&["--exclude-dir"]),
    optional(&["--color", "--colour"]),
    flag(&["--help"]),
    flag(&["-V", "--version"]),
];

// ripgrep 14.1.1; unlike BSD getopt, its parser accepts interspersed options.
// https://github.com/BurntSushi/ripgrep/blob/14.1.1/crates/core/flags/defs.rs
// https://github.com/BurntSushi/ripgrep/blob/14.1.1/crates/core/flags/parse.rs
const RIPGREP: &[OptionSpec] = &[
    flag(&["-i", "--ignore-case"]),
    flag(&["-s", "--case-sensitive"]),
    flag(&["-S", "--smart-case"]),
    flag(&["-F", "--fixed-strings"]),
    flag(&["-v", "--invert-match"]),
    flag(&["-w", "--word-regexp"]),
    flag(&["-x", "--line-regexp"]),
    flag(&["-n", "--line-number"]),
    flag(&["-N", "--no-line-number"]),
    flag(&["-l", "--files-with-matches"]),
    flag(&["--files-without-match"]),
    flag(&["--files"]),
    flag(&["--type-list"]),
    flag(&["-c", "--count"]),
    flag(&["--count-matches"]),
    flag(&["-o", "--only-matching"]),
    flag(&["-q", "--quiet"]),
    flag(&["--hidden"]),
    flag(&["-u", "--unrestricted"]),
    flag(&["-L", "--follow"]),
    flag(&["-a", "--text"]),
    flag(&["--no-config"]),
    flag(&["--no-ignore"]),
    flag(&["--no-heading"]),
    flag(&["--heading"]),
    flag(&["--column"]),
    flag(&["--json"]),
    flag(&["-0", "--null"]),
    flag(&["--null-data"]),
    flag(&["-P", "--pcre2"]),
    flag(&["-U", "--multiline"]),
    required(&["-e", "--regexp"]),
    required(&["-f", "--file"]),
    required(&["-g", "--glob"]),
    required(&["--iglob"]),
    required(&["-t", "--type"]),
    required(&["-T", "--type-not"]),
    required(&["-m", "--max-count"]),
    required(&["-A", "--after-context"]),
    required(&["-B", "--before-context"]),
    required(&["-C", "--context"]),
    required(&["--color"]),
    required(&["--colors"]),
    required(&["--pre"]),
    required(&["--pre-glob"]),
    required(&["-r", "--replace"]),
    required(&["--sort"]),
    required(&["--sortr"]),
    required(&["--max-depth"]),
    required(&["--encoding", "-E"]),
    required(&["--hostname-bin"]),
    flag(&["--no-pre"]),
    flag(&["-h", "--help"]),
    flag(&["-V", "--version"]),
];

// https://www.gnu.org/software/coreutils/manual/html_node/sort-invocation.html
// https://man.freebsd.org/cgi/man.cgi?query=sort&sektion=1
// https://keith.github.io/xcode-man-pages/sort.1.html
const SORT: &[OptionSpec] = &[
    flag(&["-b", "--ignore-leading-blanks"]),
    flag(&["-d", "--dictionary-order"]),
    flag(&["-f", "--ignore-case"]),
    flag(&["-g", "--general-numeric-sort"]),
    flag(&["-i", "--ignore-nonprinting"]),
    flag(&["-M", "--month-sort"]),
    flag(&["-n", "--numeric-sort"]),
    flag(&["-r", "--reverse"]),
    flag(&["-s", "--stable"]),
    flag(&["-u", "--unique"]),
    flag(&["-m", "--merge"]),
    flag(&["-c", "--check"]),
    flag(&["-C"]),
    required(&["-k", "--key"]),
    required(&["-t", "--field-separator"]),
    required(&["-o", "--output"]),
    required(&["-S", "--buffer-size"]),
    required(&["-T", "--temporary-directory"]),
    required(&["--compress-program"]),
    required(&["--files0-from"]),
    flag(&["--help"]),
    flag(&["--version"]),
];

pub(super) fn options(name: &str) -> Option<&'static [OptionSpec]> {
    match name {
        "cat" => Some(CAT),
        "ls" => Some(LS),
        "head" => Some(HEAD),
        "tail" => Some(TAIL),
        "wc" => Some(WC),
        "grep" => Some(GREP),
        "rg" => Some(RIPGREP),
        "sort" => Some(SORT),
        _ => None,
    }
}
