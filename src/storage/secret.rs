use crate::storage::keyring_client::KeyringClient;
use crate::storage::stronghold_backend::{commit_store, open_store};
use anyhow::Result;
use blake3;
use iota_stronghold::{KeyProvider, SnapshotPath, Stronghold};
use rand::distr::{Alphanumeric, SampleString};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use zeroize::{ZeroizeOnDrop, Zeroizing};

/// All per-user session data stored in the stronghold.
///
/// Tokens are zeroized on drop.
#[derive(ZeroizeOnDrop)]
pub struct Session {
    #[zeroize(skip)]
    pub user_id: String,
    #[zeroize(skip)]
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub oauth_client_id: Option<String>,
}

pub struct SecretService {
    /// Shared keyring client for fetching/creating the stronghold encryption key.
    keyring: KeyringClient,
    /// Directory where per-user stronghold snapshot files are kept.
    stronghold_path: PathBuf,
    /// Ensures snapshot operations run one at a time, so token writes cannot
    /// overlap with session replacement or deletion
    session_lock: Mutex<()>,
}

impl SecretService {
    pub fn new(keyring: KeyringClient, stronghold_path: PathBuf) -> Self {
        SecretService {
            keyring,
            stronghold_path,
            session_lock: Mutex::new(()),
        }
    }

