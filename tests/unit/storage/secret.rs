//! Unit suite for [`crate::storage::secret`].
//!
//! Each case gets its own directory and its own keyring service name, so the
//! per-user snapshots never overlap even though the mock credential store is
//! process-wide.

use super::*;
use crate::test_support::*;

/// A session for `user_id` with fixed tokens.
///
/// # Arguments
/// * `user_id` - The account the session belongs to.
/// * `refresh_token` - The refresh token, if the session has one.
fn session(user_id: &str, refresh_token: Option<&str>) -> Session {
    session_with(user_id, "access-token", refresh_token)
}

/// A session for `user_id` carrying a specific access token.
///
/// Spelled out rather than built with struct update syntax from [`session`]:
/// `Session` zeroizes its tokens on drop, and a type that implements `Drop`
/// cannot have its fields moved out of.
///
/// # Arguments
/// * `user_id` - The account the session belongs to.
/// * `access_token` - The access token to store.
/// * `refresh_token` - The refresh token, if the session has one.
fn session_with(user_id: &str, access_token: &str, refresh_token: Option<&str>) -> Session {
    Session {
        user_id: user_id.to_owned(),
        device_id: "DEVICE1".to_owned(),
        access_token: access_token.to_owned(),
        refresh_token: refresh_token.map(ToOwned::to_owned),
    }
}

mod identifiers {
    use super::*;

    #[test]
    fn hashes_a_user_id_the_same_way_every_time() {
        // The hash names both the keyring entry and the snapshot file, so an
        // unstable one would lose the user their stored session.
        assert_eq!(
            SecretService::user_id_hash("@alice:example.org"),
            SecretService::user_id_hash("@alice:example.org")
        );
    }

    #[test]
    fn gives_different_users_different_hashes() {
        assert_ne!(
            SecretService::user_id_hash("@alice:example.org"),
            SecretService::user_id_hash("@bob:example.org")
        );
    }

    #[test]
    fn produces_a_filesystem_safe_hash() {
        // A user id contains characters a path cannot, which is the reason for
        // hashing it rather than using it directly.
        let hash = SecretService::user_id_hash("@alice:example.org");

        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!hash.is_empty());
    }
}

mod random_secret {
    use super::*;

