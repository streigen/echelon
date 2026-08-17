use anyhow::Result;
use matrix_sdk::Client;
use url::Url;

use ruma::OwnedUserId;

use crate::storage::secret::SecretService;

use super::ClientHandler;

impl ClientHandler {
    /// Build the persistent client that owns an account's on-disk state.
    ///
    /// # Arguments
    /// * `user_id` - The account's full Matrix user id.
    /// * `new_homeserver` - The homeserver URL.
    /// * `sqlite_pwd` - The store's encryption password.
    pub(super) async fn get_new_client(
        &self,
        user_id: &str,
        new_homeserver: &str,
        sqlite_pwd: &str,
    ) -> Result<Client> {
        let client = Client::builder()
            .homeserver_url(new_homeserver)
            .sqlite_store(
                self.app_state
                    .data_dir
                    .join("accounts")
                    .join(SecretService::user_id_hash(user_id)),
                Some(sqlite_pwd),
            )
            .build()
            .await?;

        // Subscribe to event cache before sync starts.
        client.event_cache().subscribe()?;

        Ok(client)
    }

    /// Build an unauthenticated, store-less client for initial auth requests.
    ///
    /// # Arguments
    /// * `homeserver` - The homeserver URL to authenticate against.
    pub(super) async fn get_auth_client(&self, homeserver: &str) -> Result<Client> {
        Ok(Client::new(Url::parse(homeserver)?).await?)
    }

    /// Discover the homeserver URL for a given user ID via `.well-known` lookup.
    ///
    /// # Arguments
    /// * `user_id` - The account's full Matrix user ID.
    pub(super) async fn discover_homeserver(&self, user_id: &str) -> Result<String> {
        let user_id = OwnedUserId::try_from(user_id)?;
        let server_name = user_id.server_name();
        match Client::builder().server_name(server_name).build().await {
            Ok(client) => Ok(client.homeserver().to_string()),
            Err(e) => {
                tracing::warn!(
                    "Well-known discovery failed for server {server_name}: {e}, falling back to https://{server_name}"
                );
                Ok(format!("https://{server_name}"))
            }
        }
    }

    /// Log in a user with OAuth2 authentication using their homeserver
    ///
    /// # Arguments
    /// * `homeserver_url` - The URL of the homeserver to create a client for OAuth.
    pub(super) async fn get_oauth_client(&self, new_homeserver: &str) -> Result<Client> {
        let homeserver_url: Url = Url::parse(new_homeserver)?;
        let client = Client::new(homeserver_url).await?;
        client.event_cache().subscribe()?;
        Ok(client)
    }
}
