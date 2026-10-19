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

//! Pattern validation and filesystem traversal behind the `glob` tool.

use std::path::{Path, PathBuf};

use super::super::ToolError;
use super::result::{GlobCandidate, GlobMatchCounts, GlobResult};
use super::GlobInputArgs;

pub(super) fn pattern_root(pattern: &str) -> String {
    let first_glob_position = pattern
        .char_indices()
        .find(|(_, character)| matches!(character, '*' | '?' | '['))
        .map(|(index, _)| index);
    let Some(first_glob_position) = first_glob_position else {
        return ".".to_string();
    };
    let slash_position = pattern[..first_glob_position].rfind('/');
    match slash_position {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(position) => pattern[..position].to_string(),
    }
}

pub(super) fn normalize_glob_match_path(path: &Path) -> String {
    let path_string = path.display().to_string();
    if path_string == "." {
        String::new()
    } else if let Some(stripped_prefix) = path_string.strip_prefix("./") {
        stripped_prefix.to_string()
    } else {
        path_string
    }
}

pub(super) fn path_matches_type_filter(path: &Path, args: &GlobInputArgs) -> bool {
    let file_type = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        Err(_) => return false,
    };
    if args.only_files {
        return file_type.is_file();
    }
    if args.only_dirs {
        return file_type.is_dir();
    }
    true
}

pub(super) fn is_search_root(path: &Path, root: &str) -> bool {
    normalize_glob_match_path(path) == normalize_glob_match_path(Path::new(root))
}

/// Reject glob patterns that contain constructs the `glob` crate silently
/// misinterprets, giving the caller a clear error instead of empty results.
///
/// Currently detected:
/// - `!` outside a `[...]` character class: the `glob` crate treats it as a
///   literal character, so patterns like `!*.rs` or `**/!foo` never match
///   anything. Negation is not supported; the caller must filter results
///   programmatically instead.
pub(super) fn validate_glob_pattern(pattern: &str) -> Result<(), String> {
    let mut inside_bracket = false;
    for character in pattern.chars() {
        match character {
            '[' => inside_bracket = true,
            ']' => inside_bracket = false,
            '!' if !inside_bracket => {
                return Err(format!(
                    "invalid pattern '{pattern}': '!' outside '[...]' is not supported \
                     (the glob crate treats it as a literal character, so the pattern \
                     would never match). Negation is not supported; remove '!' and filter \
                     results after matching instead."
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) async fn glob_files(args: &GlobInputArgs) -> Result<GlobResult, ToolError> {
    if args.only_files && args.only_dirs {
        return Err(ToolError::failed(
            "glob",
            "arguments 'only_files' and 'only_dirs' cannot both be true",
        ));
    }
    if let Err(reason) = validate_glob_pattern(&args.pattern) {
        return Err(ToolError::failed("glob", reason));
    }
    tracing::debug!(pattern = %args.pattern, "glob searching");
    let root = pattern_root(&args.pattern);
    let candidate_paths: Vec<PathBuf> = if args.git_ignore {
        let compiled_pattern = glob::Pattern::new(&args.pattern).map_err(|error| {
            ToolError::failed(
                "glob",
                format!("invalid pattern '{}': {error}", args.pattern),
            )
        })?;
        let match_options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: args.dot_ignore,
        };
        let mut collected_paths: Vec<PathBuf> = Vec::new();
        let walker = ignore::WalkBuilder::new(&root)
            .git_ignore(true)
            .require_git(false)
            .hidden(args.dot_ignore)
            .build();
        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let entry_path = entry.path();
            let match_path = normalize_glob_match_path(entry_path);
            if is_search_root(entry_path, &root)
                || !compiled_pattern.matches_with(&match_path, match_options)
            {
                continue;
            }
            collected_paths.push(entry_path.to_path_buf());
        }
        collected_paths
    } else {
        let match_options = glob::MatchOptions {
            require_literal_leading_dot: args.dot_ignore,
            ..Default::default()
        };
        let all_matches: Vec<_> = glob::glob_with(&args.pattern, match_options)
            .map_err(|error| {
                ToolError::failed(
                    "glob",
                    format!("invalid pattern '{}': {error}", args.pattern),
                )
            })?
            .filter_map(Result::ok)
            .filter(|path| !is_search_root(path, &root))
            .collect();
        all_matches
    };

    let mut candidates: Vec<GlobCandidate> = candidate_paths
        .into_iter()
        .map(GlobCandidate::from_path)
        .collect();
    candidates.sort_by(|left, right| left.path.cmp(&right.path));

    let mut selected_candidates = Vec::new();
    for candidate in candidates {
        if path_matches_type_filter(&candidate.path, args) {
            selected_candidates.push(candidate);
        }
    }

    let mut truncated = false;
    if let Some(limit) = args.limit {
        if selected_candidates.len() > limit {
            selected_candidates.truncate(limit);
            truncated = true;
        }
    }

    let mut matches = GlobMatchCounts::default();
    let mut matched_paths = Vec::with_capacity(selected_candidates.len());
    let mut matched_path_kinds = Vec::with_capacity(selected_candidates.len());
    for candidate in selected_candidates {
        matches.add_path_kind(candidate.path_kind);
        matched_paths.push(candidate.path.display().to_string());
        matched_path_kinds.push(candidate.path_kind);
    }

    if args.only_count {
        matched_paths.clear();
    }
    tracing::debug!(matches = matches.total, truncated, "glob searched");
    Ok(GlobResult::new(
        matched_paths,
        matched_path_kinds,
        matches,
        truncated,
    ))
}
