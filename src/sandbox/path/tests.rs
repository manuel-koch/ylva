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
use crate::sandbox::rules::CROSS_FILESYSTEM_REASON;
use crate::sandbox::tests::{make_sandbox, make_visible_temporary_directory};
use crate::sandbox::{ApprovalVerdictConfig, Sandbox, SandboxRule, SandboxRuleConfig};
use std::sync::{Arc, RwLock};

#[test]
fn dot_path_matches_hidden_components_independently_of_location() {
    let context = SandboxContext::new(make_sandbox(Path::new("/workspace/service")))
        .with_configured_rules(vec![SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: Some("hidden path".into()),
            target: None,
        }])
        .expect("valid sandbox fixture");

    for path in [
        ".env",
        "../.env",
        "/workspace/.hidden/config",
        "/elsewhere/.env",
        "/.hidden/workspace/config",
    ] {
        assert_eq!(
            context.check_path(Path::new(path)),
            Some(ApprovalCheck::AutoDenied {
                reason: "hidden path".into(),
            }),
            "{path}"
        );
        let normalized =
            SandboxContext::normalize(Path::new("/workspace/service"), Path::new(path));
        assert_eq!(
            context.check_snapshot_path(&normalized),
            Some(ApprovalCheck::AutoDenied {
                reason: "hidden path".into(),
            })
        );
        assert_eq!(
            context.check_path_allowing_dot_paths(Path::new(path)),
            None,
            "enumeration must retain its dot-path exception: {path}"
        );
    }
    for path in [
        "file.txt",
        "../file.txt",
        "/elsewhere/file.txt",
        "./file.txt",
    ] {
        assert_eq!(context.check_path(Path::new(path)), None, "{path}");
    }
}

