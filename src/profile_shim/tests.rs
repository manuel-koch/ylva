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

use std::ffi::OsString;

fn home_directories(ylva_home: &Path) -> ShimDirectories {
    ShimDirectories {
        profiles_root: ylva_home.join("profiles"),
        shims_directory: ylva_home.join("shims"),
        template: ShimTemplate::Home,
    }
}

fn write_file(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().expect("file has a parent")).expect("create parent");
    std::fs::write(path, contents).expect("write file");
}

fn profile_config(directories: &ShimDirectories, name: &str, contents: &str) {
    write_file(
        &directories.profiles_root.join(name).join("config.toml"),
        contents,
    );
}

fn planned_file_names(plan: &ShimPlan) -> Vec<String> {
    plan.shims
        .iter()
        .map(|shim| {
            shim.path
                .file_name()
                .expect("shim has a file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn shim_name_rule_rejects_ylva_and_invalid_profile_names() {
    assert!(is_valid_shim_name("my-shim"));
    assert!(is_valid_shim_name("ylva-work"));
    assert!(!is_valid_shim_name("ylva"));
    assert!(!is_valid_shim_name(""));
    assert!(!is_valid_shim_name("a/b"));
    assert!(!is_valid_shim_name(".hidden"));
}

#[test]
fn a_missing_profiles_root_plans_nothing() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());

    let plan = plan_shims(&directories, None).expect("plan");

    assert_eq!(plan, ShimPlan::default());
}

#[test]
fn a_profile_without_config_claims_the_default_shim_name() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("work")).expect("create profile");

    let plan = plan_shims(&directories, None).expect("plan");

    assert_eq!(planned_file_names(&plan), vec!["ylva-work".to_string()]);
    assert_eq!(plan.shims[0].profile_name, "work");
    assert_eq!(
        plan.shims[0].path,
        directories.shims_directory.join("ylva-work")
    );
}

#[test]
fn the_default_profile_never_gets_a_shim() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("default")).expect("create profile");

    let plan = plan_shims(&directories, None).expect("plan");

    assert!(plan.shims.is_empty());
}

#[test]
fn the_default_profile_with_a_shim_name_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "default", "shim_name = \"dflt\"\n");

    let error = plan_shims(&directories, None).expect_err("default with shim_name");

    assert!(error.to_string().contains("default profile has no shim"));
}

#[test]
fn an_explicit_shim_name_replaces_the_default_file_name() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "work", "shim_name = \"w\"\n");

    let plan = plan_shims(&directories, None).expect("plan");

    assert_eq!(planned_file_names(&plan), vec!["w".to_string()]);
}

#[test]
fn a_shim_name_of_ylva_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "work", "shim_name = \"ylva\"\n");

    let error = plan_shims(&directories, None).expect_err("ylva shim name");

    assert!(error.to_string().contains("invalid shim_name"));
}

#[test]
fn a_shim_name_with_a_path_separator_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "work", "shim_name = \"../escape\"\n");

    assert!(plan_shims(&directories, None).is_err());
}

#[test]
fn a_shim_name_that_is_not_a_string_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "work", "shim_name = 3\n");

    assert!(plan_shims(&directories, None).is_err());
}

#[test]
fn unparseable_config_fails_the_plan_even_for_another_profile() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("work")).expect("create profile");
    profile_config(&directories, "broken", "this is = = not toml\n");

    let error = plan_shims(&directories, None).expect_err("broken config");

    assert!(error.to_string().contains("not valid TOML"));
}

#[test]
fn two_profiles_claiming_one_shim_name_fail_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "alpha", "shim_name = \"shared\"\n");
    profile_config(&directories, "beta", "shim_name = \"shared\"\n");

    let error = plan_shims(&directories, None).expect_err("duplicate claim");

    let message = error.to_string();
    assert!(
        message.contains("alpha") && message.contains("beta"),
        "{message}"
    );
}

#[test]
fn a_profile_directory_with_an_invalid_name_is_validated_but_not_claimed() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    let invalid_directory = directories.profiles_root.join("bad.name");
    std::fs::create_dir_all(&invalid_directory).expect("create profile");
    write_file(
        &invalid_directory.join("config.toml"),
        "shim_name = \"ylva\"\n",
    );

    let plan = plan_shims(&directories, None).expect("invalid-name profiles are not claimed");
    assert!(plan.shims.is_empty());

    write_file(&invalid_directory.join("config.toml"), "not = = toml\n");
    assert!(plan_shims(&directories, None).is_err());
}

#[test]
fn a_pending_profile_is_planned_before_its_config_is_written() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    let pending = PendingProfile {
        name: "new".to_string(),
        config_text: "shim_name = \"fresh\"\n".to_string(),
    };

    let plan = plan_shims(&directories, Some(&pending)).expect("plan");

    assert_eq!(planned_file_names(&plan), vec!["fresh".to_string()]);
    assert!(!directories.profiles_root.join("new").exists());
}

#[test]
fn a_pending_profile_with_a_duplicate_claim_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    profile_config(&directories, "existing", "shim_name = \"taken\"\n");
    let pending = PendingProfile {
        name: "new".to_string(),
        config_text: "shim_name = \"taken\"\n".to_string(),
    };

    assert!(plan_shims(&directories, Some(&pending)).is_err());
}

