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

fn write_skill(root: &Path, name: &str, body: &str) -> PathBuf {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: Test skill\n---\n{body}"),
    )
    .unwrap();
    path
}

#[test]
fn parser_preserves_body_and_handles_bom_crlf_and_optional_fields() {
    let (_, description, body) = parse_skill(
        "\u{feff}---\r\nname: example\r\ndescription: |\r\n  Multi\r\n  line\r\nallowed-tools: bash\r\n---\r\n\r\n# Body\r\n",
        "example",
    ).unwrap();
    assert_eq!(description, "Multi\nline\n");
    assert_eq!(body, "\r\n# Body\r\n");
}

#[test]
fn parser_rejects_invalid_documents() {
    for contents in [
        "name: example",
        "---\nname: example\n",
        "---\n- item\n---\nbody",
        "---\nname: example\nname: example\ndescription: x\n---\nbody",
        "---\nname: example\ndescription: x\nunknown: 1\nunknown: 2\n---\nbody",
        "---\nname: 123\ndescription: x\n---\nbody",
        "---\nname: example\ndescription: 123\n---\nbody",
        "---\nname: example\ndescription: x\n---\n \t",
        "---\nname: example\ndescription: ' '\n---\nbody",
        "---\nname: [example]\ndescription: x\n---\nbody",
        "---\nname: example\ndescription: [x]\n---\nbody",
        "---\nname: example\ndescription: [\n---\nbody",
    ] {
        assert!(parse_skill(contents, "example").is_err(), "{contents}");
    }
    for name in [
        "",
        "-example",
        "example-",
        "ex--ample",
        "Example",
        "ex_ample",
        "é",
        &"a".repeat(65),
    ] {
        assert!(parse_skill(
            &format!("---\nname: '{name}'\ndescription: x\n---\nbody"),
            name
        )
        .is_err());
    }
    assert!(parse_skill("---\nname: other\ndescription: x\n---\nbody", "example").is_err());
    assert!(parse_skill(
        &format!(
            "---\nname: example\ndescription: '{}'\n---\nbody",
            "é".repeat(1025)
        ),
        "example"
    )
    .is_err());
    assert!(parse_skill(
        &format!(
            "---\nname: example\ndescription: '{}'\n---\nbody",
            "é".repeat(1024)
        ),
        "example"
    )
    .is_ok());
    let name = "a".repeat(64);
    assert!(parse_skill(
        &format!("---\nname: {name}\ndescription: x\n---\nbody"),
        &name
    )
    .is_ok());
}

#[test]
fn discovery_precedence_fallback_and_sorted_names() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    let winner = write_skill(&first, "zebra", "first");
    let shadowed = write_skill(&second, "zebra", "second");
    let invalid = write_skill(&first, "alpha", " ");
    let fallback = write_skill(&second, "alpha", "fallback");
    let catalog = SkillCatalog::from_roots(&[first, second]);
    assert_eq!(
        catalog
            .skills
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["alpha", "zebra"]
    );
    assert_eq!(
        catalog.skills["zebra"].source.canonical_path,
        winner.canonicalize().unwrap()
    );
    assert_eq!(
        catalog.skills["zebra"].shadowed[0].canonical_path,
        shadowed.canonicalize().unwrap()
    );
    assert_eq!(
        catalog.skills["alpha"].source.canonical_path,
        fallback.canonicalize().unwrap()
    );
    assert_eq!(catalog.diagnostics[0].path, invalid);
}

#[test]
fn identical_copies_include_nested_resources_and_preserve_the_winner() {
    let temporary = tempfile::tempdir().unwrap();
    let roots: Vec<_> = ["first", "second", "third"]
        .iter()
        .map(|name| temporary.path().join(name))
        .collect();
    for root in &roots {
        write_skill(root, "example", "Instructions");
        std::fs::create_dir_all(root.join("example/resources/nested")).unwrap();
        // Creation order must not affect the manifest.
        let names = if root == &roots[0] {
            ["z", "a"]
        } else {
            ["a", "z"]
        };
        for name in names {
            std::fs::write(root.join("example/resources/nested").join(name), name).unwrap();
        }
        std::fs::write(
            root.join("example/resources/nested/SKILL.md"),
            root.as_os_str().as_encoded_bytes(),
        )
        .unwrap();
    }
    let catalog = SkillCatalog::from_roots(&roots);
    let skill = &catalog.skills["example"];
    assert_eq!(
        skill.source.canonical_path,
        roots[0].join("example/SKILL.md").canonicalize().unwrap()
    );
    assert_eq!(skill.duplicates.len(), 2);
    assert!(skill.shadowed.is_empty());
    assert!(catalog.diagnostics.is_empty());
    assert_eq!(catalog.skills.len(), 1);
    let callable = resolve_policy(
        &catalog,
        &crate::ProfileConfig::default(),
        temporary.path(),
        false,
    )
    .unwrap();
    assert_eq!(callable.skills.len(), 1);
    assert_eq!(
        callable.skills["example"].skill.source.canonical_path,
        skill.source.canonical_path
    );
    let context = crate::SandboxContext::new(Arc::new(std::sync::RwLock::new(
        crate::Sandbox::with_root(temporary.path().to_owned()),
    )))
    .with_skill_sources(Arc::new(vec![skill.source.canonical_path.clone()]))
    .with_configured_rules(vec![crate::sandbox::SandboxRuleConfig::Named {
        rule: crate::sandbox::NamedSandboxRule::SkillSidecar,
        verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
        target: None,
        reason: None,
    }]);
    assert_eq!(
        context.check_path(&roots[0].join("example/resources/nested/a")),
        Some(crate::ApprovalCheck::AutoApproved),
    );
    assert_eq!(
        context.check_path(&roots[1].join("example/resources/nested/a")),
        None,
    );
    std::fs::write(roots[1].join("example/resources/nested/a"), "changed").unwrap();
    std::fs::rename(
        roots[2].join("example/resources/nested/a"),
        roots[2].join("example/resources/nested/b"),
    )
    .unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert_eq!(catalog.skills["example"].shadowed.len(), 2);
    assert!(catalog.skills["example"].duplicates.is_empty());
}

