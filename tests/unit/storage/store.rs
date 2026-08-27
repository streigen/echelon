//! Unit suite for [`crate::storage::store`].
//!
//! Each case gets its own directory and its own keyring entry, so the snapshots
//! never overlap even though the mock credential store is process-wide.

use super::*;
use crate::test_support::*;

/// The user ids of the persisted accounts, in the order they are stored.
///
/// # Arguments
/// * `store` - The store to read.
fn user_ids(store: &EchelonStore) -> Vec<String> {
    store
        .get_accounts()
        .expect("reading accounts should succeed")
        .accounts
        .into_iter()
        .map(|account| account.user_id)
        .collect()
}

/// The homeserver recorded for `user_id`, if the account exists.
///
/// # Arguments
/// * `store` - The store to read.
/// * `user_id` - The account to look up.
fn homeserver_of(store: &EchelonStore, user_id: &str) -> Option<String> {
    store
        .get_account(user_id)
        .expect("reading an account should succeed")
        .and_then(|account| account.homeserver)
}

mod empty_store {
    use super::*;

    #[test]
    fn holds_no_accounts() {
        let (_dir, store) = temp_store("empty");

        let accounts = store.get_accounts().expect("reading should succeed");

        assert!(accounts.accounts.is_empty());
        assert_eq!(accounts.last, None);
    }

    #[test]
    fn reports_no_last_account() {
        let (_dir, store) = temp_store("empty-last");

        assert_eq!(store.get_last().expect("reading should succeed"), None);
    }

    #[test]
    fn finds_no_account_by_id() {
        let (_dir, store) = temp_store("empty-lookup");

        assert!(
            store
                .get_account("@alice:example.org")
                .expect("reading should succeed")
                .is_none()
        );
    }
}

mod adding {
    use super::*;

    #[test]
    fn records_the_account_and_marks_it_last_used() {
        let (_dir, store) = temp_store("add");

        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        assert_eq!(user_ids(&store), ["@alice:example.org"]);
        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://example.org")
        );
        assert_eq!(
            store.get_last().expect("reading should succeed").as_deref(),
            Some("@alice:example.org")
        );
    }

    #[test]
    fn puts_the_newest_account_first() {
        let (_dir, store) = temp_store("add-order");

        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");
        store
            .add_account("@bob:example.org", "https://example.org")
            .expect("adding should succeed");

        assert_eq!(
            user_ids(&store),
            ["@bob:example.org", "@alice:example.org"]
        );
    }

    #[test]
    fn re_adding_an_account_moves_it_rather_than_duplicating_it() {
        let (_dir, store) = temp_store("add-again");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");
        store
            .add_account("@bob:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        assert_eq!(
            user_ids(&store),
            ["@alice:example.org", "@bob:example.org"]
        );
    }

    #[test]
    fn re_adding_an_account_updates_its_homeserver() {
        // Signing in again through a different URL for the same account should
        // leave the account reachable at the URL that actually worked.
        let (_dir, store) = temp_store("add-rehome");
        store
            .add_account("@alice:example.org", "https://old.example.org")
            .expect("adding should succeed");

        store
            .add_account("@alice:example.org", "https://new.example.org")
            .expect("adding should succeed");

        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://new.example.org")
        );
    }
}

mod set_homeserver {
    use super::*;

    #[test]
    fn records_a_discovered_url() {
        let (_dir, store) = temp_store("set-hs");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .set_homeserver("@alice:example.org", "https://matrix.example.org")
            .expect("setting should succeed");

        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://matrix.example.org")
        );
    }

    #[test]
    fn does_nothing_for_an_account_that_was_never_added() {
        let (_dir, store) = temp_store("set-hs-unknown");

        store
            .set_homeserver("@nobody:example.org", "https://example.org")
            .expect("setting an unknown account should not be an error");

        assert!(user_ids(&store).is_empty());
    }

    #[test]
    fn accepts_the_url_it_already_holds() {
        let (_dir, store) = temp_store("set-hs-same");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .set_homeserver("@alice:example.org", "https://example.org")
            .expect("setting the same value should not be an error");

        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://example.org")
        );
    }
}

mod removing {
    use super::*;

    #[test]
    fn drops_the_account() {
        let (_dir, store) = temp_store("remove");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .remove_account("@alice:example.org")
            .expect("removing should succeed");

        assert!(user_ids(&store).is_empty());
    }

    #[test]
    fn promotes_the_next_account_when_the_last_used_one_goes() {
        let (_dir, store) = temp_store("remove-promote");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");
        store
            .add_account("@bob:example.org", "https://example.org")
            .expect("adding should succeed");

        // Bob was added most recently, so he is both first in the list and the
        // last-used account. Removing him has to hand both roles to Alice.
        store
            .remove_account("@bob:example.org")
            .expect("removing should succeed");

        assert_eq!(user_ids(&store), ["@alice:example.org"]);
        assert_eq!(
            store.get_last().expect("reading should succeed").as_deref(),
            Some("@alice:example.org")
        );
    }

