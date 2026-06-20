use anyhow::Result;
use blake3;
use iota_stronghold::{KeyProvider, SnapshotPath, Stronghold};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::keyring_client::KeyringClient;
use crate::stronghold_backend::{commit_store, open_store};

/// The list of known accounts persisted on-device.
#[derive(Serialize, Deserialize, Default)]
pub struct Accounts {
    pub(crate) last: Option<String>,
    pub(crate) accounts: Vec<String>,
}

/// App-level persistent store backed by a Stronghold snapshot.
///
/// The encryption key for the snapshot is retrieved from (or lazily created
/// in) the OS keyring via [KeyringClient].
pub struct EchelonStore {
    keyring: KeyringClient,
    /// Keyring account name under which the stronghold key lives.
    keyring_account: String,
    /// Path to the stronghold snapshot file used for app-level data.
    snapshot_path: SnapshotPath,
}

/// Fixed client name inside the stronghold snapshot for app-level data.
/// This is separate from the per-user client used by [SecretService] to store
const APP_CLIENT: &str = "echelon-app";

impl EchelonStore {

    /// Create a new [EchelonStore] with the given keyring client, keyring account name, and snapshot directory.
    ///
    /// # Arguments
    /// * `keyring` - The [KeyringClient] used to access the OS keyring for the stronghold encryption key.
    /// * `keyring_account` - The account name under which the stronghold encryption key is stored
    ///    in the keyring. This will be hashed to create a stable, FS-safe filename for the snapshot.
    /// * `store_dir` - The directory where the stronghold snapshot file will be stored.
    ///    The actual filename will be derived from the `keyring_account` by hashing it with blake3 to
    ///    ensure it's stable and safe for the filesystem
    pub fn new(keyring: KeyringClient, keyring_account: String, store_dir: PathBuf) -> Self {
        // Hash the keyring_account string to get a stable, FS-safe filename.
        let name = blake3::hash(keyring_account.as_bytes()).to_string();
        let snapshot_path = SnapshotPath::from_path(store_dir.join(format!("{name}")));
        EchelonStore { keyring, keyring_account, snapshot_path }
    }

    /// Fetch (or lazily create) the stronghold encryption key from the OS keyring.
    fn key_provider(&self) -> Result<KeyProvider> {
        self.keyring.key_provider(&self.keyring_account)
    }

    /// Open (or lazily create) the stronghold and return its Store.
    ///
    /// ### Returns
    ///
    /// If the snapshot file is missing, it will be created on commit, so this does not return an error in that case.
    /// Returns an error if the snapshot file exists but cannot be loaded, or if the client cannot be loaded/created.
    fn open(&self) -> Result<(Stronghold, iota_stronghold::Store, KeyProvider)> {
        let key_provider = self.key_provider()?;
        let (stronghold, store) = open_store(&key_provider, &self.snapshot_path, APP_CLIENT, true)?
            .ok_or_else(|| anyhow::anyhow!("Failed to open app stronghold store"))?;
        Ok((stronghold, store, key_provider))
    }

    /// Commit changes to the stronghold snapshot.
    fn commit(&self, stronghold: &Stronghold, key_provider: &KeyProvider) -> Result<()> {
        commit_store(stronghold, key_provider, &self.snapshot_path)
    }

    /// Read the list of accounts from the store, returning an empty list if not present.
    ///
    /// # Arguments
    /// * `store` - The stronghold store from which to read the accounts list.
    ///
    /// ### Retruns
    /// Returns an error if the data is present but cannot be deserialized, or if there is an issue reading from the store.
    /// Returns `Ok(Accounts::default())` if the "accounts" key is not present in the store, which is expected on first run.
    ///
    fn read_accounts(&self, store: &iota_stronghold::Store) -> Result<Accounts> {
        match store.get(b"accounts")? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
            None => Ok(Accounts::default()),
        }
    }

    /// Write the list of accounts to the store.
    ///
    /// # Arguments
    /// * `store` - The stronghold store to which to write the accounts list.
    /// * `accounts` - The list of accounts to persist.
    ///
    /// ### Returns
    /// An error if the accounts cannot be serialized or if there is an issue writing to the store.
    fn write_accounts(
        &self,
        store: &iota_stronghold::Store,
        accounts: &Accounts,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(accounts)?;
        store.insert(b"accounts".to_vec(), bytes, None)?;
        Ok(())
    }

    /// Get the list of persisted accounts, along with the most-recently-used account (if any).
    pub fn get_accounts(&self) -> Result<Accounts> {
        let (_, store, _) = self.open()?;
        self.read_accounts(&store)
    }

    /// Add `user_id` to the persisted account list (or move it to front) and
    /// mark it as the most-recently-used account.
    ///
    /// # Arguments
    /// * `user_id` - The user ID to add or move to the front of the accounts list.
    pub fn add_account(&self, user_id: &str) -> Result<()> {
        let (stronghold, store, key_provider) = self.open()?;
        let mut accounts = self.read_accounts(&store)?;

        accounts.accounts.retain(|x| x != user_id);
        accounts.accounts.insert(0, user_id.to_string());
        accounts.last = Some(user_id.to_string());

        self.write_accounts(&store, &accounts)?;
        self.commit(&stronghold, &key_provider)
    }

    /// Remove `user_id` from the persisted account list.
    ///
    /// If it was also the last-used account, promotes the next account in the
    /// list (or sets `last` to `None` if the list is now empty).
    ///
    /// # Arguments
    /// * `user_id` - The user ID to remove from the accounts list.
    pub fn remove_account(&self, user_id: &str) -> Result<()> {
        let (stronghold, store, key_provider) = self.open()?;
        let mut accounts = self.read_accounts(&store)?;

        let before = accounts.accounts.len();
        accounts.accounts.retain(|x| x != user_id);

        if accounts.accounts.len() == before {
            // Nothing changed, user_id was not in the list.
            return Ok(());
        }

        if accounts.last.as_deref() == Some(user_id) {
            accounts.last = accounts.accounts.first().cloned();
        }

        self.write_accounts(&store, &accounts)?;
        self.commit(&stronghold, &key_provider)
    }

    /// Return the most-recently-used account, if any.
    pub fn get_last(&self) -> Result<Option<String>> {
        Ok(self.get_accounts()?.last)
    }
}