#[test]
fn duplicate_detection_compares_raw_frontmatter_and_does_not_scan_unique_skills() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    write_skill(&first, "example", "Same body");
    let candidate = write_skill(&second, "example", "Same body");
    let original = std::fs::read_to_string(&candidate).unwrap();
    for contents in [
        original.replace("Test skill", "Another description"),
        original.replace("---\nSame body", "license: MIT\n---\nSame body"),
        original.replace('\n', "\r\n"),
    ] {
        std::fs::write(&candidate, contents).unwrap();
        let catalog = SkillCatalog::from_roots(&[first.clone(), second.clone()]);
        assert_eq!(catalog.skills["example"].shadowed.len(), 1);
        assert!(catalog.skills["example"].duplicates.is_empty());
        assert!(catalog.diagnostics.is_empty());
    }
    let directory = first.join("example");
    for index in 0..=fingerprint::MAX_RESOURCE_ENTRIES {
        std::fs::write(directory.join(format!("resource-{index}")), "").unwrap();
    }
    let catalog = SkillCatalog::from_roots(&[first]);
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn duplicate_fingerprints_ignore_only_explicit_filesystem_and_git_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let roots: Vec<_> = ["first", "second", "third"]
        .iter()
        .map(|name| temporary.path().join(name))
        .collect();
    for root in &roots {
        write_skill(root, "example", "Instructions");
        std::fs::create_dir_all(root.join("example/resources")).unwrap();
        std::fs::write(root.join("example/resources/reference"), "Reference").unwrap();
    }
    let git_directory = roots[0].join("example").join(crate::GIT_ENTRY_NAME);
    std::fs::create_dir_all(git_directory.join("objects")).unwrap();
    std::fs::File::create(git_directory.join("objects/large"))
        .unwrap()
        .set_len(fingerprint::MAX_RESOURCE_BYTES + 1)
        .unwrap();
    std::fs::File::create(
        roots[0]
            .join("example/resources")
            .join(crate::DS_STORE_FILE_NAME),
    )
    .unwrap()
    .set_len(fingerprint::MAX_RESOURCE_BYTES + 1)
    .unwrap();
    std::fs::write(
        roots[1].join("example").join(crate::GIT_ENTRY_NAME),
        "gitdir: elsewhere",
    )
    .unwrap();
    std::fs::write(
        roots[1].join("example").join(crate::DS_STORE_FILE_NAME),
        "Finder metadata",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "missing",
        roots[2].join("example").join(crate::GIT_ENTRY_NAME),
    )
    .unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert_eq!(catalog.skills["example"].duplicates.len(), 2);
    assert!(catalog.skills["example"].shadowed.is_empty());
    assert!(catalog.diagnostics.is_empty());
}

#[test]
fn duplicate_fingerprints_still_compare_hidden_and_git_ignored_resources() {
    let temporary = tempfile::tempdir().unwrap();
    let roots: Vec<_> = ["first", "second"]
        .iter()
        .map(|name| temporary.path().join(name))
        .collect();
    for root in &roots {
        write_skill(root, "example", "Instructions");
        std::fs::create_dir_all(root.join("example/generated")).unwrap();
        std::fs::write(root.join("example/.gitignore"), "generated/\n").unwrap();
        std::fs::write(root.join("example/generated/reference"), "Reference").unwrap();
        std::fs::write(root.join("example/.runtime-config"), "Configuration").unwrap();
    }
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .duplicates
            .len(),
        1
    );
    let generated = roots[1].join("example/generated/reference");
    std::fs::write(&generated, "Different reference").unwrap();
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .shadowed
            .len(),
        1
    );
    std::fs::write(generated, "Reference").unwrap();
    std::fs::write(
        roots[1].join("example/.runtime-config"),
        "Different configuration",
    )
    .unwrap();
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .shadowed
            .len(),
        1
    );
}

