//! Unit suite for [`crate::storage::stronghold_backend`].
//!
//! Two callers share this: the app-level store and the per-user secret service.
//! Between them they cover the happy paths, so what is exercised here is the
//! sorting of failures that reach it — a snapshot that is not there yet, one
//! that will not decrypt, and one that decrypts but holds nothing for the client
//! being asked for. Only the first of those is a normal state of affairs; the
//! other two have to be reported rather than silently treated as "no data",
//! which would hand the user an empty account list and then overwrite the
//! snapshot that still held it.

use super::*;
use tempfile::TempDir;

/// A key provider derived from `passphrase`.
///
/// # Arguments
/// * `passphrase` - The passphrase to hash into a key.
fn key(passphrase: &str) -> KeyProvider {
    KeyProvider::with_passphrase_hashed_blake2b(passphrase.to_owned())
        .expect("a passphrase should hash into a key")
}

/// A directory and a snapshot path inside it that nothing has written yet.
fn empty_snapshot() -> (TempDir, SnapshotPath) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = SnapshotPath::from_path(dir.path().join("snapshot"));
    (dir, path)
}

/// The error from an open that is expected to fail.
///
/// [`iota_stronghold::Store`] is not `Debug`, so the `Result` helper that would
/// report an unexpected success is unavailable.
///
/// # Arguments
/// * `result` - The outcome to unwrap.
fn expect_error(result: Result<Option<(Stronghold, iota_stronghold::Store)>>) -> anyhow::Error {
    match result {
        Ok(Some(_)) => panic!("expected the open to fail, got a store"),
        Ok(None) => panic!("expected the open to fail, got no store"),
        Err(error) => error,
    }
}

/// Write a snapshot holding one client with one key in its store.
///
/// # Arguments
/// * `key_provider` - The key to encrypt the snapshot with.
/// * `path` - Where to write it.
/// * `client_name` - The client to create inside it.
fn write_snapshot(key_provider: &KeyProvider, path: &SnapshotPath, client_name: &str) {
    let (stronghold, store) = open_store(key_provider, path, client_name, true)
        .expect("creating a snapshot should succeed")
        .expect("creating is allowed, so there should be a store");
    store
        .insert(b"key".to_vec(), b"value".to_vec(), None)
        .expect("inserting into a fresh store should succeed");
    commit_store(&stronghold, key_provider, path).expect("committing should succeed");
}

mod missing_snapshot {
    use super::*;

    #[test]
    fn reports_nothing_when_the_snapshot_does_not_exist_and_may_not_be_created() {
        // A user who has never signed in has no snapshot. That is not a
        // failure, it is the state every fresh install starts in.
        let (_dir, path) = empty_snapshot();

        let opened = open_store(&key("passphrase"), &path, "client", false)
            .expect("a missing snapshot is not an error");

        assert!(opened.is_none());
    }

    #[test]
    fn creates_the_snapshot_when_it_is_allowed_to() {
        let (_dir, path) = empty_snapshot();

        let opened = open_store(&key("passphrase"), &path, "client", true)
            .expect("creating should succeed");

        assert!(opened.is_some());
    }

    #[test]
    fn nothing_is_written_to_disk_until_the_store_is_committed() {
        // Opening for creation only sets things up in memory. A snapshot that
        // appeared on open would leave an empty file behind for a session that
        // never stored anything.
        let (_dir, path) = empty_snapshot();

        open_store(&key("passphrase"), &path, "client", true).expect("creating should succeed");

        assert!(!path.as_path().exists());
    }
}

mod existing_snapshot {
    use super::*;

    #[test]
    fn reads_back_what_was_committed() {
        let (_dir, path) = empty_snapshot();
        write_snapshot(&key("passphrase"), &path, "client");

        let (_stronghold, store) = open_store(&key("passphrase"), &path, "client", false)
            .expect("reopening should succeed")
            .expect("the snapshot exists and holds the client");

        assert_eq!(
            store.get(b"key").expect("reading should succeed"),
            Some(b"value".to_vec())
        );
    }

    #[test]
    fn refuses_a_snapshot_it_cannot_decrypt() {
        // The keyring handed back a key that does not open this file. Treating
        // that as "no data" would let the caller carry on and commit an empty
        // snapshot over the user's real one.
        let (_dir, path) = empty_snapshot();
        write_snapshot(&key("the-right-passphrase"), &path, "client");

        expect_error(open_store(
            &key("the-wrong-passphrase"),
            &path,
            "client",
            false,
        ));
    }

    #[test]
    fn refuses_a_snapshot_it_cannot_decrypt_even_when_creating_is_allowed() {
        // Being allowed to create only covers a snapshot that is not there.
        // Once the file exists, a key that does not open it is a failure
        // whichever way the caller asked.
        let (_dir, path) = empty_snapshot();
        write_snapshot(&key("the-right-passphrase"), &path, "client");

        expect_error(open_store(
            &key("the-wrong-passphrase"),
            &path,
            "client",
            true,
        ));
    }

    #[test]
    fn reports_nothing_for_a_client_the_snapshot_does_not_hold() {
        // The two callers keep separate clients inside their own snapshots, so
        // asking for one that was never created is the same situation as a
        // snapshot that does not exist yet.
        let (_dir, path) = empty_snapshot();
        write_snapshot(&key("passphrase"), &path, "written-client");

        let opened = open_store(&key("passphrase"), &path, "other-client", false)
            .expect("a missing client is not an error");

        assert!(opened.is_none());
    }

    #[test]
    fn creates_a_client_the_snapshot_does_not_hold_when_it_is_allowed_to() {
        let (_dir, path) = empty_snapshot();
        write_snapshot(&key("passphrase"), &path, "written-client");

        let opened = open_store(&key("passphrase"), &path, "other-client", true)
            .expect("creating a client should succeed")
            .expect("creating is allowed, so there should be a store");

        assert_eq!(
            opened.1.get(b"key").expect("reading should succeed"),
            None,
            "the new client should not see the other client's data"
        );
    }
}
