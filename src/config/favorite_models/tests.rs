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

const CONNECTION: &str = "dummy";

struct Fixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    writer: FavoriteModelsWriter,
}

fn fixture(contents: &str) -> Fixture {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("config.toml");
    fs::write(&path, contents).expect("write config");
    let writer = FavoriteModelsWriter::from_loaded_source(
        path.clone(),
        contents.to_string(),
        FavoriteSourceLayer::Home,
    );
    Fixture {
        _directory: directory,
        path,
        writer,
    }
}

fn read(fixture: &Fixture) -> String {
    fs::read_to_string(&fixture.path).expect("read config")
}

fn parsed_favorites(contents: &str, section: &str) -> Vec<String> {
    let value: toml::Value = toml::from_str(contents).expect("valid toml");
    value[section][CONNECTION]["favorite_models"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().expect("string").to_string())
                .collect()
        })
        .unwrap_or_default()
}

const BASIC: &str = r#"# profile header
default_model = "dummy/a"

[provider_connections.dummy]
# connection comment
provider = "dummy"
api_key = "${DUMMY_API_KEY}"
unknown_future_field = { nested = 1 }

favorite_models = [
    "a/one", # keep me
    "b/two",
]
allowed_models = ["a/*"]

[other]
value = 1
"#;

#[test]
fn add_appends_new_id_preserving_order_and_everything_else() {
    let mut fixture = fixture(BASIC);

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("save");

    assert!(outcome.changed);
    assert!(!outcome.publication_uncertain);
    assert_eq!(outcome.layer, FavoriteSourceLayer::Home);
    assert_eq!(outcome.path, fixture.path);
    let expected = BASIC.replace("    \"b/two\",\n]", "    \"b/two\",\n    \"c/three\",\n]");
    assert_eq!(read(&fixture), expected);
}

#[test]
fn add_creates_the_key_in_an_existing_table_without_it() {
    let source = "[provider_connections.dummy]\nprovider = \"dummy\" # c\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "a/one", true)
        .expect("save");

    let contents = read(&fixture);
    assert!(contents.starts_with(source), "{contents}");
    assert_eq!(
        parsed_favorites(&contents, "provider_connections"),
        vec!["a/one"]
    );
}

#[test]
fn adding_an_existing_id_is_an_idempotent_no_op() {
    let mut fixture = fixture(BASIC);

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "b/two", true)
        .expect("save");

    assert!(!outcome.changed);
    assert_eq!(read(&fixture), BASIC);
}

#[test]
fn membership_is_case_sensitive() {
    let mut fixture = fixture(BASIC);

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "B/TWO", true)
        .expect("save");
    assert!(outcome.changed);
    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["a/one", "b/two", "B/TWO"]
    );

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "A/ONE", false)
        .expect("save");
    assert!(!outcome.changed);
    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["a/one", "b/two", "B/TWO"]
    );
}

#[test]
fn removal_deletes_every_exact_duplicate_and_keeps_other_entries() {
    let source =
        "[provider_connections.dummy]\nfavorite_models = [\"x\", \"y\", \"x\", \"X\", \"x\"]\n";
    let mut fixture = fixture(source);

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "x", false)
        .expect("save");

    assert!(outcome.changed);
    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["y", "X"]
    );
}

#[test]
fn removing_the_last_entry_removes_the_key_and_keeps_the_rest() {
    let source = "# top\n[provider_connections.dummy]\nprovider = \"dummy\"\nfavorite_models = [\"x\", \"x\"]\nallowed_models = [\"*\"]\n\n[other]\nk = 1\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "x", false)
        .expect("save");

    assert_eq!(
        read(&fixture),
        "# top\n[provider_connections.dummy]\nprovider = \"dummy\"\nallowed_models = [\"*\"]\n\n[other]\nk = 1\n"
    );
}

#[test]
fn removing_from_a_missing_key_is_a_no_op() {
    let source = "[provider_connections.dummy]\nprovider = \"dummy\"\n";
    let mut fixture = fixture(source);

    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "x", false)
        .expect("save");

    assert!(!outcome.changed);
    assert_eq!(read(&fixture), source);
}