#[test]
fn duplicate_resource_limits_are_inclusive_and_fail_conservatively() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    for root in [&first, &second] {
        write_skill(root, "example", "Same body");
        std::fs::File::create(root.join("example/resource"))
            .unwrap()
            .set_len(fingerprint::MAX_RESOURCE_BYTES)
            .unwrap();
    }
    let roots = [first.clone(), second.clone()];
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .duplicates
            .len(),
        1
    );
    std::fs::File::options()
        .write(true)
        .open(second.join("example/resource"))
        .unwrap()
        .set_len(fingerprint::MAX_RESOURCE_BYTES + 1)
        .unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert_eq!(catalog.skills["example"].shadowed.len(), 1);
    assert!(catalog.skills["example"].duplicates.is_empty());
    assert_eq!(catalog.diagnostics.len(), 1);
    assert_eq!(catalog.diagnostics[0].kind, SkillDiagnosticKind::Comparison);
    assert!(catalog.diagnostics[0]
        .reason
        .contains("resource bytes exceed"));
    for root in [&first, &second] {
        std::fs::remove_file(root.join("example/resource")).unwrap();
        // SKILL.md also counts toward traversal work, though its captured bytes
        // are hashed separately.
        for index in 1..fingerprint::MAX_RESOURCE_ENTRIES {
            std::fs::write(root.join("example").join(format!("resource-{index}")), "").unwrap();
        }
    }
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .duplicates
            .len(),
        1
    );
    std::fs::write(second.join("example/one-too-many"), "").unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert_eq!(catalog.skills["example"].shadowed.len(), 1);
    assert!(catalog.diagnostics[0]
        .reason
        .contains("resource entries exceed"));
}

#[cfg(unix)]
#[test]
fn duplicate_resources_account_for_executable_bits_and_safe_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let roots: Vec<_> = ["first", "second"]
        .iter()
        .map(|name| temporary.path().join(name))
        .collect();
    for root in &roots {
        write_skill(root, "example", "Same body");
        let directory = root.join("example");
        std::fs::create_dir_all(directory.join("resources")).unwrap();
        std::fs::write(directory.join("resources/script"), "script").unwrap();
        std::fs::set_permissions(
            directory.join("resources/script"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        symlink("resources/script", directory.join("script-link")).unwrap();
        symlink("resources", directory.join("directory-link")).unwrap();
        symlink("SKILL.md", directory.join("instruction-alias")).unwrap();
    }
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .duplicates
            .len(),
        1
    );
    let script = roots[1].join("example/resources/script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .shadowed
            .len(),
        1
    );
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = roots[1].join("example/script-link");
    std::fs::remove_file(&link).unwrap();
    symlink("./resources/script", &link).unwrap();
    assert_eq!(
        SkillCatalog::from_roots(&roots).skills["example"]
            .shadowed
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn uncomparable_resources_remain_shadowed_and_warn_without_rejecting_skills() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temporary = tempfile::tempdir().unwrap();
    let roots: Vec<_> = ["first", "second", "third"]
        .iter()
        .map(|name| temporary.path().join(name))
        .collect();
    for root in &roots {
        write_skill(root, "example", "Same body");
    }
    let resource = roots[0].join("example/resource");
    let outside = temporary.path().join("outside");
    std::fs::write(&outside, "outside").unwrap();
    for target in [Path::new("."), outside.as_path(), Path::new("missing")] {
        symlink(target, &resource).unwrap();
        let catalog = SkillCatalog::from_roots(&roots);
        assert_eq!(catalog.skills["example"].shadowed.len(), 2);
        assert!(catalog.skills["example"].duplicates.is_empty());
        // A failed winner comparison is reported once, not per collision.
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(catalog.diagnostics[0].kind, SkillDiagnosticKind::Comparison);
        std::fs::remove_file(&resource).unwrap();
    }
    std::fs::write(&resource, "secret").unwrap();
    std::fs::set_permissions(&resource, std::fs::Permissions::from_mode(0o0)).unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    std::fs::set_permissions(&resource, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(catalog.skills["example"].shadowed.len(), 2);
    assert_eq!(catalog.diagnostics.len(), 1);
    std::fs::remove_file(&resource).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&resource).unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert_eq!(catalog.skills["example"].shadowed.len(), 2);
    assert!(catalog.diagnostics[0]
        .reason
        .contains("not a regular file or directory"));
    drop(listener);
    std::fs::remove_file(&resource).unwrap();
    let mut directory = roots[0].join("example");
    for _ in 1..fingerprint::MAX_RESOURCE_DEPTH {
        directory = directory.join("nested");
    }
    std::fs::create_dir_all(&directory).unwrap();
    assert!(SkillCatalog::from_roots(&roots).diagnostics.is_empty());
    std::fs::create_dir(directory.join("too-deep")).unwrap();
    let catalog = SkillCatalog::from_roots(&roots);
    assert!(catalog.diagnostics[0]
        .reason
        .contains("resource cycle or depth exceeds"));
}

#[test]
fn discovery_uses_only_approved_roots_and_no_recursion() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    let launch = temporary.path().join("launch");
    let home = temporary.path().join("home");
    let ylva = temporary.path().join("ylva");
    std::fs::create_dir_all(&launch).unwrap();
    for directory in [".ylva", ".agents", ".github", ".claude"] {
        write_skill(
            &workspace.join(directory).join("skills"),
            "example",
            directory,
        );
    }
    write_skill(&ylva.join("skills"), "example", "ylva");
    for directory in [".agents", ".copilot", ".claude"] {
        write_skill(&home.join(directory).join("skills"), "example", directory);
    }
    write_skill(&launch.join(".ylva/skills"), "launch", "launch");
    write_skill(
        &workspace.join(".ylva/skills/nested/deeper"),
        "hidden",
        "body",
    );
    let catalog =
        SkillCatalog::discover(Some(&workspace), &launch, Some(&home), Some(&ylva)).unwrap();
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills["example"].body, ".ylva");
    assert_eq!(catalog.skills["example"].shadowed.len(), 7);
    let catalog = SkillCatalog::discover(None, &launch, None, None).unwrap();
    assert_eq!(catalog.skills.len(), 1);
    assert!(catalog.skills.contains_key("launch"));
    assert!(SkillCatalog::discover(None, &temporary.path().join("missing"), None, None).is_err());
}