    #[test]
    fn clears_the_last_used_account_when_the_only_one_goes() {
        let (_dir, store) = temp_store("remove-only");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .remove_account("@alice:example.org")
            .expect("removing should succeed");

        assert_eq!(store.get_last().expect("reading should succeed"), None);
    }

    #[test]
    fn leaves_the_last_used_account_alone_when_another_one_goes() {
        let (_dir, store) = temp_store("remove-other");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");
        store
            .add_account("@bob:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .remove_account("@alice:example.org")
            .expect("removing should succeed");

        assert_eq!(
            store.get_last().expect("reading should succeed").as_deref(),
            Some("@bob:example.org")
        );
    }

    #[test]
    fn removing_an_unknown_account_changes_nothing() {
        let (_dir, store) = temp_store("remove-unknown");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");

        store
            .remove_account("@nobody:example.org")
            .expect("removing an unknown account should not be an error");

        assert_eq!(user_ids(&store), ["@alice:example.org"]);
        assert_eq!(
            store.get_last().expect("reading should succeed").as_deref(),
            Some("@alice:example.org")
        );
    }
}

mod persistence {
    use super::*;

    #[test]
    fn a_later_run_of_the_app_reads_back_what_was_written() {
        let (dir, store) = temp_store("persist");
        store
            .add_account("@alice:example.org", "https://example.org")
            .expect("adding should succeed");
        drop(store);

        let reopened = reopen_store(&dir, "persist");

        assert_eq!(user_ids(&reopened), ["@alice:example.org"]);
        assert_eq!(
            homeserver_of(&reopened, "@alice:example.org").as_deref(),
            Some("https://example.org")
        );
    }
}

mod legacy_snapshots {
    use super::*;

    /// Overwrite the account list with raw JSON, as an older build would have
    /// left it.
    ///
    /// # Arguments
    /// * `store` - The store to write into.
    /// * `json` - The account list as it should appear on disk.
    fn seed_raw(store: &EchelonStore, json: serde_json::Value) {
        let (stronghold, inner, key_provider) = store.open().expect("opening should succeed");
        inner
            .insert(
                b"accounts".to_vec(),
                serde_json::to_vec(&json).expect("the fixture should serialize"),
                None,
            )
            .expect("writing should succeed");
        store
            .commit(&stronghold, &key_provider)
            .expect("committing should succeed");
    }

    #[test]
    fn an_account_list_of_bare_user_ids_still_loads() {
        // Older builds stored each account as just its user id. Failing to read
        // one of those snapshots would silently sign the user out of every
        // account they had.
        let (_dir, store) = temp_store("legacy");
        seed_raw(
            &store,
            serde_json::json!({
                "last": "@alice:example.org",
                "accounts": ["@alice:example.org", "@bob:example.org"],
            }),
        );

        let accounts = store.get_accounts().expect("reading should succeed");

        assert_eq!(
            user_ids(&store),
            ["@alice:example.org", "@bob:example.org"]
        );
        assert_eq!(accounts.last.as_deref(), Some("@alice:example.org"));
        // A bare id says nothing about where the account lives, so the
        // homeserver has to be rediscovered rather than guessed.
        assert_eq!(homeserver_of(&store, "@alice:example.org"), None);
    }

    #[test]
    fn a_list_mixing_both_shapes_loads() {
        // What a snapshot looks like part-way through the migration: entries
        // rewritten by the current build sit alongside ones that have not been
        // touched since the upgrade.
        let (_dir, store) = temp_store("legacy-mixed");
        seed_raw(
            &store,
            serde_json::json!({
                "last": "@alice:example.org",
                "accounts": [
                    {"user_id": "@alice:example.org", "homeserver": "https://example.org"},
                    "@bob:example.org",
                ],
            }),
        );

        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://example.org")
        );
        assert_eq!(homeserver_of(&store, "@bob:example.org"), None);
    }

    #[test]
    fn a_legacy_account_gains_a_homeserver_once_one_is_known() {
        let (_dir, store) = temp_store("legacy-upgrade");
        seed_raw(
            &store,
            serde_json::json!({"last": null, "accounts": ["@alice:example.org"]}),
        );

        store
            .set_homeserver("@alice:example.org", "https://example.org")
            .expect("setting should succeed");

        assert_eq!(
            homeserver_of(&store, "@alice:example.org").as_deref(),
            Some("https://example.org")
        );
    }
}