#[test]
fn dot_path_default_floor_protects_hidden_launch_ancestors() {
    let context = SandboxContext::new(make_sandbox(Path::new("/workspace/.hidden/service")));
    assert_eq!(
        context.check_path(Path::new("config")),
        Some(ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(
        context.check_path_allowing_dot_paths(Path::new("config")),
        None
    );
}

#[test]
fn outside_workspace_uses_discovered_workspace_without_rebasing_other_checks() {
    for marker in [crate::GIT_ENTRY_NAME, ".ylva"] {
        let temporary_directory = make_visible_temporary_directory().expect("temporary directory");
        let workspace = temporary_directory.path().join("workspace");
        let launch_directory = workspace.join("service");
        std::fs::create_dir_all(&launch_directory).expect("launch directory");
        if marker == crate::GIT_ENTRY_NAME {
            std::fs::write(workspace.join(marker), "gitdir: elsewhere").expect("git marker");
        } else {
            std::fs::create_dir(workspace.join(marker)).expect("configuration marker");
        }
        let sandbox = make_sandbox(&launch_directory);
        let context = SandboxContext::new(Arc::clone(&sandbox));
        let configured_context = context
            .clone()
            .with_configured_rules(vec![SandboxRuleConfig::Named {
                rule: NamedSandboxRule::OutsideWorkspace,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: Some("outside workspace".into()),
                target: None,
            }])
            .expect("valid sandbox fixture");

        for path in [
            workspace.join("README.md"),
            workspace.join("sibling/new.txt"),
        ] {
            assert_eq!(context.check_path(&path), None, "{marker}: {path:?}");
            let canonical_path = SandboxContext::canonicalize_best_effort(&path);
            assert_eq!(context.check_snapshot_path(&canonical_path), None);
            assert_eq!(configured_context.check_path(&path), None);
        }
        assert_eq!(context.check_path(Path::new("../README.md")), None);
        assert_eq!(
            context.check_path(Path::new("../../outside.txt")),
            Some(ApprovalCheck::UserApprovalRequired)
        );
        assert_eq!(
            configured_context.check_path(Path::new("../../outside.txt")),
            Some(ApprovalCheck::AutoDenied {
                reason: "outside workspace".into(),
            })
        );
        assert_eq!(
            context.check_path(Path::new(".env")),
            Some(ApprovalCheck::UserApprovalRequired),
            "hidden files inside the launch directory require approval"
        );
        assert_eq!(
            context.check_path(Path::new("../.env")),
            Some(ApprovalCheck::UserApprovalRequired),
            "hidden files elsewhere in the workspace require approval"
        );
        let launch_directory = sandbox.read().expect("sandbox lock").sandbox_root.clone();
        assert_eq!(
            SandboxContext::normalize(&launch_directory, Path::new("README.md")),
            launch_directory.join("README.md"),
            "relative path resolution must remain launch-relative"
        );
    }
}

#[test]
fn outside_workspace_prefers_nearest_marker_and_falls_back_to_launch_directory() {
    let temporary_directory = make_visible_temporary_directory().expect("temporary directory");
    let workspace = temporary_directory.path().join("workspace");
    let nested_workspace = workspace.join("nested");
    let launch_directory = nested_workspace.join("service");
    std::fs::create_dir_all(&launch_directory).expect("launch directory");
    std::fs::create_dir(workspace.join(".ylva")).expect("outer marker");
    std::fs::write(nested_workspace.join(crate::GIT_ENTRY_NAME), "").expect("inner marker");
    let context = SandboxContext::new(make_sandbox(&launch_directory));
    assert_eq!(context.check_path(Path::new("../README.md")), None);
    assert_eq!(
        context.check_path(Path::new("../../README.md")),
        Some(ApprovalCheck::UserApprovalRequired),
        "a nearer git marker must win over an outer configuration marker"
    );

    let unmarked_launch = temporary_directory.path().join("unmarked/service");
    std::fs::create_dir_all(&unmarked_launch).expect("unmarked launch directory");
    assert_eq!(
        crate::config::layered::discover_workspace_root(&unmarked_launch),
        None
    );
    let context = SandboxContext::new(make_sandbox(&unmarked_launch));
    assert_eq!(context.check_path(Path::new("README.md")), None);
    assert_eq!(
        context.check_path(Path::new("../README.md")),
        Some(ApprovalCheck::UserApprovalRequired)
    );
}

#[cfg(unix)]
#[test]
fn outside_workspace_resolves_symlinks_against_discovered_boundary() {
    let temporary_directory = make_visible_temporary_directory().expect("temporary directory");
    let workspace = temporary_directory.path().join("workspace");
    let launch_directory = workspace.join("service");
    std::fs::create_dir_all(&launch_directory).expect("launch directory");
    std::fs::create_dir(workspace.join(".ylva")).expect("workspace marker");
    std::fs::write(workspace.join("README.md"), "inside").expect("workspace file");
    let outside_directory = temporary_directory.path().join("outside");
    std::fs::create_dir(&outside_directory).expect("outside directory");
    std::os::unix::fs::symlink(workspace.join("README.md"), launch_directory.join("inside"))
        .expect("inside symlink");
    std::os::unix::fs::symlink(outside_directory, launch_directory.join("outside"))
        .expect("outside symlink");
    let context = SandboxContext::new(make_sandbox(&launch_directory));
    assert_eq!(context.check_path(Path::new("inside")), None);
    assert_eq!(
        context.check_path(Path::new("outside/new.txt")),
        Some(ApprovalCheck::UserApprovalRequired),
        "missing targets beneath an escaping symlink must remain outside"
    );
}

#[test]
fn check_path_outside_workspace_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().join("workspace");
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let outside = temporary_dir.path().join("other/file.rs");
    assert_eq!(
        context.check_path(&outside),
        Some(ApprovalCheck::UserApprovalRequired),
        "path outside workspace should be Some(UserApprovalRequired)"
    );
}

#[test]
fn check_path_dot_path_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let dot_path = workspace.join(".env");
    assert_eq!(
        context.check_path(&dot_path),
        Some(ApprovalCheck::UserApprovalRequired),
        "dot-path relative to workspace should be Some(UserApprovalRequired)"
    );
    let dot_dir_path = workspace.join(crate::GIT_ENTRY_NAME).join("config");
    assert_eq!(
        context.check_path(&dot_dir_path),
        Some(ApprovalCheck::UserApprovalRequired),
        "dot-directory relative to workspace should be Some(UserApprovalRequired)"
    );
}