#[test]
fn exact_case_missing_non_directory_invalid_utf8_and_broken_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("skills");
    let path = write_skill(&root, "lower", "body");
    std::fs::rename(&path, path.with_file_name("skill.md")).unwrap();
    let path = write_skill(&root, "invalid", "body");
    std::fs::write(path, [255]).unwrap();
    let file = temporary.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    let catalog = SkillCatalog::from_roots(&[temporary.path().join("missing"), file, root]);
    assert!(catalog.skills.is_empty());
    assert_eq!(catalog.diagnostics.len(), 2);
}

#[test]
fn per_file_limit_exact_fit_and_fallback() {
    let temporary = tempfile::tempdir().unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    let path = write_skill(&first, "example", "x");
    let overhead = std::fs::read(&path).unwrap().len() - 1;
    write_skill(&first, "example", &"x".repeat(MAX_SKILL_BYTES - overhead));
    assert_eq!(
        SkillCatalog::from_roots(std::slice::from_ref(&first)).skills["example"].raw_bytes,
        MAX_SKILL_BYTES
    );
    write_skill(
        &first,
        "example",
        &"x".repeat(MAX_SKILL_BYTES - overhead + 1),
    );
    write_skill(&second, "example", "fallback");
    let catalog = SkillCatalog::from_roots(&[first, second]);
    assert_eq!(catalog.skills["example"].body, "fallback");
    assert_eq!(catalog.diagnostics[0].kind, SkillDiagnosticKind::FileLimit);
}

#[test]
fn catalog_count_and_aggregate_limits_continue_scanning() {
    let temporary = tempfile::tempdir().unwrap();
    for index in 0..129 {
        write_skill(temporary.path(), &format!("skill-{index:03}"), "body");
    }
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned()]);
    assert_eq!(catalog.skills.len(), MAX_SKILLS);
    assert_eq!(catalog.diagnostics.len(), 1);
    let temporary = tempfile::tempdir().unwrap();
    for index in 0..64 {
        let name = format!("skill-{index:03}");
        let path = write_skill(temporary.path(), &name, "x");
        let overhead = std::fs::read(path).unwrap().len() - 1;
        write_skill(
            temporary.path(),
            &name,
            &"x".repeat(MAX_SKILL_BYTES - overhead),
        );
    }
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned()]);
    assert_eq!(
        catalog
            .skills
            .values()
            .map(|skill| skill.raw_bytes)
            .sum::<usize>(),
        MAX_CATALOG_BYTES
    );
    write_skill(temporary.path(), "z-last", "body");
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned()]);
    assert_eq!(catalog.skills.len(), 64);
    assert_eq!(catalog.diagnostics.len(), 1);
    let path = temporary.path().join("skill-000/SKILL.md");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 200);
    std::fs::write(path, bytes).unwrap();
    write_skill(temporary.path(), "y-large", &"x".repeat(200));
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned()]);
    assert!(!catalog.skills.contains_key("y-large"));
    assert!(catalog.skills.contains_key("z-last"));
    let fallback = temporary.path().join("fallback");
    write_skill(&fallback, "y-large", "small");
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned(), fallback]);
    assert_eq!(catalog.skills["y-large"].body, "small");
}

