use anyhow::Result;
use blake3;
use iota_stronghold::{KeyProvider, SnapshotPath, Stronghold};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::storage::keyring_client::KeyringClient;
use crate::storage::stronghold_backend::{commit_store, open_store};

/// Represents a Matrix account persisted on-device, including user ID and optional homeserver URL.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(from = "AccountRepr")]
pub struct Account {
    pub user_id: String,
    pub homeserver: Option<String>,
}

/// Wire form of [`Account`], accepting both the current object and the bare user id
/// string that older snapshots hold, so an existing account list still loads.
#[derive(Deserialize)]
#[serde(untagged)]
enum AccountRepr {
    Legacy(String),
    Current {
        user_id: String,
        homeserver: Option<String>,
    },
}

impl From<AccountRepr> for Account {
    fn from(repr: AccountRepr) -> Self {
        match repr {
            AccountRepr::Legacy(user_id) => Account {
                user_id,
                homeserver: None,
            },
            AccountRepr::Current {
                user_id,
                homeserver,
            } => Account {
                user_id,
                homeserver,
            },
        }
    }
}

/// The list of known accounts persisted on-device.
#[derive(Serialize, Deserialize, Default)]
pub struct Accounts {
    pub(crate) last: Option<String>,
    pub(crate) accounts: Vec<Account>,
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
    /// Create a new [`EchelonStore`].
    ///
    /// # Arguments
    /// * `keyring` - The keyring client.
    /// * `keyring_account` - The account name under which the encryption key is stored.
    /// * `store_dir` - The directory where the snapshot file will be stored.
    pub fn new(keyring: KeyringClient, keyring_account: String, store_dir: PathBuf) -> Self {
        let name = blake3::hash(keyring_account.as_bytes()).to_string();
        let snapshot_path = SnapshotPath::from_path(store_dir.join(name));
        EchelonStore {
            keyring,
            keyring_account,
            snapshot_path,
        }
    }

    /// Fetch (or lazily create) the stronghold encryption key from the OS keyring.
    fn key_provider(&self) -> Result<KeyProvider> {
        self.keyring.key_provider(&self.keyring_account)
    }

    /// Open the stronghold store.
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

    /// Read the list of accounts from the store, returning default if missing.
    fn read_accounts(&self, store: &iota_stronghold::Store) -> Result<Accounts> {
        match store.get(b"accounts")? {
            Some(bytes) => Ok(serde_json::from_slice(&bytes)?),
            None => Ok(Accounts::default()),
        }
    }

    /// Write the list of accounts to the store.
    fn write_accounts(&self, store: &iota_stronghold::Store, accounts: &Accounts) -> Result<()> {
        let bytes = serde_json::to_vec(accounts)?;
        store.insert(b"accounts".to_vec(), bytes, None)?;
        Ok(())
    }

    /// Get the list of persisted accounts, along with the most-recently-used account (if any).
    pub fn get_accounts(&self) -> Result<Accounts> {
        let (_, store, _) = self.open()?;
        self.read_accounts(&store)
    }

    /// Look up a single persisted account by user id.
    ///
    /// # Arguments
    /// * `user_id` - The full Matrix user id to look up.
    pub fn get_account(&self, user_id: &str) -> Result<Option<Account>> {
        Ok(self
            .get_accounts()?
            .accounts
            .into_iter()
            .find(|a| a.user_id == user_id))
    }

    /// Add `user_id` to the persisted account list (or move it to front) and
    /// mark it as the most-recently-used account.
    ///
    /// # Arguments
    /// * `user_id` - The user ID to add or move to the front of the accounts list.
    /// * `homeserver` - The homeserver URL the account was reached through.
    pub fn add_account(&self, user_id: &str, homeserver: &str) -> Result<()> {
        let (stronghold, store, key_provider) = self.open()?;
        let mut accounts = self.read_accounts(&store)?;

        accounts.accounts.retain(|x| x.user_id != user_id);
        accounts.accounts.insert(
            0,
            Account {
                user_id: user_id.to_string(),
                homeserver: Some(homeserver.to_string()),
            },
        );
        accounts.last = Some(user_id.to_string());

        self.write_accounts(&store, &accounts)?;
        self.commit(&stronghold, &key_provider)
    }

    /// Record the homeserver URL for an account.
    ///
    /// # Arguments
    /// * `user_id` - The full Matrix user ID to update.
    /// * `homeserver` - The homeserver URL to record.
    pub fn set_homeserver(&self, user_id: &str, homeserver: &str) -> Result<()> {
        let (stronghold, store, key_provider) = self.open()?;
        let mut accounts = self.read_accounts(&store)?;

        let Some(account) = accounts.accounts.iter_mut().find(|a| a.user_id == user_id) else {
            return Ok(());
        };
        if account.homeserver.as_deref() == Some(homeserver) {
            return Ok(());
        }
        account.homeserver = Some(homeserver.to_string());

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
        accounts.accounts.retain(|x| x.user_id != user_id);

        if accounts.accounts.len() == before {
            // Nothing changed, user_id was not in the list.
            return Ok(());
        }

        if accounts.last.as_deref() == Some(user_id) {
            accounts.last = accounts.accounts.first().map(|a| a.user_id.clone());
        }

        self.write_accounts(&store, &accounts)?;
        self.commit(&stronghold, &key_provider)
    }

    /// Return the most-recently-used account, if any.
    pub fn get_last(&self) -> Result<Option<String>> {
        Ok(self.get_accounts()?.last)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/storage/store.rs"]
mod tests;