#[test]
fn interpolation_entries_and_credentials_stay_untouched() {
    let source = "[provider_connections.dummy]\napi_key = \"${DUMMY_API_KEY}\"\nfavorite_models = [\"${FAVORITE}\", \"a\", \"b\"]\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "a", false)
        .expect("remove literal");
    fixture
        .writer
        .set_favorite(CONNECTION, "c", true)
        .expect("add");

    assert_eq!(
        read(&fixture),
        "[provider_connections.dummy]\napi_key = \"${DUMMY_API_KEY}\"\nfavorite_models = [\"${FAVORITE}\", \"b\", \"c\"]\n"
    );
}

#[test]
fn removal_is_refused_when_only_an_interpolation_might_expand_to_the_id() {
    let source = "[provider_connections.dummy]\nfavorite_models = [\"${FAVORITE}\", \"a\"]\n";
    let mut fixture = fixture(source);

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "expanded/model", false)
        .expect_err("ambiguous");

    assert!(matches!(
        error,
        FavoriteSaveError::AmbiguousInterpolation { .. }
    ));
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("expanded/model"));
    assert_eq!(read(&fixture), source);
}

#[test]
fn adding_next_to_an_interpolation_entry_is_allowed() {
    let source = "[provider_connections.dummy]\nfavorite_models = [\"${FAVORITE}\"]\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "m", true)
        .expect("save");

    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["${FAVORITE}", "m"]
    );
}

#[test]
fn inline_connection_tables_are_supported() {
    let source = "[provider_connections]\ndummy = { provider = \"dummy\", favorite_models = [\"a\"] } # trailing\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "b", true)
        .expect("add");
    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["a", "b"]
    );
    assert!(read(&fixture).contains("# trailing"));

    fixture
        .writer
        .set_favorite(CONNECTION, "a", false)
        .expect("remove");
    fixture
        .writer
        .set_favorite(CONNECTION, "b", false)
        .expect("remove last");
    let contents = read(&fixture);
    assert!(!contents.contains("favorite_models"), "{contents}");
    assert!(contents.contains("provider = \"dummy\""));
}

#[test]
fn inline_connection_table_without_the_key_gets_it() {
    let source = "[provider_connections]\ndummy = { provider = \"dummy\" }\n";
    let mut fixture = fixture(source);

    fixture
        .writer
        .set_favorite(CONNECTION, "a", true)
        .expect("save");

    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["a"]
    );
}

#[test]
fn dotted_key_connection_tables_are_supported() {
    for source in [
        "[provider_connections]\ndummy.provider = \"dummy\"\ndummy.favorite_models = [\"a\"]\n",
        "provider_connections.dummy.provider = \"dummy\"\nprovider_connections.dummy.favorite_models = [\"a\"]\n",
    ] {
        let mut fixture = fixture(source);

        fixture
            .writer
            .set_favorite(CONNECTION, "b", true)
            .expect("save");

        assert_eq!(
            parsed_favorites(&read(&fixture), "provider_connections"),
            vec!["a", "b"],
            "{}",
            read(&fixture)
        );
    }
}

#[test]
fn missing_backing_table_is_reported_and_nothing_is_created() {
    for source in [
        "default_model = \"x\"\n",
        "[provider_connections.other]\nprovider = \"dummy\"\n",
    ] {
        let mut fixture = fixture(source);

        let error = fixture
            .writer
            .set_favorite(CONNECTION, "a", true)
            .expect_err("missing");

        assert!(matches!(
            error,
            FavoriteSaveError::MissingConnectionTable { .. }
        ));
        assert!(!error.is_retryable());
        assert_eq!(read(&fixture), source);
    }
}

#[test]
fn non_array_or_non_string_favorites_are_unsupported() {
    for source in [
        "[provider_connections.dummy]\nfavorite_models = \"a\"\n",
        "[provider_connections.dummy]\nfavorite_models = [1]\n",
    ] {
        let mut fixture = fixture(source);

        let error = fixture
            .writer
            .set_favorite(CONNECTION, "a", true)
            .expect_err("unsupported");

        assert!(matches!(error, FavoriteSaveError::Unsupported { .. }));
        assert_eq!(read(&fixture), source);
    }
}

