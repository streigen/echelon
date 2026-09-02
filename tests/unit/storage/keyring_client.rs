//! Unit suite for [`crate::storage::keyring_client`].
//!
//! This is the only place the app talks to the OS credential store, and the
//! interesting behaviour is what it does when that store does not cooperate. A
//! missing entry is normal and is filled in; anything else is a failure the user
//! has to be told about, because it means their stored session cannot be
//! decrypted until they unlock something.
//!
//! Failures are arranged with [`fail_next_keyring_call`], which arms the mock
//! credential store to return one error. Each case uses its own service name, so
//! an armed failure stays inside the case that armed it.

use super::*;
use crate::test_support::*;

/// A client over a service name unique to the calling case.
///
/// # Arguments
/// * `name` - A label unique to the calling test.
fn client(name: &str) -> (String, KeyringClient) {
    install_mock_keyring();
    let service = format!("echelon-test-{name}");
    (service.clone(), KeyringClient::new(service))
}

mod get_or_create_password {
    use super::*;

    #[test]
    fn creates_a_password_when_the_entry_does_not_exist_yet() {
        // First run of the app on a machine. There is nothing to read, and
        // failing here would mean the user could never sign in at all.
        let (_service, keyring) = client("keyring-create");

        let password = keyring
            .get_or_create_password("account")
            .expect("a missing entry should be filled in");

        assert_eq!(
            password.len(),
            32,
            "the generated secret should be 32 chars"
        );
    }

    #[test]
    fn returns_the_same_password_on_every_later_call() {
        // This password decrypts the stronghold snapshot. A second call handing
        // back a different one would lock the user out of their own session.
        let (_service, keyring) = client("keyring-stable");

        let first = keyring
            .get_or_create_password("account")
            .expect("the entry should be created");
        let second = keyring
            .get_or_create_password("account")
            .expect("the entry should be read back");

        assert_eq!(*first, *second);
    }

    #[test]
    fn gives_each_account_its_own_password() {
        let (_service, keyring) = client("keyring-per-account");

        let alice = keyring
            .get_or_create_password("alice")
            .expect("the entry should be created");
        let bob = keyring
            .get_or_create_password("bob")
            .expect("the entry should be created");

        assert_ne!(*alice, *bob);
    }

    #[test]
    fn reports_a_store_that_cannot_be_read() {
        // A locked keyring is not a missing entry: generating a fresh password
        // here would overwrite the key to a snapshot the user can still open
        // once they unlock, and lose them their session for good.
        let (service, keyring) = client("keyring-read-fails");
        fail_next_keyring_call(&service, "account", keyring_locked());

        let error = keyring
            .get_or_create_password("account")
            .expect_err("a locked store should be reported");

        assert!(
            error
                .to_string()
                .starts_with("Failed to get password from keyring"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn recovers_once_the_store_is_readable_again() {
        // The injected failure is one-shot, which is what a keyring that was
        // locked and has since been unlocked looks like.
        let (service, keyring) = client("keyring-read-recovers");
        fail_next_keyring_call(&service, "account", keyring_locked());

        keyring
            .get_or_create_password("account")
            .expect_err("the first call should fail");
        let password = keyring
            .get_or_create_password("account")
            .expect("the second call should succeed");

        assert_eq!(password.len(), 32);
    }
}

mod key_provider {
    use super::*;

    #[test]
    fn derives_a_provider_from_the_stored_password() {
        let (_service, keyring) = client("keyring-provider");

        keyring
            .key_provider("account")
            .expect("a provider should be derivable");
    }

    #[test]
    fn is_stable_across_calls() {
        // The provider is what decrypts the snapshot, so two calls that derived
        // different keys would make the second one fail to open the first's
        // file.
        let (_service, keyring) = client("keyring-provider-stable");

        let first = keyring
            .key_provider("account")
            .expect("a provider should be derivable");
        let second = keyring
            .key_provider("account")
            .expect("a provider should be derivable");

        assert_eq!(
            first.try_unlock().expect("the key should be readable"),
            second.try_unlock().expect("the key should be readable")
        );
    }

    #[test]
    fn propagates_a_keyring_failure_rather_than_deriving_from_nothing() {
        let (service, keyring) = client("keyring-provider-fails");
        fail_next_keyring_call(&service, "account", keyring_locked());

        let error = keyring
            .key_provider("account")
            .expect_err("a locked store should be reported");

        assert!(
            error
                .to_string()
                .starts_with("Failed to get password from keyring"),
            "unexpected error: {error}"
        );
    }
}

mod delete_password {
    use super::*;

    #[test]
    fn removes_an_entry_that_exists() {
        let (_service, keyring) = client("keyring-delete");
        let before = keyring
            .get_or_create_password("account")
            .expect("the entry should be created");

        keyring
            .delete_password("account")
            .expect("deleting an existing entry should succeed");

        let after = keyring
            .get_or_create_password("account")
            .expect("a fresh entry should be created");
        assert_ne!(
            *before, *after,
            "the old password should not have survived the delete"
        );
    }

    #[test]
    fn deleting_an_entry_that_was_never_created_is_not_an_error() {
        // Signing out of an account whose keyring entry is already gone has
        // nothing left to do, and reporting that as a failure would stop the
        // rest of the logout.
        let (_service, keyring) = client("keyring-delete-missing");

        keyring
            .delete_password("never-created")
            .expect("deleting nothing should succeed");
    }

    #[test]
    fn deleting_twice_is_not_an_error() {
        let (_service, keyring) = client("keyring-delete-twice");
        keyring
            .get_or_create_password("account")
            .expect("the entry should be created");

        keyring
            .delete_password("account")
            .expect("the first delete should succeed");
        keyring
            .delete_password("account")
            .expect("the second delete should succeed");
    }

    #[test]
    fn reports_a_store_that_refuses_the_delete() {
        let (service, keyring) = client("keyring-delete-fails");
        keyring
            .get_or_create_password("account")
            .expect("the entry should be created");
        fail_next_keyring_call(&service, "account", keyring_locked());

        let error = keyring
            .delete_password("account")
            .expect_err("a locked store should be reported");

        assert!(
            error
                .to_string()
                .starts_with("Failed to delete password from keyring"),
            "unexpected error: {error}"
        );
    }
}