#[test]
fn unclaimed_regular_files_are_pruned_and_subdirectories_are_kept() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("work")).expect("create profile");
    std::fs::create_dir_all(directories.shims_directory.join("keep-dir")).expect("create dir");
    write_file(&directories.shims_directory.join("ylva-old"), "stale");
    write_file(&directories.shims_directory.join("notes.txt"), "stale");

    let plan = plan_shims(&directories, None).expect("plan");

    assert_eq!(
        plan.prunes,
        vec![
            directories.shims_directory.join("notes.txt"),
            directories.shims_directory.join("ylva-old"),
        ]
    );
}

#[test]
fn a_claimed_path_that_is_a_directory_fails_the_plan() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("work")).expect("create profile");
    std::fs::create_dir_all(directories.shims_directory.join("ylva-work")).expect("create dir");

    let error = plan_shims(&directories, None).expect_err("directory in the way");

    assert!(error.to_string().contains("not a regular file"));
}

#[test]
fn the_home_script_only_forwards_arguments() {
    let script = render_script(ShimTemplate::Home, "work");

    assert_eq!(
        script, "#!/bin/sh\nexec ylva --profile 'work' \"$@\"\n",
        "a home shim must not set YLVA_HOME or change directory"
    );
}

#[test]
fn the_workspace_script_changes_to_the_workspace_root() {
    let script = render_script(ShimTemplate::Workspace, "work");

    assert!(script.contains(r#"cd "$(dirname "$0")/../.." || exit 2"#));
    assert!(script.ends_with("exec ylva --profile 'work' \"$@\"\n"));
}

#[cfg(unix)]
#[test]
fn apply_writes_executable_shims_and_removes_pruned_files() {
    use std::os::unix::fs::PermissionsExt;

    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());
    std::fs::create_dir_all(directories.profiles_root.join("work")).expect("create profile");
    write_file(&directories.shims_directory.join("stale"), "stale");
    let plan = plan_shims(&directories, None).expect("plan");

    apply_shim_plan(&directories, &plan).expect("apply");

    let shim_path = directories.shims_directory.join("ylva-work");
    let mode = std::fs::metadata(&shim_path)
        .expect("shim metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert!(!directories.shims_directory.join("stale").exists());
}

#[test]
fn apply_with_nothing_to_write_does_not_create_the_shims_directory() {
    let temporary_directory = tempfile::tempdir().expect("tempdir");
    let directories = home_directories(temporary_directory.path());

    apply_shim_plan(&directories, &ShimPlan::default()).expect("apply");

    assert!(!directories.shims_directory.exists());
}

#[test]
fn config_text_with_shim_name_puts_the_key_before_any_table() {
    let text = config_text_with_shim_name("[section]\nkey = 1\n", "w");

    assert_eq!(text, "shim_name = \"w\"\n[section]\nkey = 1\n");
    let table: toml::Table = toml::from_str(&text).expect("valid toml");
    assert_eq!(
        table.get(SHIM_NAME_KEY).and_then(|value| value.as_str()),
        Some("w")
    );
}

#[test]
fn is_on_path_detects_a_directory_listed_in_path() {
    let shims_directory = Path::new("/home/u/.ylva/shims");
    let path_variable = OsString::from("/usr/bin:/home/u/.ylva/shims");

    assert!(is_on_path(shims_directory, Some(&path_variable)));
}

#[test]
fn is_on_path_is_false_for_a_directory_missing_from_path() {
    let shims_directory = Path::new("/home/u/.ylva/shims");
    let path_variable = OsString::from("/usr/bin");

    assert!(!is_on_path(shims_directory, Some(&path_variable)));
    assert!(!is_on_path(shims_directory, None));
}

#[test]
fn path_export_line_exports_a_directory_missing_from_path() {
    let shims_directory = Path::new("/home/u/.ylva/shims");

    assert_eq!(
        path_export_line(shims_directory),
        "export PATH=\"/home/u/.ylva/shims:$PATH\""
    );
}

#[test]
fn path_export_line_quotes_a_directory_with_special_characters() {
    let shims_directory = Path::new("/home/u/my projects/it's/.ylva/shims");

    assert_eq!(
        path_export_line(shims_directory),
        r#"export PATH='/home/u/my projects/it'\''s/.ylva/shims':"$PATH""#
    );
}

#[test]
fn export_line_leaves_the_tilde_of_a_home_relative_plain_directory_unquoted() {
    assert_eq!(
        export_path_line("~/.ylva/shims"),
        r#"export PATH=~/.ylva/shims:"$PATH""#
    );
}

#[test]
fn export_line_quotes_only_the_part_after_a_home_relative_tilde() {
    assert_eq!(
        export_path_line("~/my projects/shims"),
        r#"export PATH=~/'my projects/shims':"$PATH""#
    );
}

#[test]
fn path_export_line_shows_the_real_home_directory_as_tilde() {
    let home_directory = dirs::home_dir().expect("home directory");
    let shims_directory = home_directory.join(".ylva").join("shims");

    assert_eq!(
        path_export_line(&shims_directory),
        r#"export PATH=~/.ylva/shims:"$PATH""#
    );
}