    fn lock_sessions(&self) -> Result<MutexGuard<'_, ()>> {
        self.session_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("Session storage lock was poisoned"))
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
    /// # Arguments
    /// * `user_id` - The user ID whose store to open.
    /// * `create_if_missing` - Whether to create the snapshot and client if missing.
    fn open_store(
        &self,
        user_id: &str,
        create_if_missing: bool,
    ) -> Result<
        Option<(
            Stronghold,
            iota_stronghold::Store,
            KeyProvider,
            SnapshotPath,
        )>,
    > {
        let key_provider = self.key_provider(user_id)?;
        let snapshot_path = self.snapshot_path(user_id);

        let opened = open_store(&key_provider, &snapshot_path, user_id, create_if_missing)?;
        Ok(opened.map(|(stronghold, store)| (stronghold, store, key_provider, snapshot_path)))
    }

    /// Commit changes to disk.
    fn commit(
        &self,
        stronghold: &Stronghold,
        key_provider: &KeyProvider,
        snapshot_path: &SnapshotPath,
    ) -> Result<()> {
        commit_store(stronghold, key_provider, snapshot_path)
    }

    /// Persist a full [`Session`].
    pub fn set_session(&self, session: &Session) -> Result<()> {
        let _guard = self.lock_sessions()?;
        let (stronghold, store, key_provider, snapshot_path) = self
            .open_store(&session.user_id, true)?
            .ok_or_else(|| anyhow::anyhow!("Failed to open user stronghold store"))?;

        store.insert(
            b"user_id".to_vec(),
            session.user_id.as_bytes().to_vec(),
            None,
        )?;
        store.insert(
            b"device_id".to_vec(),
            session.device_id.as_bytes().to_vec(),
            None,
        )?;
        store.insert(
            b"access_token".to_vec(),
            session.access_token.as_bytes().to_vec(),
            None,
        )?;

        if let Some(t) = &session.refresh_token {
            store.insert(b"refresh_token".to_vec(), t.as_bytes().to_vec(), None)?;
        } else {
            let _ = store.delete(b"refresh_token");
        }

        if let Some(t) = &session.oauth_client_id {
            store.insert(b"oauth_client_id".to_vec(), t.as_bytes().to_vec(), None)?;
        } else {
            let _ = store.delete(b"oauth_client_id");
        }

        self.commit(&stronghold, &key_provider, &snapshot_path)
    }

    /// Persist a rotated access and refresh token pair without replacing the
    /// account metadata stored alongside the session.
    pub fn set_session_tokens_for_device(
        &self,
        user_id: &str,
        device_id: &str,
        oauth_client_id: Option<&str>,
        access_token: &str,
        refresh_token: Option<&str>,
    ) -> Result<bool> {
        let _guard = self.lock_sessions()?;
        // Avoid asking the keyring to create a replacement key after logout
        // removed this account's snapshot.
        let snapshot = self.snapshot_path(user_id);
        if !snapshot.as_path().exists() {
            return Ok(false);
        }
        let (stronghold, store, key_provider, snapshot_path) = self
            .open_store(user_id, false)?
            .ok_or_else(|| anyhow::anyhow!("No stronghold store found for user"))?;

        let stored_device = store
            .get(b"device_id")?
            .map(String::from_utf8)
            .transpose()?
            .unwrap_or_default();
        let stored_oauth_client_id = store
            .get(b"oauth_client_id")?
            .map(String::from_utf8)
            .transpose()?;
        let stored_user_id = store.get(b"user_id")?.map(String::from_utf8).transpose()?;
        let has_access_token = store.get(b"access_token")?.is_some();
        if !has_access_token
            || stored_user_id.as_deref() != Some(user_id)
            || stored_device != device_id
            || stored_oauth_client_id.as_deref() != oauth_client_id
        {
            return Ok(false);
        }

        // Commit only after both values have been updated. If a write fails,
        // the on-disk snapshot still contains the last committed token pair.
        store.insert(
            b"access_token".to_vec(),
            access_token.as_bytes().to_vec(),
            None,
        )?;
        if let Some(token) = refresh_token {
            store.insert(b"refresh_token".to_vec(), token.as_bytes().to_vec(), None)?;
        } else {
            let _ = store.delete(b"refresh_token");
        }

        self.commit(&stronghold, &key_provider, &snapshot_path)?;
        Ok(true)
    }

    /// Retrieve the stored [`Session`] for `user_id`, or `None` if not found.
    pub fn get_session(&self, user_id: &str) -> Result<Option<Session>> {
        let _guard = self.lock_sessions()?;
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
                .map(String::from_utf8)
                .transpose()?
                .unwrap_or_default(),
            access_token: String::from_utf8(access_bytes)?,
            refresh_token: store
                .get(b"refresh_token")?
                .map(String::from_utf8)
                .transpose()?,
            oauth_client_id: store
                .get(b"oauth_client_id")?
                .map(String::from_utf8)
                .transpose()?,
        }))
    }

    /// Return the sqlite password for `user_id`, generating and persisting one on first use.
    pub fn get_or_create_sqlite_pwd(&self, user_id: &str) -> Result<Zeroizing<String>> {
        let _guard = self.lock_sessions()?;
        let (stronghold, store, key_provider, snapshot_path) = self
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

    /// Permanently remove all stored secrets for `user_id`: the stronghold
    /// snapshot file on disk and its OS-keyring encryption key.
    ///
    /// # Arguments
    /// * `user_id` - The full Matrix user id whose stored secrets to delete.
    pub fn delete_session(&self, user_id: &str) -> Result<()> {
        let _guard = self.lock_sessions()?;
        let path = self.snapshot_path(user_id).as_path().to_path_buf();
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to delete stronghold snapshot at {path:?}: {e}"
                ));
            }
        }

        self.keyring.delete_password(&Self::user_id_hash(user_id))
    }

    /// Delete session credentials while retaining the shared per-user keyring
    /// key. Logout uses this if the SQLite directory could not be deleted, since
    /// that key also protects the database password.
    pub fn delete_session_preserving_key(&self, user_id: &str) -> Result<()> {
        let _guard = self.lock_sessions()?;
        let Some((stronghold, store, key_provider, snapshot_path)) =
            self.open_store(user_id, false)?
        else {
            return Ok(());
        };

        for key in [
            b"user_id".as_slice(),
            b"device_id",
            b"access_token",
            b"refresh_token",
            b"oauth_client_id",
        ] {
            store.delete(key)?;
        }
        self.commit(&stronghold, &key_provider, &snapshot_path)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/storage/secret.rs"]
mod tests;
