use anyhow::Result;
use iota_stronghold::KeyProvider;
use keyring_core::{Entry, Error as KeyringError};
use tracing::error;
use zeroize::Zeroizing;
use crate::storage::secret::SecretService;

/// Abstracts OS-keyring access for both [crate::secret::SecretService] and
/// [crate::store::EchelonStore].
pub struct KeyringClient {
    /// Service/app identifier (e.g. app id).
    service: String,
}

impl KeyringClient {
    pub fn new(service: String) -> Self {
        KeyringClient { service }
    }

    /// Retrieve (or generate) the password stored in the keyring under `account`.
    ///
    /// # Arguments
    /// * `account` - The keyring account name.
    pub fn get_or_create_password(&self, account: &str) -> Result<Zeroizing<String>> {
        let entry = Entry::new(&self.service, account)?;
        match entry.get_password() {
            Ok(p) => Ok(Zeroizing::new(p)),
            Err(KeyringError::NoEntry) => {
                let p = SecretService::random_secret();
                entry.set_password(&p)?;
                Ok(p)
            }
            Err(e) => {
                error!("Failed to get password from keyring (account={account:?}): {e:?}");
                Err(anyhow::anyhow!("Failed to get password from keyring: {e}"))
            }
        }
    }

    /// Build an [iota_stronghold::KeyProvider] for `account`, creating the
    /// keyring entry if it does not yet exist.
    pub fn key_provider(&self, account: &str) -> Result<KeyProvider> {
        let password = self.get_or_create_password(account)?;
        Ok(KeyProvider::with_passphrase_hashed_blake2b(
            password.to_string(),
        )?)
    }
}
