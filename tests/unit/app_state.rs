//! Unit suite for [`crate::app_state`].
//!
//! The only logic here is where the application puts its data, which on Linux is
//! a two-variable precedence rule. The rule itself is tested through
//! [`linux_data_dir`], which takes the variables as arguments; `app_data_dir` is
//! then checked against whatever the running session actually sets, which is all
//! that can be asserted about it without writing to the environment.

use super::*;

#[cfg(target_os = "linux")]
mod linux_precedence {
    use super::*;

    #[test]
    fn uses_the_data_home_the_session_set() {
        assert_eq!(
            linux_data_dir(
                Some(PathBuf::from("/xdg/data")),
                Some(PathBuf::from("/home/user")),
                "com.streigen.echelon"
            ),
            PathBuf::from("/xdg/data/com.streigen.echelon")
        );
    }

    #[test]
    fn falls_back_to_the_spec_default_under_the_home_directory() {
        assert_eq!(
            linux_data_dir(
                None,
                Some(PathBuf::from("/home/user")),
                "com.streigen.echelon"
            ),
            PathBuf::from("/home/user/.local/share/com.streigen.echelon")
        );
    }

    #[test]
    fn prefers_the_data_home_even_when_a_home_directory_is_also_set() {
        // Both are set on a normal desktop session. Reading them the other way
        // round would ignore a user who relocated their data directory.
        assert_eq!(
            linux_data_dir(
                Some(PathBuf::from("/xdg/data")),
                Some(PathBuf::from("/home/user")),
                "app"
            ),
            PathBuf::from("/xdg/data/app")
        );
    }

    #[test]
    fn does_not_need_a_home_directory_when_the_data_home_is_set() {
        // A service unit can run with `XDG_DATA_HOME` but no `HOME`. Demanding
        // both would panic on a machine that is perfectly well configured.
        assert_eq!(
            linux_data_dir(Some(PathBuf::from("/xdg/data")), None, "app"),
            PathBuf::from("/xdg/data/app")
        );
    }

    #[test]
    #[should_panic(expected = "HOME not set")]
    fn refuses_to_guess_when_neither_variable_is_set() {
        // There is no sensible default here, and picking one silently would put
        // the account database somewhere the next run does not look.
        linux_data_dir(None, None, "app");
    }
}

#[test]
fn the_directory_is_named_after_the_application() {
    // Two applications sharing a data directory would share an account database.
    assert!(
        app_data_dir("com.streigen.echelon").ends_with("com.streigen.echelon"),
        "the app id has to be the last component"
    );
}

#[test]
fn the_same_app_id_always_resolves_to_the_same_place() {
    // Called once at startup and again wherever a path is needed; a result that
    // varied between calls would split the account's state across directories.
    assert_eq!(
        app_data_dir("com.streigen.echelon"),
        app_data_dir("com.streigen.echelon")
    );
}