#[test]
fn check_path_allowing_dot_paths_abstains_inside_workspace() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let dot_dir_path = workspace.join(crate::GIT_ENTRY_NAME).join("config");
    assert_eq!(
        context.check_path_allowing_dot_paths(&dot_dir_path),
        None,
        "dot-directory enumeration inside the workspace should abstain"
    );
}

#[test]
fn check_path_normal_inside_workspace_returns_none() {
    // No rule fires for a plain in-workspace path: sandbox abstains.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let path = workspace.join("src/main.rs");
    assert_eq!(
        context.check_path(&path),
        None,
        "normal workspace path: sandbox abstains (None)"
    );
}

#[test]
fn normalize_collapses_dot_dot_components() {
    let base = Path::new("/workspace");
    let path = Path::new("src/../README.md");
    let normalized = SandboxContext::normalize(base, path);
    assert_eq!(normalized, PathBuf::from("/workspace/README.md"));
}

#[test]
fn normalize_collapses_current_dir_component() {
    let base = Path::new("/workspace");
    let path = Path::new("./src/./main.rs");
    let normalized = SandboxContext::normalize(base, path);
    assert_eq!(normalized, PathBuf::from("/workspace/src/main.rs"));
}

#[test]
fn normalize_does_not_pop_past_root() {
    // A `..` that cannot pop (root) is left in place.
    let base = Path::new("/");
    let path = Path::new("/../file.txt");
    let normalized = SandboxContext::normalize(base, path);
    // The `..` at root is a no-op; normalized stays at /../file.txt level.
    assert!(normalized.ends_with("file.txt"));
}

#[test]
fn check_path_glob_rule_overrides_to_explicit_verdict() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    // Build the glob pattern from the canonical sandbox_root stored in
    // the sandbox, not from the raw `workspace` variable, so that the
    // pattern matches the canonical `effective` path inside check_path.
    let canonical_sandbox_root = sandbox.read().unwrap().sandbox_root.clone();
    let allow_pattern = glob::Pattern::new(&format!(
        "{}/allowed/**",
        canonical_sandbox_root.to_string_lossy()
    ))
    .unwrap();
    let mut context = SandboxContext::new(Arc::clone(&sandbox));
    context.rules.push(SandboxRule {
        verdict: ApprovalCheck::AutoApproved,
        pattern: allow_pattern,
    });

    // Glob rule fires → Some(AutoApproved).
    let allowed_path = workspace.join("allowed/file.txt");
    assert_eq!(
        context.check_path(&allowed_path),
        Some(ApprovalCheck::AutoApproved),
        "glob rule should produce Some(AutoApproved)"
    );

    // No rule fires for this path → sandbox abstains.
    let normal_path = workspace.join("src/main.rs");
    assert_eq!(
        context.check_path(&normal_path),
        None,
        "unmatched path: sandbox abstains (None)"
    );
}

#[cfg(unix)]
#[test]
fn symlink_escaping_workspace_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let outside = temporary_dir.path().join("secret.txt");
    std::fs::write(&outside, "sensitive").unwrap();

    // A workspace-relative symlink whose target lives outside the workspace.
    let link = workspace.join("innocent.txt");
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    assert_eq!(
        context.check_path(&link),
        Some(ApprovalCheck::UserApprovalRequired),
        "symlink escaping the workspace must require user approval"
    );
}

#[cfg(unix)]
#[test]
fn symlink_to_dot_file_inside_workspace_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let real_dot_file = workspace.join(".env");
    std::fs::write(&real_dot_file, "SECRET=x").unwrap();

    // A non-dot symlink name pointing at a dot-file inside the workspace.
    let link = workspace.join("config.txt");
    std::os::unix::fs::symlink(&real_dot_file, &link).unwrap();

    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    assert_eq!(
        context.check_path(&link),
        Some(ApprovalCheck::UserApprovalRequired),
        "symlink whose real target is a dot-file must require user approval"
    );
}