    #[test]
    fn is_thirty_two_alphanumeric_characters() {
        let secret = SecretService::random_secret();

        assert_eq!(secret.len(), 32);
        assert!(secret.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn differs_between_calls() {
        // The user never picks these, so every one of them has to come from the
        // generator rather than from a seed shared across calls.
        let first = SecretService::random_secret();
        let second = SecretService::random_secret();

        assert_ne!(*first, *second);
    }
}

mod sessions {
    use super::*;

    #[test]
    fn stores_and_returns_a_session() {
        let (_dir, secrets) = temp_secret_service("session-round-trip");

        secrets
            .set_session(&session("@alice:example.org", Some("refresh-token")))
            .expect("storing should succeed");
        let stored = secrets
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("the session was just stored");

        assert_eq!(stored.user_id, "@alice:example.org");
        assert_eq!(stored.device_id, "DEVICE1");
        assert_eq!(stored.access_token, "access-token");
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh-token"));
    }

    #[test]
    fn stores_a_session_that_has_no_refresh_token() {
        let (_dir, secrets) = temp_secret_service("session-no-refresh");

        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");
        let stored = secrets
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("the session was just stored");

        assert_eq!(stored.refresh_token, None);
    }

    #[test]
    fn replacing_a_session_drops_a_refresh_token_it_no_longer_has() {
        // A homeserver that stops issuing refresh tokens must not leave the old
        // one behind to be replayed.
        let (_dir, secrets) = temp_secret_service("session-refresh-cleared");
        secrets
            .set_session(&session("@alice:example.org", Some("refresh-token")))
            .expect("storing should succeed");

        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");

        let stored = secrets
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("the session was just stored");
        assert_eq!(stored.refresh_token, None);
    }

    #[test]
    fn reports_nothing_for_a_user_with_no_snapshot() {
        let (_dir, secrets) = temp_secret_service("session-missing");

        assert!(
            secrets
                .get_session("@nobody:example.org")
                .expect("reading should succeed")
                .is_none()
        );
    }

    #[test]
    fn reports_nothing_when_a_snapshot_holds_no_access_token() {
        // Asking for a sqlite password creates the snapshot without signing
        // anybody in, so a session read against it has to come back empty rather
        // than half-built.
        let (_dir, secrets) = temp_secret_service("session-no-token");
        secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("creating the password should succeed");

        assert!(
            secrets
                .get_session("@alice:example.org")
                .expect("reading should succeed")
                .is_none()
        );
    }

    #[test]
    fn keeps_each_users_session_separate() {
        let (_dir, secrets) = temp_secret_service("session-per-user");
        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");

        secrets
            .set_session(&session_with("@bob:example.org", "bobs-token", None))
            .expect("storing should succeed");

        let alice = secrets
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("alice has a session");
        let bob = secrets
            .get_session("@bob:example.org")
            .expect("reading should succeed")
            .expect("bob has a session");
        assert_eq!(alice.access_token, "access-token");
        assert_eq!(bob.access_token, "bobs-token");
    }

    #[test]
    fn a_later_run_of_the_app_reads_back_the_session() {
        let (dir, secrets) = temp_secret_service("session-persist");
        secrets
            .set_session(&session("@alice:example.org", Some("refresh-token")))
            .expect("storing should succeed");
        drop(secrets);

        let reopened = reopen_secret_service(&dir, "session-persist");

        let stored = reopened
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("the session outlived the service that wrote it");
        assert_eq!(stored.access_token, "access-token");
    }
}

mod sqlite_password {
    use super::*;

    #[test]
    fn returns_the_same_password_on_every_call() {
        // The store is encrypted with this; a second, different password would
        // make the account's database unreadable.
        let (_dir, secrets) = temp_secret_service("sqlite-stable");

        let first = secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("creating should succeed");
        let second = secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("reading should succeed");

        assert_eq!(*first, *second);
    }

    #[test]
    fn survives_a_restart() {
        let (dir, secrets) = temp_secret_service("sqlite-persist");
        let first = secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("creating should succeed")
            .to_string();
        drop(secrets);

        let reopened = reopen_secret_service(&dir, "sqlite-persist");

        let second = reopened
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("reading should succeed");
        assert_eq!(first, *second);
    }

    #[test]
    fn gives_each_user_their_own_password() {
        let (_dir, secrets) = temp_secret_service("sqlite-per-user");

        let alice = secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("creating should succeed");
        let bob = secrets
            .get_or_create_sqlite_pwd("@bob:example.org")
            .expect("creating should succeed");

        assert_ne!(*alice, *bob);
    }

    #[test]
    fn leaves_a_stored_session_alone() {
        let (_dir, secrets) = temp_secret_service("sqlite-keeps-session");
        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");

        secrets
            .get_or_create_sqlite_pwd("@alice:example.org")
            .expect("creating should succeed");

        assert!(
            secrets
                .get_session("@alice:example.org")
                .expect("reading should succeed")
                .is_some()
        );
    }
}

mod deletion {
    use super::*;

    #[test]
    fn removes_the_stored_session() {
        let (_dir, secrets) = temp_secret_service("delete");
        secrets
            .set_session(&session("@alice:example.org", Some("refresh-token")))
            .expect("storing should succeed");

        secrets
            .delete_session("@alice:example.org")
            .expect("deleting should succeed");

        assert!(
            secrets
                .get_session("@alice:example.org")
                .expect("reading should succeed")
                .is_none()
        );
    }

    #[test]
    fn removes_the_snapshot_from_disk() {
        // Logging out has to take the file with it, not just make the app stop
        // reading it.
        let (dir, secrets) = temp_secret_service("delete-file");
        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");
        let snapshot = dir
            .path()
            .join(SecretService::user_id_hash("@alice:example.org"));
        assert!(snapshot.is_file(), "the snapshot should exist to begin with");

        secrets
            .delete_session("@alice:example.org")
            .expect("deleting should succeed");

        assert!(!snapshot.exists());
    }

    #[test]
    fn reports_a_snapshot_that_cannot_be_removed() {
        // Anything other than "it was already gone" has to be reported, because
        // the keyring key is deleted immediately afterwards: carrying on would
        // leave a snapshot on disk that nothing can ever decrypt again.
        //
        // A directory standing where the snapshot belongs is the reachable
        // version of this. A file whose parent denies writes would be the other
        // one, but that stops failing as soon as the tests run as root.
        let (dir, secrets) = temp_secret_service("delete-blocked");
        let snapshot = dir
            .path()
            .join(SecretService::user_id_hash("@alice:example.org"));
        std::fs::create_dir(&snapshot).expect("creating the blocking directory should succeed");

        let error = secrets
            .delete_session("@alice:example.org")
            .expect_err("a snapshot that cannot be removed should be reported");

        assert!(
            error
                .to_string()
                .starts_with("Failed to delete stronghold snapshot at"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn deleting_a_user_who_has_nothing_stored_is_not_an_error() {
        // Logout runs this even when the session was never written, so a missing
        // snapshot has to be the normal case rather than a failure.
        let (_dir, secrets) = temp_secret_service("delete-missing");

        secrets
            .delete_session("@nobody:example.org")
            .expect("deleting nothing should succeed");
    }

    #[test]
    fn leaves_other_users_alone() {
        let (_dir, secrets) = temp_secret_service("delete-other");
        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");
        secrets
            .set_session(&session("@bob:example.org", None))
            .expect("storing should succeed");

        secrets
            .delete_session("@alice:example.org")
            .expect("deleting should succeed");

        assert!(
            secrets
                .get_session("@bob:example.org")
                .expect("reading should succeed")
                .is_some()
        );
    }

    #[test]
    fn a_user_can_sign_in_again_after_deletion() {
        let (_dir, secrets) = temp_secret_service("delete-then-readd");
        secrets
            .set_session(&session("@alice:example.org", None))
            .expect("storing should succeed");
        secrets
            .delete_session("@alice:example.org")
            .expect("deleting should succeed");

        secrets
            .set_session(&session_with("@alice:example.org", "second-token", None))
            .expect("storing again should succeed");

        let stored = secrets
            .get_session("@alice:example.org")
            .expect("reading should succeed")
            .expect("the new session is stored");
        assert_eq!(stored.access_token, "second-token");
    }
}