#[cfg(unix)]
#[test]
fn unix_permission_bits_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o600, 0o644] {
        let mut fixture = fixture(BASIC);
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(mode)).expect("chmod");

        fixture
            .writer
            .set_favorite(CONNECTION, "c/three", true)
            .expect("save");

        let actual = fs::metadata(&fixture.path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(actual, mode);
    }
}

#[cfg(unix)]
#[test]
fn read_only_target_is_retryable_and_leaves_the_file_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = fixture(BASIC);
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o444)).expect("chmod");

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("read-only");

    assert!(error.is_retryable(), "{error}");
    assert_eq!(read(&fixture), BASIC);

    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o644)).expect("chmod");
    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("retry succeeds with an unchanged source");
    assert!(outcome.changed);
}

#[test]
fn disappearance_is_a_source_change() {
    let mut fixture = fixture(BASIC);
    fs::remove_file(&fixture.path).expect("remove");

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("gone");

    assert!(matches!(error, FavoriteSaveError::SourceChanged { .. }));
    assert!(!error.is_retryable());
    assert!(error.to_string().contains("restart Ylva"));
    assert!(!fixture.path.exists(), "no file is recreated");
}

#[test]
fn external_modification_is_a_source_change_and_is_not_overwritten() {
    let mut fixture = fixture(BASIC);
    let edited = format!("{BASIC}# edited elsewhere\n");
    fs::write(&fixture.path, &edited).expect("edit");

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("changed");

    assert!(matches!(error, FavoriteSaveError::SourceChanged { .. }));
    assert_eq!(read(&fixture), edited);
}

#[test]
fn replacement_with_identical_contents_by_a_new_file_is_a_source_change() {
    let mut fixture = fixture(BASIC);
    let replacement = fixture.path.with_extension("replacement");
    fs::write(&replacement, BASIC).expect("replacement");
    fs::rename(&replacement, &fixture.path).expect("replace");

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("replaced");

    assert!(matches!(error, FavoriteSaveError::SourceChanged { .. }));
    assert_eq!(read(&fixture), BASIC);
}

#[cfg(unix)]
#[test]
fn replacement_by_a_symlink_is_a_source_change() {
    let mut fixture = fixture(BASIC);
    let elsewhere = fixture.path.with_extension("elsewhere");
    fs::write(&elsewhere, BASIC).expect("copy");
    fs::remove_file(&fixture.path).expect("remove");
    std::os::unix::fs::symlink(&elsewhere, &fixture.path).expect("symlink");

    let error = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("symlinked");

    assert!(matches!(error, FavoriteSaveError::SourceChanged { .. }));
    assert_eq!(fs::read_to_string(&elsewhere).expect("read"), BASIC);
}

#[test]
fn successful_save_advances_the_snapshot_so_the_next_save_works() {
    let mut fixture = fixture(BASIC);

    fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("first");
    fixture
        .writer
        .set_favorite(CONNECTION, "a/one", false)
        .expect("second");
    fixture
        .writer
        .set_favorite(CONNECTION, "c/three", false)
        .expect("third");

    assert_eq!(
        parsed_favorites(&read(&fixture), "provider_connections"),
        vec!["b/two"]
    );
}

#[test]
fn failed_pre_publication_attempt_keeps_the_snapshot_so_retry_works() {
    let mut fixture = fixture(BASIC);

    let error = fixture
        .writer
        .set_favorite_with_publisher(CONNECTION, "c/three", true, |_, _, _| {
            Err(io::Error::other("disk full"))
        })
        .expect_err("publisher fails before rename");

    assert!(error.is_retryable(), "{error}");
    assert_eq!(read(&fixture), BASIC);
    let outcome = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("retry");
    assert!(outcome.changed);
}