#[test]
fn shadowed_sources_do_not_charge_winning_budgets_and_diagnostics_are_stable() {
    let temporary = tempfile::tempdir().unwrap();
    let preferred = temporary.path().join("preferred");
    let shadowed = temporary.path().join("shadowed");
    for index in 0..MAX_SKILLS {
        let name = format!("skill-{index:03}");
        write_skill(&preferred, &name, "small");
        write_skill(&shadowed, &name, &"large".repeat(1_000));
    }
    write_skill(&shadowed, "z-over-limit", "body");
    let roots = [preferred, shadowed];
    let first = SkillCatalog::from_roots(&roots);
    let second = SkillCatalog::from_roots(&roots);
    assert_eq!(first.skills.len(), MAX_SKILLS);
    assert!(first.skills.values().all(|skill| skill.shadowed.len() == 1));
    assert_eq!(first.diagnostics.len(), 1);
    assert_eq!(
        first
            .diagnostics
            .iter()
            .map(|diagnostic| (&diagnostic.path, &diagnostic.kind, &diagnostic.reason))
            .collect::<Vec<_>>(),
        second
            .diagnostics
            .iter()
            .map(|diagnostic| (&diagnostic.path, &diagnostic.kind, &diagnostic.reason))
            .collect::<Vec<_>>(),
    );
}

#[cfg(unix)]
#[test]
fn unreadable_roots_directories_and_files_are_visible_omissions() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let inaccessible_root = temporary.path().join("root");
    let root = temporary.path().join("skills");
    std::fs::create_dir_all(&inaccessible_root).unwrap();
    let source = write_skill(&root, "unreadable", "body");
    let directory = root.join("inaccessible");
    std::fs::create_dir_all(&directory).unwrap();
    for path in [&inaccessible_root, &source, &directory] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o0)).unwrap();
    }
    let catalog = SkillCatalog::from_roots(&[inaccessible_root.clone(), root]);
    for path in [&inaccessible_root, &source, &directory] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(catalog.skills.is_empty());
    assert_eq!(catalog.diagnostics.len(), 3);
    assert!(catalog
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.kind == SkillDiagnosticKind::Io));
}

#[cfg(unix)]
#[test]
fn symlinked_files_validate_logical_names_and_broken_roots_and_files_warn() {
    use std::os::unix::fs::symlink;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("skills");
    let target = temporary.path().join("different-name");
    std::fs::create_dir_all(&target).unwrap();
    let target_file = target.join("instructions.md");
    std::fs::write(
        &target_file,
        "---\nname: example\ndescription: External\n---\nBody",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("example")).unwrap();
    symlink(&target_file, root.join("example/SKILL.md")).unwrap();
    std::fs::create_dir_all(root.join("broken")).unwrap();
    symlink(
        temporary.path().join("missing"),
        root.join("broken/SKILL.md"),
    )
    .unwrap();
    let broken_root = temporary.path().join("broken-root");
    symlink(temporary.path().join("missing-root"), &broken_root).unwrap();
    let catalog = SkillCatalog::from_roots(&[root.clone(), broken_root]);
    assert_eq!(
        catalog.skills["example"].source.canonical_path,
        target_file.canonicalize().unwrap()
    );
    assert_eq!(catalog.diagnostics.len(), 2);
    let context = crate::SandboxContext::new(Arc::new(std::sync::RwLock::new(
        crate::Sandbox::with_root(temporary.path().to_owned()),
    )))
    .with_configured_rules(vec![
        crate::sandbox::SandboxRuleConfig::PathGlob {
            pattern: target_file
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoApproved,
            reason: None,
        },
        crate::sandbox::SandboxRuleConfig::PathGlob {
            pattern: "**/changed".into(),
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
            reason: None,
        },
    ]);
    std::fs::remove_file(&target_file).unwrap();
    let changed = target.join("changed");
    std::fs::write(&changed, "Changed").unwrap();
    symlink(&changed, &target_file).unwrap();
    assert_eq!(
        context.check_snapshot_path(&catalog.skills["example"].source.canonical_path),
        Some(crate::ApprovalCheck::AutoApproved)
    );
    assert!(matches!(
        context.check_path(&target_file),
        Some(crate::ApprovalCheck::AutoDenied { .. })
    ));
}

