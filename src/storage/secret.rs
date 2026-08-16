use crate::storage::keyring_client::KeyringClient;
use crate::storage::stronghold_backend::{commit_store, open_store};
use anyhow::Result;
use blake3;
use iota_stronghold::{KeyProvider, SnapshotPath, Stronghold};
use rand::distr::{Alphanumeric, SampleString};
use std::path::PathBuf;
use zeroize::{Zeroizing, ZeroizeOnDrop};

/// All per-user session data stored in the stronghold.
///
/// The tokens are wiped when this is dropped. They are bearer credentials for the
/// whole account, so a copy left in freed heap is a copy that can be read out of a
/// core dump or handed to the next allocation. The ids are not secret and are left
/// alone, since zeroizing them would only cost allocations.
#[derive(ZeroizeOnDrop)]
pub struct Session {
    #[zeroize(skip)]
    pub user_id: String,
    #[zeroize(skip)]
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
}

pub struct SecretService {
    /// Shared keyring client for fetching/creating the stronghold encryption key.
    keyring: KeyringClient,
    /// Directory where per-user stronghold snapshot files are kept.
    stronghold_path: PathBuf,
}

impl SecretService {
    pub fn new(keyring: KeyringClient, stronghold_path: PathBuf) -> Self {
        SecretService { keyring, stronghold_path }
    }

    /// Generate a random 32-character alphanumeric string, wiped when dropped.
    ///
    /// `rand::rng()` is a CSPRNG, so 32 alphanumeric characters carry ~190 bits of
    /// entropy. That is the strength of every store password and stronghold key the
    /// app creates, since the user never picks one.
    pub fn random_secret() -> Zeroizing<String> {
        Zeroizing::new(Alphanumeric.sample_string(&mut rand::rng(), 32))
    }

    /// Return the blake3 hex hash of `user_id`, used as both the keyring account
    /// name and the stronghold snapshot filename so each user has isolated secrets.
    pub fn user_id_hash(user_id: &str) -> String {
        blake3::hash(user_id.as_bytes()).to_string()
    }

    /// Return the snapshot path for `user_id` (blake3-hashed to be FS-safe).
    fn snapshot_path(&self, user_id: &str) -> SnapshotPath {
        SnapshotPath::from_path(self.stronghold_path.join(Self::user_id_hash(user_id)))
    }

    /// Fetch (or lazily create) the per-user stronghold encryption key from the OS keyring.
    /// The keyring account name is the blake3 hash of `user_id`, giving each user
    /// their own isolated keyring entry.
    fn key_provider(&self, user_id: &str) -> Result<KeyProvider> {
        self.keyring.key_provider(&Self::user_id_hash(user_id))
    }

    /// Open the stronghold store for `user_id`.
    ///
    /// When `create_if_missing` is `true` the client and snapshot are created on first use;
    /// otherwise `None` is returned if either does not yet exist.
    ///
    /// # Arguments
    /// * `user_id` - The user ID whose stronghold store should be opened.
    /// * `create_if_missing` - Whether to create the stronghold snapshot and client if
    ///    they do not already exist. If `false`, this function will return `Ok(None)` if client data not present
    ///
    /// ### Returns
    /// If `create_if_missing` is `false`, returns `Ok(None)` if the client data not exist yet.
    /// Otherwise, returns a tuple of the opened [`Stronghold`], its [`Store`], the [`KeyProvider`], and the [`SnapshotPath`]
    /// for the caller to commit changes later.
    /// Returns an error if the snapshot file exists but cannot be loaded, or if the client cannot be loaded/created.
    fn open_store(
        &self,
        user_id: &str,
        create_if_missing: bool,
    ) -> Result<Option<(Stronghold, iota_stronghold::Store, KeyProvider, SnapshotPath)>> {
        let key_provider = self.key_provider(user_id)?;
        let snapshot_path = self.snapshot_path(user_id);

        let opened = open_store(&key_provider, &snapshot_path, user_id, create_if_missing)?;
        Ok(opened.map(|(stronghold, store)| (stronghold, store, key_provider, snapshot_path)))
    }