#[cfg(unix)]
#[test]
fn symlink_parent_traversal_out_of_workspace_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().join("workspace");
    let outside = temporary_dir.path().join("outside");
    let nested = outside.join("nested");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(outside.join("id_rsa"), "sensitive").unwrap();
    std::os::unix::fs::symlink(&nested, workspace.join("link")).unwrap();

    let context = SandboxContext::new(make_sandbox(&workspace));
    // Existing and not-yet-existing targets resolve `link/..` physically.
    for name in ["id_rsa", "new_file"] {
        assert_eq!(
            context.check_path(Path::new(&format!("link/../{name}"))),
            Some(ApprovalCheck::UserApprovalRequired),
            "link/../{name} opens outside the workspace"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_parent_traversal_into_dot_directory_requires_user_approval() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let dot_directory = workspace.join(".secrets");
    let nested = dot_directory.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(dot_directory.join("token"), "sensitive").unwrap();
    std::os::unix::fs::symlink(&nested, workspace.join("link")).unwrap();

    let context = SandboxContext::new(make_sandbox(&workspace));
    assert_eq!(
        context.check_path(Path::new("link/../token")),
        Some(ApprovalCheck::UserApprovalRequired),
        "link/../token opens inside a dot directory"
    );
}

#[test]
fn to_relative_path_rewrites_absolute_path_inside_base() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
    std::fs::create_dir_all(base.join("src")).unwrap();
    let absolute = base.join("src/main.rs");

    assert_eq!(
        to_relative_path(absolute.to_str().unwrap(), &base),
        Some("src/main.rs".to_string())
    );
}

#[test]
fn to_relative_path_rewrites_absolute_glob_pattern_inside_base() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
    std::fs::create_dir_all(base.join("src")).unwrap();
    let pattern = format!("{}/src/**/*.rs", base.display());

    assert_eq!(
        to_relative_path(&pattern, &base),
        Some("src/**/*.rs".to_string())
    );
}

#[test]
fn to_relative_path_returns_none_for_relative_path() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    assert_eq!(to_relative_path("src/main.rs", temporary_dir.path()), None);
}

#[test]
fn to_relative_path_returns_none_for_path_outside_base() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let base = temporary_dir.path().join("workspace");
    std::fs::create_dir_all(&base).unwrap();
    let outside = temporary_dir.path().join("other/file.rs");
    assert_eq!(to_relative_path(outside.to_str().unwrap(), &base), None);
}

#[test]
fn to_relative_path_returns_none_for_base_itself() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let base = std::fs::canonicalize(temporary_dir.path()).unwrap();
    assert_eq!(to_relative_path(base.to_str().unwrap(), &base), None);
}

fn lookup_from<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name: &str| {
        values
            .iter()
            .find(|(variable_name, _)| *variable_name == name)
            .map(|(_, value)| (*value).to_string())
    }
}

#[test]
fn expand_path_tokens_replaces_defined_variables() {
    let lookup = lookup_from(&[("YLVA_HOME", "/home/user/.ylva")]);

    assert_eq!(
        expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
        "/home/user/.ylva/AGENTS.md"
    );
    assert_eq!(
        expand_path_tokens("${YLVA_HOME}/AGENTS.md", &lookup),
        "/home/user/.ylva/AGENTS.md"
    );
    assert_eq!(
        expand_path_tokens("pre$YLVA_HOME_post", &lookup),
        "pre$YLVA_HOME_post",
        "an underscore is part of the variable name, not a delimiter"
    );
}

#[test]
fn expand_path_tokens_leaves_undefined_or_malformed_tokens_literal() {
    let lookup = lookup_from(&[]);

    for value in [
        "$UNDEFINED/file.md",
        "${UNDEFINED}/file.md",
        "${unterminated",
        "${}",
        "$1/file.md",
        "cost$5/file.md",
        "$",
        "src/main.rs",
    ] {
        assert_eq!(expand_path_tokens(value, &lookup), value, "for {value:?}");
    }
}

