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

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

fn command(home: &Path, launch: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ylva"));
    command
        .env_clear()
        .env("HOME", home)
        .env("YLVA_HOME", home.join(".ylva"))
        .current_dir(launch);
    command
}

fn write_skill(root: &Path, name: &str, description: &str) -> std::path::PathBuf {
    let directory = root.join(name);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("SKILL.md");
    std::fs::write(
        &path,
        format!("---\nname: {name}\ndescription: {description}\n---\nInstructions"),
    )
    .unwrap();
    path
}

#[test]
fn listing_is_provider_independent_sorted_and_reports_shadowed_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let launch = temporary.path().join("workspace");
    std::fs::create_dir_all(launch.join(".git")).unwrap();
    let primary = write_skill(&launch.join(".ylva/skills"), "zebra", "Winner");
    write_skill(&home.join(".ylva/skills"), "zebra", "Shadowed");
    write_skill(&home.join(".copilot/skills"), "alpha", "\"Multi\\nline\"");
    let output = command(&home, &launch)
        .args(["--profile", "does-not-exist", "skill", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let expected = format!(
        "alpha\n  Description: Multi\n               line\n  Source:      ~/.copilot/skills/alpha/SKILL.md\n  Body tokens: ~1\n\nzebra\n  Description: Winner\n  Source:      {}\n  Body tokens: ~1\n  Shadowed:    ~/.ylva/skills/zebra/SKILL.md\n",
        primary.canonicalize().unwrap().display(),
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    assert!(!home.join(".ylva/chats").exists());
    assert!(!home.join(".ylva/profiles").exists());
    assert!(!launch.join(".ylva/profiles").exists());
}

#[test]
fn empty_listing_warnings_help_and_unread_stdin() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let launch = temporary.path().join("launch");
    std::fs::create_dir_all(&launch).unwrap();
    let mut subagent = command(&home, &launch)
        .args(["skill", "list"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep stdin open: listing must return without waiting for EOF.
    let stdin = subagent.stdin.take().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if subagent.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            subagent.kill().unwrap();
            panic!("skill list consumed stdin");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(stdin);
    let output = subagent.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"No skills discovered.\n");
    let invalid = write_skill(&home.join(".ylva/skills"), "bad", "x");
    std::fs::write(&invalid, "not frontmatter").unwrap();
    let output = command(&home, &launch)
        .args(["skill", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"No skills discovered.\n");
    assert!(String::from_utf8_lossy(&output.stderr).contains("warning: skill"));
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("warning: skill ~/.ylva/skills/bad/SKILL.md: "));
    let output = command(&home, &launch)
        .args(["skill", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("list"));
}

#[test]
fn public_listing_propagates_output_failures() {
    struct FailingWriter;
    impl io::Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    assert!(
        ylva::skill_command::list(&ylva::skills::SkillCatalog::default(), &mut FailingWriter)
            .is_err()
    );
    let temporary = tempfile::tempdir().unwrap();
    write_skill(
        &temporary.path().join(".ylva/skills"),
        "example",
        "description",
    );
    let catalog = ylva::skills::SkillCatalog::discover(None, temporary.path(), None, None).unwrap();
    assert!(ylva::skill_command::list(&catalog, &mut FailingWriter).is_err());
}

#[test]
fn identical_skill_copies_are_hidden_unless_duplicates_are_requested() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let launch = temporary.path().join("workspace");
    std::fs::create_dir_all(&launch).unwrap();
    let primary = write_skill(&launch.join(".ylva/skills"), "example", "Same");
    write_skill(&home.join(".agents/skills"), "example", "Same");
    write_skill(&home.join(".claude/skills"), "example", "Different");
    let expected = format!(
        "example\n  Description: Same\n  Source:      {}\n  Body tokens: ~1\n  Shadowed:    ~/.claude/skills/example/SKILL.md\n",
        primary.canonicalize().unwrap().display(),
    );
    for with_duplicates in [false, true] {
        let mut command = command(&home, &launch);
        command.args(["skill", "list"]);
        if with_duplicates {
            command.arg("--with-duplicates");
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let mut expected = expected.clone();
        if with_duplicates {
            expected.push_str("  Duplicate:   ~/.agents/skills/example/SKILL.md\n");
        }
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
    let help = command(&home, &launch)
        .args(["skill", "list", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--with-duplicates"));
}

#[test]
#[cfg(unix)]
fn canonical_skill_aliases_show_only_the_discovered_path_when_requested() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let launch = temporary.path().join("workspace");
    std::fs::create_dir_all(&launch).unwrap();
    let primary = write_skill(&home.join(".agents/skills"), "example", "Same");
    let alias_root = home.join(".claude/skills");
    std::fs::create_dir_all(&alias_root).unwrap();
    std::os::unix::fs::symlink(primary.parent().unwrap(), alias_root.join("example")).unwrap();
    for with_duplicates in [false, true] {
        let mut command = command(&home, &launch);
        command.args(["skill", "list"]);
        if with_duplicates {
            command.arg("--with-duplicates");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(output.stderr.is_empty());
        let mut expected = String::from(
            "example\n  Description: Same\n  Source:      ~/.agents/skills/example/SKILL.md\n  Body tokens: ~1\n",
        );
        if with_duplicates {
            expected.push_str("  Duplicate:   ~/.claude/skills/example/SKILL.md\n");
        }
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[test]
fn skill_list_color_modes_style_names_and_labels_without_styling_values() {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let launch = temporary.path().join("workspace");
    std::fs::create_dir_all(&launch).unwrap();
    let source = write_skill(&launch.join(".ylva/skills"), "example", "Instructions");
    let plain = format!(
        "example\n  Description: Instructions\n  Source:      {}\n  Body tokens: ~1\n",
        source.canonicalize().unwrap().display(),
    );
    for mode in ["auto", "yes", "no"] {
        let output = command(&home, &launch)
            .args(["skill", "list", &format!("--color={mode}")])
            .env("TERM", "dumb")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let output = String::from_utf8(output.stdout).unwrap();
        if mode == "yes" {
            assert!(output.contains("\x1b[1mexample"));
            assert!(output.contains("\x1b[4mDescription:"));
            assert!(output.contains("\x1b[4mSource:"));
            assert!(output.contains("\x1b[4mBody tokens:"));
            assert!(output.contains(" ~1\n"));
            assert!(output.contains(" Instructions\n"));
            assert_eq!(
                output
                    .replace("\x1b[1m", "")
                    .replace("\x1b[4m", "")
                    .replace("\x1b[0m", ""),
                plain,
            );
        } else {
            assert_eq!(output, plain);
        }
    }
    let output = command(&home, &launch)
        .args(["--color=yes", "skill", "list", "--color=no"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), plain);
    let output = command(&home, &launch)
        .args(["skill", "list", "--color=invalid"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let help = command(&home, &launch)
        .args(["skill", "list", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("--color"));
}

#[test]
fn descriptions_wrap_as_indented_blocks_and_escape_terminal_controls() {
    let temporary = tempfile::tempdir().unwrap();
    write_skill(
        &temporary.path().join(".ylva/skills"),
        "example",
        "\"one two three four five\\nsix\\n\\nseven\\e[31m\\u202e\"",
    );
    let catalog = ylva::skills::SkillCatalog::discover(None, temporary.path(), None, None).unwrap();
    let mut output = Vec::new();
    ylva::skill_command::list_with_options(
        &catalog,
        false,
        ylva::skill_command::SkillListOptions {
            color: false,
            width: 30,
        },
        &mut output,
    )
    .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.starts_with(
        "example\n  Description: one two three\n               four five\n               six\n\n               seven\\u{1b}\n               [31m\\u{202e}\n",
    ), "{output}");
    assert!(!output.contains('\x1b'));
    assert!(!output.contains('\u{202e}'));
    for line in output
        .lines()
        .skip(1)
        .take_while(|line| !line.starts_with("  Source:"))
    {
        assert!(line.chars().count() <= 30, "{line}");
    }
    let mut narrow = Vec::new();
    ylva::skill_command::list_with_options(
        &catalog,
        false,
        ylva::skill_command::SkillListOptions {
            color: false,
            width: 0,
        },
        &mut narrow,
    )
    .unwrap();
    assert!(!narrow.is_empty());
}

#[test]
fn body_token_estimates_exclude_frontmatter_sidecars_and_loader_framing() {
    let temporary = tempfile::tempdir().unwrap();
    let body = "# Instructions\n\nRead `reference.md`.\nKeep Unicode: äöü 日本語.\n";
    let source = write_skill(
        &temporary.path().join(".ylva/skills"),
        "example",
        &"Long description ".repeat(30),
    );
    let instruction_file = format!(
        "---\nname: example\ndescription: {}\n---\n{body}",
        "Long description ".repeat(30),
    );
    std::fs::write(&source, &instruction_file).unwrap();
    std::fs::write(
        source.parent().unwrap().join("reference.md"),
        "sidecar ".repeat(1000),
    )
    .unwrap();
    let catalog = ylva::skills::SkillCatalog::discover(None, temporary.path(), None, None).unwrap();
    assert_eq!(catalog.skills["example"].body, body);
    let estimator = ylva::llm::TokenEstimator::Tiktoken;
    let expected = estimator.estimate_tokens(body);
    assert!(expected < estimator.estimate_tokens(&instruction_file));
    let mut output = Vec::new();
    ylva::skill_command::list(&catalog, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert_eq!(
        output
            .lines()
            .filter_map(|line| line.strip_prefix("  Body tokens: "))
            .collect::<Vec<_>>(),
        vec![format!("~{expected}")],
    );
}

#[test]
fn large_body_token_estimates_use_the_tui_compact_format() {
    let temporary = tempfile::tempdir().unwrap();
    let body = "instruction ".repeat(2_000);
    let source = write_skill(&temporary.path().join(".ylva/skills"), "example", "Example");
    std::fs::write(
        &source,
        format!("---\nname: example\ndescription: Example\n---\n{body}"),
    )
    .unwrap();
    let catalog = ylva::skills::SkillCatalog::discover(None, temporary.path(), None, None).unwrap();
    let estimate = ylva::llm::TokenEstimator::Tiktoken.estimate_tokens(&body);
    assert!(estimate >= 1_000);
    let formatted = ylva::llm::token_estimate::format_token_estimate(estimate);
    assert!(formatted.ends_with('K'));
    let mut output = Vec::new();
    ylva::skill_command::list(&catalog, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains(&format!("  Body tokens: ~{formatted}\n")));
    assert_eq!(
        ylva::tui_reducer_state::format_context_usage(estimate, estimate),
        format!("{formatted} (100%)"),
    );
}
