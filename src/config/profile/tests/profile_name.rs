use super::*;

#[test]
fn profile_name_rule_accepts_letters_digits_underscores_and_hyphens() {
    let longest = "a".repeat(MAX_PROFILE_NAME_LENGTH);
    for name in ["default", "work-2", "a_b", "9lives", longest.as_str()] {
        assert!(is_valid_profile_name(name), "{name:?} must be accepted");
    }
}

#[test]
fn profile_name_rule_rejects_names_that_are_not_a_single_safe_component() {
    let too_long = "a".repeat(MAX_PROFILE_NAME_LENGTH + 1);
    for name in [
        "",
        "-x",
        "_x",
        "../x",
        "a/b",
        "a\\b",
        ".",
        "..",
        "has space",
        "my.profile",
        "naïve",
        too_long.as_str(),
    ] {
        assert!(!is_valid_profile_name(name), "{name:?} must be rejected");
    }
}