#[test]
fn expand_path_tokens_substitutes_once_without_rescanning() {
    let lookup = lookup_from(&[("FIRST", "$SECOND"), ("SECOND", "resolved")]);

    assert_eq!(
        expand_path_tokens("$FIRST", &lookup),
        "$SECOND",
        "a substituted value is never rescanned for further tokens"
    );
}

#[test]
fn expand_path_tokens_expands_only_a_leading_tilde() {
    let lookup = lookup_from(&[("HOME", "/home/user")]);

    assert_eq!(expand_path_tokens("~", &lookup), "/home/user");
    assert_eq!(
        expand_path_tokens("~/notes.md", &lookup),
        "/home/user/notes.md"
    );
    assert_eq!(
        expand_path_tokens("~user/notes.md", &lookup),
        "~user/notes.md",
        "`~user` expansion is deliberately unsupported"
    );
    assert_eq!(
        expand_path_tokens("docs/~template.md", &lookup),
        "docs/~template.md",
        "only a leading tilde is expanded"
    );
}

#[test]
fn expand_path_tokens_without_any_lookup_returns_the_value_unchanged() {
    let lookup = lookup_from(&[]);

    assert_eq!(expand_path_tokens("src/main.rs", &lookup), "src/main.rs");
    assert_eq!(expand_path_tokens("~/notes.md", &lookup), "~/notes.md");
    assert_eq!(
        expand_path_tokens("$YLVA_HOME/AGENTS.md", &lookup),
        "$YLVA_HOME/AGENTS.md"
    );
}

#[test]
fn configured_named_rule_overrides_default_dot_path_verdict() {
    // A `[tools.read]`-style config changing `dot_path`'s verdict to
    // `AutoApproved` for this tool only — observable against the default
    // floor, which asks for user approval; `outside_workspace`
    // protection is intentionally NOT included, confirming the
    // full-replace (not merge) semantics.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoApproved,
            reason: None,
            target: None,
        }])
        .expect("valid sandbox fixture");

    let dot_path = workspace.join(".env");
    assert_eq!(
        context.check_path(&dot_path),
        Some(ApprovalCheck::AutoApproved),
        "configured dot_path rule should override the default floor's \
         user-approval verdict"
    );

    // outside_workspace is not part of this tool's configured list, so it
    // no longer fires (full replace, not merge) — sandbox abstains.
    let outside = temporary_dir.path().join("other/file.rs");
    assert_eq!(
        context.check_path(&outside),
        None,
        "outside_workspace should not fire when omitted from configured_rules"
    );
}

#[test]
fn unconfigured_tool_falls_back_to_default_floor() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().join("workspace");
    let sandbox = make_sandbox(&workspace);
    // No with_configured_rules call: default floor applies to every tool.
    let context = SandboxContext::new(Arc::clone(&sandbox));

    let dot_path = workspace.join(".env");
    assert_eq!(
        context.check_path(&dot_path),
        Some(ApprovalCheck::UserApprovalRequired)
    );
    let outside = temporary_dir.path().join("other/file.rs");
    assert_eq!(
        context.check_path(&outside),
        Some(ApprovalCheck::UserApprovalRequired)
    );
}