#[test]
fn error_after_rename_reports_uncertainty_and_advances_the_snapshot_when_contents_match() {
    let mut fixture = fixture(BASIC);

    let outcome = fixture
        .writer
        .set_favorite_with_publisher(CONNECTION, "c/three", true, |path, contents, mode| {
            replace_file(path, contents, mode)?;
            Err(io::Error::other("directory sync failed"))
        })
        .expect("published and verified");

    assert!(outcome.changed);
    assert!(outcome.publication_uncertain);
    fixture
        .writer
        .set_favorite(CONNECTION, "a/one", false)
        .expect("snapshot advanced, next save works");
}

#[test]
fn error_after_rename_with_unexpected_contents_is_post_publish_uncertain() {
    let mut fixture = fixture(BASIC);

    let error = fixture
        .writer
        .set_favorite_with_publisher(CONNECTION, "c/three", true, |path, _, mode| {
            replace_file(path, b"something else\n", mode)?;
            Err(io::Error::other("directory sync failed"))
        })
        .expect_err("unexpected contents");

    assert!(matches!(
        error,
        FavoriteSaveError::PostPublishUncertain { .. }
    ));
    assert!(!error.is_retryable());
    let followup = fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect_err("snapshot not advanced");
    assert!(matches!(followup, FavoriteSaveError::SourceChanged { .. }));
}

#[test]
fn saves_leave_no_temporary_files_behind() {
    let mut fixture = fixture(BASIC);
    fixture
        .writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("save");

    let entries = fs::read_dir(fixture.path.parent().expect("parent"))
        .expect("read dir")
        .count();
    assert_eq!(entries, 1);
}

#[test]
fn for_profile_resolves_workspace_over_home_and_reports_the_layer() {
    let home = tempfile::tempdir().expect("home");
    let workspace = tempfile::tempdir().expect("workspace");
    let home_path = home.path().join("default").join("config.toml");
    fs::create_dir_all(home_path.parent().expect("parent")).expect("dir");
    fs::write(&home_path, BASIC).expect("home config");

    let mut writer = FavoriteModelsWriter::for_profile(home.path(), "default", None)
        .expect("resolve")
        .expect("home file");
    assert_eq!(
        writer
            .set_favorite(CONNECTION, "c/three", true)
            .expect("save")
            .layer,
        FavoriteSourceLayer::Home
    );

    let workspace_path = workspace.path().join(".ylva/profiles/default/config.toml");
    fs::create_dir_all(workspace_path.parent().expect("parent")).expect("dir");
    fs::write(&workspace_path, BASIC).expect("workspace config");
    let mut writer =
        FavoriteModelsWriter::for_profile(home.path(), "default", Some(workspace.path()))
            .expect("resolve")
            .expect("workspace file");
    let outcome = writer
        .set_favorite(CONNECTION, "c/three", true)
        .expect("workspace save succeeds");
    assert_eq!(outcome.layer, FavoriteSourceLayer::Workspace);
    assert_eq!(outcome.path, workspace_path);
    assert_eq!(
        parsed_favorites(
            &fs::read_to_string(&workspace_path).expect("read"),
            "provider_connections"
        ),
        vec!["a/one", "b/two", "c/three"]
    );
    assert_eq!(
        fs::read_to_string(&home_path).expect("read home"),
        BASIC.replace("    \"b/two\",\n]", "    \"b/two\",\n    \"c/three\",\n]"),
        "home layer was only modified by its own earlier save"
    );
    assert!(
        !workspace.path().join(".ylva/trust").exists()
            && !workspace
                .path()
                .join(".ylva")
                .read_dir()
                .expect("dir")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .contains("trust")),
        "no trust state is written"
    );
}

#[test]
fn for_profile_without_a_file_has_nothing_to_save_to() {
    let home = tempfile::tempdir().expect("home");

    let writer = FavoriteModelsWriter::for_profile(home.path(), "default", None).expect("resolve");

    assert!(writer.is_none());
}

#[test]
fn built_in_layer_has_no_favorite_layer() {
    assert_eq!(
        FavoriteSourceLayer::from_config_layer(ConfigLayer::BuiltIn),
        None
    );
}