    /// Commit changes to disk.
    ///
    /// # Arguments
    /// * `stronghold` - The stronghold instance to commit.
    /// * `key_provider` - The key provider for encrypting the snapshot.
    /// * `snapshot_path` - The path where the snapshot should be saved.
    ///
    /// ### Returns
    /// An error if the commit fails for any reason (e.g. snapshot cannot be written, etc.). Returns `Ok(())` on success.
    fn commit(
        &self,
        stronghold: &Stronghold,
        key_provider: &KeyProvider,
        snapshot_path: &SnapshotPath,
    ) -> Result<()> {
        commit_store(stronghold, key_provider, snapshot_path)
    }


    /// Persist a full [`Session`].
    ///
    /// The sqlite password is not touched here. It has to exist before the account's
    /// store can be opened at all, which is earlier than this, so
    /// [`Self::get_or_create_sqlite_pwd`] is what creates it.
    ///
    /// # Arguments
    /// * `session` - The session to persist, which must include a user_id and
    ///    access_token. The device_id and refresh_token are optional but will be persisted if provided.
    ///
    /// ### Returns
    /// An error if the session cannot be persisted for any reason (e.g. stronghold cannot be loaded or committed, etc.).
    /// Returns `Ok(())` on success.
    pub fn set_session(&self, session: &Session) -> Result<()> {
        let (stronghold, store, key_provider, snapshot_path) =
            self
                .open_store(&session.user_id, true)?
                .ok_or_else(|| anyhow::anyhow!("Failed to open user stronghold store"))?;

        store.insert(b"user_id".to_vec(), session.user_id.as_bytes().to_vec(), None)?;
        store.insert(b"device_id".to_vec(), session.device_id.as_bytes().to_vec(), None)?;
        store.insert(b"access_token".to_vec(), session.access_token.as_bytes().to_vec(), None)?;

        if let Some(t) = &session.refresh_token {
            store.insert(b"refresh_token".to_vec(), t.as_bytes().to_vec(), None)?;
        } else {
            let _ = store.delete(b"refresh_token");
        }

        self.commit(&stronghold, &key_provider, &snapshot_path)
    }

    /// Retrieve the stored [`Session`] for `user_id`, or `None` if not found.
    ///
    /// # Arguments
    /// * `user_id` - The user ID whose session should be returned.
    ///
    /// ### Returns
    /// the stored [`Session`] for `user_id`, or `None` if not found
    pub fn get_session(&self, user_id: &str) -> Result<Option<Session>> {
        let Some((_, store, _, _)) = self.open_store(user_id, false)? else {
            return Ok(None);
        };

        let Some(access_bytes) = store.get(b"access_token")? else {
            return Ok(None);
        };

        Ok(Some(Session {
            user_id: user_id.to_string(),
            device_id: store
                .get(b"device_id")?
                .map(|b| String::from_utf8(b))
                .transpose()?
                .unwrap_or_default(),
            access_token: String::from_utf8(access_bytes)?,
            refresh_token: store
                .get(b"refresh_token")?
                .map(|b| String::from_utf8(b))
                .transpose()?,
        }))
    }

    /// Return the sqlite password for `user_id`, generating and persisting one if it
    /// doesn't exist yet.
    ///
    /// This encrypts the sqlite store holding the account's E2EE keys and message
    /// history, so there is deliberately no way to ask for "the password if there is
    /// one": a caller that got `None` back could only carry on by opening the store
    /// unencrypted, which is exactly the outcome this must not allow. The password is
    /// created on first use and never overwritten, since rotating it would orphan the
    /// existing database.
    ///
    /// # Arguments
    /// * `user_id` - The user ID whose sqlite password should be returned or created
    pub fn get_or_create_sqlite_pwd(&self, user_id: &str) -> Result<Zeroizing<String>> {
        let (stronghold, store, key_provider, snapshot_path) =
            self
                .open_store(user_id, true)?
                .ok_or_else(|| anyhow::anyhow!("Failed to open user stronghold store"))?;

        if let Some(bytes) = store.get(b"sqlite_password")? {
            return Ok(Zeroizing::new(String::from_utf8(bytes)?));
        }

        let pwd = Self::random_secret();
        store.insert(b"sqlite_password".to_vec(), pwd.as_bytes().to_vec(), None)?;
        self.commit(&stronghold, &key_provider, &snapshot_path)?;
        Ok(pwd)
    }
}