#[test]
fn policy_never_selects_a_shadowed_source_after_denial() {
    let temporary = tempfile::tempdir().unwrap();
    let preferred = temporary.path().join("preferred");
    let fallback = temporary.path().join("fallback");
    let source = write_skill(&preferred, "example", "Preferred");
    write_skill(&fallback, "example", "Fallback");
    let catalog = SkillCatalog::from_roots(&[preferred, fallback]);
    let mut profile = crate::ProfileConfig::default();
    profile.tools.insert(
        "load_skill".into(),
        crate::config::ToolSandboxConfig {
            rules: vec![crate::sandbox::SandboxRuleConfig::PathGlob {
                pattern: source
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
                reason: None,
            }],
        },
    );
    assert!(resolve_policy(&catalog, &profile, temporary.path(), true)
        .unwrap()
        .skills
        .is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_allow_external_sources_and_deduplicate_canonical_files() {
    use std::os::unix::fs::symlink;
    let temporary = tempfile::tempdir().unwrap();
    let external = temporary.path().join("external");
    let root = temporary.path().join("skills");
    let original = write_skill(&external, "example", "snapshot");
    std::fs::create_dir_all(&root).unwrap();
    symlink(external.join("example"), root.join("example")).unwrap();
    let alias = temporary.path().join("alias");
    symlink(&root, &alias).unwrap();
    symlink(temporary.path().join("missing"), root.join("broken")).unwrap();
    let catalog = SkillCatalog::from_roots(&[root, alias]);
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(
        catalog.skills["example"].source.canonical_path,
        original.canonicalize().unwrap()
    );
    assert!(catalog.skills["example"].shadowed.is_empty());
    assert_eq!(catalog.skills["example"].duplicates.len(), 1);
    assert_eq!(catalog.diagnostics.len(), 2);
    std::fs::write(original, "changed").unwrap();
    assert_eq!(catalog.skills["example"].body, "snapshot");
}

#[test]
fn terminal_metadata_cannot_inject_control_sequences() {
    assert_eq!(terminal_safe("a\n\u{1b}[31m\tb"), "a\\n\\u{1b}[31m\\tb");
}

fn policy_fixture() -> (tempfile::TempDir, SkillCatalog, crate::ProfileConfig) {
    let temporary = tempfile::tempdir().unwrap();
    write_skill(temporary.path(), "alpha", "Alpha instructions");
    write_skill(temporary.path(), "beta", "Beta instructions");
    let catalog = SkillCatalog::from_roots(&[temporary.path().to_owned()]);
    (temporary, catalog, crate::ProfileConfig::default())
}

fn resolve_policy(
    catalog: &SkillCatalog,
    profile: &crate::ProfileConfig,
    root: &Path,
    interactive: bool,
) -> anyhow::Result<policy::CallableSkills> {
    policy::CallableSkills::resolve(
        catalog,
        profile,
        "default",
        &["load_skill".into(), "read".into()],
        interactive,
        Arc::new(std::sync::RwLock::new(crate::Sandbox::with_root(
            root.to_owned(),
        ))),
    )
}

#[test]
fn policy_distinguishes_all_none_subset_and_missing_loader() {
    let (temporary, catalog, mut profile) = policy_fixture();
    assert_eq!(
        resolve_policy(&catalog, &profile, temporary.path(), false)
            .unwrap()
            .skills
            .len(),
        2
    );
    profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec![]);
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false)
        .unwrap()
        .skills
        .is_empty());
    profile.agents.get_mut("default").unwrap().allowed_skills =
        Some(vec!["alpha".into(), "alpha".into()]);
    let callable = resolve_policy(&catalog, &profile, temporary.path(), false).unwrap();
    assert_eq!(callable.skills.len(), 1);
    assert!(callable.advertisement().unwrap().contains("Test skill"));
    assert!(!callable
        .advertisement()
        .unwrap()
        .contains("Alpha instructions"));
    profile.agents.get_mut("default").unwrap().allowed_tools = Some(vec!["read".into()]);
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false)
        .unwrap_err()
        .to_string()
        .contains("disables load_skill"));
    profile.agents.get_mut("default").unwrap().allowed_skills = None;
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false)
        .unwrap()
        .advertisement()
        .is_none());
    profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec!["missing".into()]);
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false)
        .unwrap_err()
        .to_string()
        .contains("unknown skill 'missing'"));
}

#[test]
fn policy_denial_and_approval_respect_mode_and_agent_override() {
    let (temporary, catalog, mut profile) = policy_fixture();
    profile.tools.insert(
        "load_skill".into(),
        crate::config::ToolSandboxConfig {
            rules: vec![crate::sandbox::SandboxRuleConfig::PathGlob {
                pattern: "**".into(),
                verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
                reason: Some("blocked".into()),
            }],
        },
    );
    assert!(resolve_policy(&catalog, &profile, temporary.path(), true)
        .unwrap()
        .skills
        .is_empty());
    profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec!["alpha".into()]);
    assert!(resolve_policy(&catalog, &profile, temporary.path(), true)
        .unwrap_err()
        .to_string()
        .contains("blocked"));
    profile.agents.get_mut("default").unwrap().tools.insert(
        "load_skill".into(),
        crate::config::ToolSandboxConfig { rules: vec![] },
    );
    assert_eq!(
        resolve_policy(&catalog, &profile, temporary.path(), false)
            .unwrap()
            .skills
            .len(),
        1
    );
    profile.agents.get_mut("default").unwrap().tools.clear();
    profile.tools.get_mut("load_skill").unwrap().rules =
        vec![crate::sandbox::SandboxRuleConfig::PathGlob {
            pattern: "**".into(),
            verdict: crate::sandbox::ApprovalVerdictConfig::UserApprovalRequired,
            reason: None,
        }];
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false).is_err());
    assert!(resolve_policy(&catalog, &profile, temporary.path(), true)
        .unwrap()
        .advertisement()
        .unwrap()
        .contains("[requires user approval]"));
    profile.agents.get_mut("default").unwrap().allowed_skills = None;
    assert!(resolve_policy(&catalog, &profile, temporary.path(), false)
        .unwrap()
        .skills
        .is_empty());
}