#[test]
fn configured_rules_mix_named_and_glob_last_match_wins() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let canonical_sandbox_root = sandbox.read().unwrap().sandbox_root.clone();
    let allow_pattern = format!("{}/allowed/**", canonical_sandbox_root.to_string_lossy());

    // List order: dot_path (AutoDenied) then a glob rule that re-approves
    // one specific dot-path — last match wins, so the glob rule (later in
    // the list) should override the earlier named rule for matching paths.
    let dot_allow_pattern = format!(
        "{}/.allowed_dot/**",
        canonical_sandbox_root.to_string_lossy()
    );
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: dot_allow_pattern,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ])
        .expect("valid sandbox fixture");

    // Regular dot-path still denied (only the named rule matches it).
    let dot_path = workspace.join(".env");
    assert!(matches!(
        context.check_path(&dot_path),
        Some(ApprovalCheck::AutoDenied { .. })
    ));

    // Dot-path under .allowed_dot: named dot_path rule matches (deny),
    // then the later glob rule also matches and overrides to approved —
    // proving last-match-wins across Named and Glob in one list.
    let allowed_dot_path = workspace.join(".allowed_dot/file.txt");
    assert_eq!(
        context.check_path(&allowed_dot_path),
        Some(ApprovalCheck::AutoApproved)
    );

    // Non-dot path under allowed/: only the glob rule matches.
    let allowed_path = workspace.join("allowed/file.txt");
    assert_eq!(
        context.check_path(&allowed_path),
        Some(ApprovalCheck::AutoApproved)
    );
}

#[test]
fn auto_approve_catch_all_wins_over_configured_deny_rule() {
    // Mirrors --auto-approve-write/--auto-approve-edit: the catch-all
    // `with_rule` is appended after `with_configured_rules` and must win
    // even though the configured list would otherwise deny the path.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::Named {
            rule: NamedSandboxRule::DotPath,
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
            target: None,
        }])
        .expect("valid sandbox fixture")
        .with_rule(SandboxRule {
            verdict: ApprovalCheck::AutoApproved,
            pattern: glob::Pattern::new("**").unwrap(),
        });

    let dot_path = workspace.join(".env");
    assert_eq!(
        context.check_path(&dot_path),
        Some(ApprovalCheck::AutoApproved),
        "catch-all auto-approve rule must win over a configured deny rule"
    );
}

/// A context whose configured list contains only one `cross_filesystem`
/// rule with the given verdict/reason (the configured list fully
/// replaces the default floor).
fn cross_filesystem_configured_context(
    sandbox: &Arc<RwLock<Sandbox>>,
    verdict: ApprovalVerdictConfig,
    reason: Option<String>,
) -> SandboxContext {
    SandboxContext::new(Arc::clone(sandbox))
        .with_configured_rules(vec![SandboxRuleConfig::Named {
            rule: NamedSandboxRule::CrossFilesystem,
            verdict,
            reason,
            target: None,
        }])
        .expect("valid sandbox fixture")
}

#[test]
fn is_cross_device_requires_two_known_and_different_ids() {
    assert!(is_cross_device(Some(1), Some(2)), "different ids match");
    assert!(!is_cross_device(Some(1), Some(1)), "equal ids abstain");
    assert!(
        !is_cross_device(Some(1), None),
        "unknown sandbox-root metadata abstains"
    );
    assert!(
        !is_cross_device(None, Some(2)),
        "unknown candidate metadata abstains"
    );
    assert!(!is_cross_device(None, None), "both sides unknown abstains");
}

#[cfg(unix)]
#[test]
fn resolve_device_returns_some_for_existing_path_and_none_for_missing() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let existing = temporary_dir.path().join("present.txt");
    std::fs::write(&existing, "x").unwrap();
    assert!(
        resolve_device(&existing).is_some(),
        "an existing path yields its st_dev on unix"
    );
    assert_eq!(
        resolve_device(&temporary_dir.path().join("missing.txt")),
        None,
        "a path with no metadata yields None"
    );
}

#[cfg(not(unix))]
#[test]
fn resolve_device_is_always_none_on_non_unix_targets() {
    assert_eq!(
        resolve_device(Path::new("/")),
        None,
        "non-unix targets never report a device, so cross_filesystem never matches"
    );
}

#[test]
fn check_path_cross_filesystem_does_not_match_same_device_path() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = cross_filesystem_configured_context(
        &sandbox,
        ApprovalVerdictConfig::UserApprovalRequired,
        None,
    );

    let existing_path = workspace.join("src/main.rs");
    std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
    std::fs::write(&existing_path, "fn main() {}").unwrap();
    assert_eq!(
        context.check_path(&existing_path),
        None,
        "an existing same-device path inside the workspace must not match"
    );
    assert_eq!(
        context.check_path(&workspace.join("not-yet-created.txt")),
        None,
        "a missing target whose deepest existing ancestor is the workspace \
         (same device) must not match"
    );
}

#[cfg(unix)]
#[test]
fn check_path_cross_filesystem_matches_path_on_a_different_device() {
    // `/dev` (devfs/tmpfs) is a different device from a normal workspace
    // tempdir on macOS and Linux — no mounts required.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let sandbox_device = resolve_device(&sandbox.read().unwrap().sandbox_root)
        .expect("the sandbox root is stat-able");
    let cross_device_probe = resolve_device(Path::new("/dev")).expect("/dev is stat-able");
    assert_ne!(
        sandbox_device, cross_device_probe,
        "fixture assumption: /dev must be a different device than the workspace tempdir"
    );

    let context = cross_filesystem_configured_context(
        &sandbox,
        ApprovalVerdictConfig::UserApprovalRequired,
        None,
    );
    assert_eq!(
        context.check_path(Path::new("/dev/null")),
        Some(ApprovalCheck::UserApprovalRequired),
        "a path on a different device than the sandbox root must match, \
         even though the configured list replaced the default floor \
         (the floor's outside_workspace check never runs)"
    );
}

#[cfg(unix)]
#[test]
fn check_path_cross_filesystem_matches_missing_target_via_cross_device_ancestor() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context = cross_filesystem_configured_context(
        &sandbox,
        ApprovalVerdictConfig::UserApprovalRequired,
        None,
    );

    assert_eq!(
        context.check_path(Path::new("/dev/ylva-cross-fs-missing/deep/file.txt")),
        Some(ApprovalCheck::UserApprovalRequired),
        "a nonexistent target whose deepest existing ancestor is /dev (a \
         different device) must match via the ancestor anchor"
    );
}

#[cfg(unix)]
#[test]
fn check_path_cross_filesystem_honors_each_verdict() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let cross_device_path = Path::new("/dev/null");

    let approved =
        cross_filesystem_configured_context(&sandbox, ApprovalVerdictConfig::AutoApproved, None);
    assert_eq!(
        approved.check_path(cross_device_path),
        Some(ApprovalCheck::AutoApproved)
    );

    let denied =
        cross_filesystem_configured_context(&sandbox, ApprovalVerdictConfig::AutoDenied, None);
    assert_eq!(
        denied.check_path(cross_device_path),
        Some(ApprovalCheck::AutoDenied {
            reason: CROSS_FILESYSTEM_REASON.to_string(),
        }),
        "auto_denied without a custom reason falls back to CROSS_FILESYSTEM_REASON"
    );

    let custom_denied = cross_filesystem_configured_context(
        &sandbox,
        ApprovalVerdictConfig::AutoDenied,
        Some("cross-device reads are blocked".to_string()),
    );
    assert_eq!(
        custom_denied.check_path(cross_device_path),
        Some(ApprovalCheck::AutoDenied {
            reason: "cross-device reads are blocked".to_string(),
        }),
        "a custom reason overrides the default denial reason"
    );

    let approval_required = cross_filesystem_configured_context(
        &sandbox,
        ApprovalVerdictConfig::UserApprovalRequired,
        None,
    );
    assert_eq!(
        approval_required.check_path(cross_device_path),
        Some(ApprovalCheck::UserApprovalRequired)
    );
}

#[cfg(unix)]
#[test]
fn check_path_cross_filesystem_last_match_wins_against_outside_workspace() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let cross_device_path = Path::new("/dev/null");

    let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::OutsideWorkspace,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::UserApprovalRequired,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert_eq!(
        cross_filesystem_last.check_path(cross_device_path),
        Some(ApprovalCheck::UserApprovalRequired),
        "both rules match a cross-device path outside the workspace; the \
         later cross_filesystem rule wins"
    );

    let outside_workspace_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::UserApprovalRequired,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::OutsideWorkspace,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert!(matches!(
        outside_workspace_last.check_path(cross_device_path),
        Some(ApprovalCheck::AutoDenied { .. })
    ));
}