#[test]
fn unknown_skill_reports_rejection_provenance() {
    let (temporary, mut catalog, mut profile) = policy_fixture();
    catalog.skills.clear();
    catalog.diagnostic(
        &temporary.path().join("alpha/SKILL.md"),
        Some("alpha".into()),
        SkillDiagnosticKind::FileLimit,
        "too large",
    );
    profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec!["alpha".into()]);
    let error = resolve_policy(&catalog, &profile, temporary.path(), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("alpha/SKILL.md") && error.contains("too large"));
}

#[test]
fn loader_delivers_snapshot_and_rejects_adversarial_names_without_side_effects() {
    use crate::Tool;
    crate::test_support::run_locked(async {
        let (temporary, catalog, mut profile) = policy_fixture();
        profile.agents.get_mut("default").unwrap().allowed_skills = Some(vec!["alpha".into()]);
        let callable =
            Arc::new(resolve_policy(&catalog, &profile, temporary.path(), false).unwrap());
        let context = policy::loader_context(
            Arc::new(std::sync::RwLock::new(crate::Sandbox::with_root(
                temporary.path().to_owned(),
            ))),
            &profile,
            "default",
        );
        let tool = crate::agent::tools::load_skill::LoadSkillTool::new(callable, context);
        assert!(tool.validate_args(&serde_json::json!({})).is_err());
        assert!(tool
            .validate_args(&serde_json::json!({"name":"alpha","path":"/tmp"}))
            .is_err());
        assert_eq!(
            tool.needs_approval(&serde_json::json!({"name":"alpha"})),
            crate::ApprovalCheck::AutoApproved
        );
        let first = tool
            .create_call(uuid::Uuid::new_v4(), serde_json::json!({"name":"alpha"}))
            .run()
            .await
            .unwrap();
        assert!(first.requires_complete_delivery());
        assert!(first
            .text()
            .starts_with("Alpha instructions\n\n<ylva_skill_source>\n"));
        assert!(first.text().ends_with("\n</ylva_skill_source>"));
        assert!(first.text().contains("Resolve relative references"));
        assert!(first.text().contains(
            catalog.skills["alpha"]
                .source
                .canonical_path
                .to_str()
                .unwrap()
        ));
        assert!(!first.text().contains("description:"));
        assert_eq!(first.metadata().unwrap()["name"], "alpha");
        assert!(first.metadata_schema().is_some());
        assert_eq!(first.content_sections().len(), 2);
        assert!(first.display_summary().is_empty());
        assert_eq!(first.display_text(), Some(first.text()));
        std::fs::remove_file(temporary.path().join("alpha/SKILL.md")).unwrap();
        let repeated = tool
            .create_call(uuid::Uuid::new_v4(), serde_json::json!({"name":"alpha"}))
            .run()
            .await
            .unwrap();
        assert_eq!(first.text(), repeated.text());
        for name in ["", "beta", "../alpha", "/tmp/SKILL.md", "ALPHA"] {
            let args = serde_json::json!({"name":name});
            assert!(matches!(
                tool.needs_approval(&args),
                crate::ApprovalCheck::AutoDenied { .. }
            ));
            assert!(tool
                .create_call(uuid::Uuid::new_v4(), args)
                .run()
                .await
                .is_err());
        }
        assert!(!temporary.path().join("executed").exists());
        let denied_context = crate::SandboxContext::new(Arc::new(std::sync::RwLock::new(
            crate::Sandbox::with_root(temporary.path().to_owned()),
        )))
        .with_configured_rules(vec![crate::sandbox::SandboxRuleConfig::PathGlob {
            pattern: "**".into(),
            verdict: crate::sandbox::ApprovalVerdictConfig::AutoDenied,
            reason: None,
        }]);
        let callable =
            Arc::new(resolve_policy(&catalog, &profile, temporary.path(), false).unwrap());
        let denied = crate::agent::tools::load_skill::LoadSkillTool::new(callable, denied_context);
        assert!(denied
            .create_call(uuid::Uuid::new_v4(), serde_json::json!({"name":"alpha"}))
            .run()
            .await
            .is_err());
    });
}

#[test]
fn sidecar_rules_use_callable_canonical_boundaries_and_normal_ordering() {
    use crate::sandbox::{ApprovalVerdictConfig, NamedSandboxRule, SandboxRuleConfig};
    let (temporary, catalog, _) = policy_fixture();
    let source = catalog.skills["alpha"].source.canonical_path.clone();
    let directory = source.parent().unwrap();
    std::fs::create_dir_all(directory.join("nested")).unwrap();
    let sidecar = directory.join("nested/reference.md");
    std::fs::write(&sidecar, "sidecar").unwrap();
    let sandbox = Arc::new(std::sync::RwLock::new(crate::Sandbox::with_root(
        temporary.path().join("workspace"),
    )));
    let rule = |verdict| SandboxRuleConfig::Named {
        rule: NamedSandboxRule::SkillSidecar,
        verdict,
        reason: None,
        target: None,
    };
    let context = crate::SandboxContext::new(sandbox.clone())
        .with_skill_sources(Arc::new(vec![source.clone()]))
        .with_configured_rules(vec![
            SandboxRuleConfig::Named {
                rule: NamedSandboxRule::OutsideWorkspace,
                verdict: ApprovalVerdictConfig::UserApprovalRequired,
                reason: None,
                target: None,
            },
            rule(ApprovalVerdictConfig::AutoApproved),
        ]);
    assert_eq!(
        context.check_path(&sidecar),
        Some(crate::ApprovalCheck::AutoApproved)
    );
    assert_eq!(
        context.check_path(&source),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(
        context.check_path(directory),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(
        context.check_path(&directory.join("missing")),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    assert_eq!(
        context.check_path(&catalog.skills["beta"].source.canonical_path),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    std::fs::write(directory.join("nested/SKILL.md"), "not eligible").unwrap();
    assert_eq!(
        context.check_path(&directory.join("nested/SKILL.md")),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    let default_context = crate::SandboxContext::new(sandbox.clone())
        .with_skill_sources(Arc::new(vec![source.clone()]));
    assert_eq!(
        default_context.check_path(&sidecar),
        Some(crate::ApprovalCheck::UserApprovalRequired)
    );
    let empty_context = crate::SandboxContext::new(sandbox.clone())
        .with_configured_rules(vec![rule(ApprovalVerdictConfig::AutoApproved)]);
    assert_eq!(empty_context.check_path(&sidecar), None);
    for verdict in [
        ApprovalVerdictConfig::AutoApproved,
        ApprovalVerdictConfig::AutoDenied,
        ApprovalVerdictConfig::UserApprovalRequired,
    ] {
        let context = context
            .clone()
            .with_configured_rules(vec![rule(verdict.clone())]);
        assert!(matches!(
            (verdict, context.check_path(&sidecar)),
            (
                ApprovalVerdictConfig::AutoApproved,
                Some(crate::ApprovalCheck::AutoApproved)
            ) | (
                ApprovalVerdictConfig::AutoDenied,
                Some(crate::ApprovalCheck::AutoDenied { .. })
            ) | (
                ApprovalVerdictConfig::UserApprovalRequired,
                Some(crate::ApprovalCheck::UserApprovalRequired),
            )
        ));
    }
    let denied = context.clone().with_configured_rules(vec![
        rule(ApprovalVerdictConfig::AutoApproved),
        SandboxRuleConfig::PathGlob {
            pattern: "**".into(),
            verdict: ApprovalVerdictConfig::AutoDenied,
            reason: None,
        },
    ]);
    assert!(matches!(
        denied.check_path(&sidecar),
        Some(crate::ApprovalCheck::AutoDenied { .. })
    ));
    let wrong_tool = crate::SandboxContext::new(sandbox).with_tool_rules(
        "load_skill",
        vec![rule(ApprovalVerdictConfig::AutoApproved)],
    );
    assert!(matches!(
        wrong_tool.check_path(&sidecar),
        Some(crate::ApprovalCheck::AutoDenied { .. })
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&sidecar, directory.join("inside")).unwrap();
        symlink(
            &catalog.skills["beta"].source.canonical_path,
            directory.join("escape"),
        )
        .unwrap();
        symlink(
            catalog.skills["beta"]
                .source
                .canonical_path
                .parent()
                .unwrap(),
            directory.join("outside"),
        )
        .unwrap();
        assert_eq!(
            context.check_path(&directory.join("inside")),
            Some(crate::ApprovalCheck::AutoApproved)
        );
        assert_eq!(
            context.check_path(&directory.join("escape")),
            Some(crate::ApprovalCheck::UserApprovalRequired)
        );
        assert_eq!(
            context.check_path(&directory.join("outside/../beta/SKILL.md")),
            Some(crate::ApprovalCheck::UserApprovalRequired)
        );
    }
}