#[cfg(unix)]
#[test]
fn check_path_cross_filesystem_last_match_wins_against_path_glob() {
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let cross_device_path = Path::new("/dev/null");

    let glob_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::PathGlob {
                pattern: "/dev/**".to_string(),
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert_eq!(
        glob_last.check_path(cross_device_path),
        Some(ApprovalCheck::AutoApproved),
        "the later path glob re-approves the cross-device path"
    );

    let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::PathGlob {
                pattern: "/dev/**".to_string(),
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert!(matches!(
        cross_filesystem_last.check_path(cross_device_path),
        Some(ApprovalCheck::AutoDenied { .. })
    ));
}

#[test]
fn check_path_dot_path_ordering_unaffected_by_cross_filesystem_on_same_device() {
    // A dot path inside the workspace is always on the workspace's
    // device (no mount involved), so `cross_filesystem` abstains and
    // `dot_path`'s verdict applies in either list order.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let dot_path = workspace.join(".env");

    let dot_path_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert!(matches!(
        dot_path_last.check_path(&dot_path),
        Some(ApprovalCheck::AutoDenied { .. })
    ));

    let cross_filesystem_last = SandboxContext::new(Arc::clone(&sandbox))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::DotPath,
                verdict: ApprovalVerdictConfig::AutoDenied,
                reason: None,
                target: None,
            },
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::CrossFilesystem,
                verdict: ApprovalVerdictConfig::AutoApproved,
                reason: None,
                target: None,
            },
        ])
        .expect("valid sandbox fixture");
    assert!(
        matches!(
            cross_filesystem_last.check_path(&dot_path),
            Some(ApprovalCheck::AutoDenied { .. })
        ),
        "cross_filesystem does not match a same-device path, so the later \
         rule cannot override the earlier dot_path verdict"
    );
}

#[cfg(unix)]
#[test]
fn check_path_default_floor_does_not_include_cross_filesystem() {
    // Unconfigured tool: the built-in floor applies. With `/` as the
    // sandbox root, `/dev/null` is *inside* the workspace but on a
    // different device, so `cross_filesystem` is the only rule that could
    // match — and because it stays opt-in, the sandbox abstains.
    let workspace = Path::new("/");
    let sandbox = make_sandbox(workspace);
    let context = SandboxContext::new(Arc::clone(&sandbox));
    let sandbox_device = resolve_device(workspace).expect("/ is stat-able");
    let candidate_device = resolve_device(Path::new("/dev/null")).expect("/dev/null is stat-able");
    assert_ne!(
        sandbox_device, candidate_device,
        "fixture assumption: /dev/null must live on a different device than /"
    );

    assert_eq!(
        context.check_path(Path::new("/dev/null")),
        None,
        "cross_filesystem is opt-in: the default floor abstains for a \
         cross-device path inside the workspace"
    );
    assert_eq!(
        context.check_path(&workspace.join("file.txt")),
        None,
        "an unconfigured tool still abstains for a normal in-workspace path"
    );
}

#[cfg(unix)]
#[test]
fn auto_approve_catch_all_wins_over_configured_cross_filesystem_rule() {
    // Mirrors --auto-approve-read-style catch-all: appended last, so it
    // wins over a configured cross_filesystem rule for a cross-device path.
    let temporary_dir = make_visible_temporary_directory().unwrap();
    let workspace = temporary_dir.path().to_path_buf();
    let sandbox = make_sandbox(&workspace);
    let context =
        cross_filesystem_configured_context(&sandbox, ApprovalVerdictConfig::AutoDenied, None)
            .with_rule(SandboxRule {
                verdict: ApprovalCheck::AutoApproved,
                pattern: glob::Pattern::new("**").unwrap(),
            });

    assert_eq!(
        context.check_path(Path::new("/dev/null")),
        Some(ApprovalCheck::AutoApproved),
        "the catch-all rule must win over a configured cross_filesystem deny"
    );
}
