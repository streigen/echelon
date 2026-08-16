use anyhow::Result;
use matrix_sdk::Client;
use url::Url;

use crate::storage::secret::SecretService;

use super::ClientHandler;

impl ClientHandler {
    /// Build the client that owns an account's on-disk state.
    ///
    /// The store is named after the account's real user id rather than anything the
    /// user typed, so an account is found again whatever URL it was reached through.
    /// It is always encrypted: the password is created on first use and reread after
    /// that, and there is no path that opens this store without one.
    ///
    /// # Arguments
    /// * `user_id` - The account's full Matrix user id, as the homeserver reported it.
    /// * `new_homeserver` - The homeserver URL to point the client at.
    /// * `sqlite_pwd` - The store's encryption password, from [`SecretService`].
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

        // Enable the local event cache so already-synced/persisted room timelines
        // can be served without a `/messages` network round trip (see
        // commands::messages::get_messages_from_room_paginated). Must happen
        // before sync starts so live events get fed into the cache as they arrive.
        client.event_cache().subscribe()?;

        Ok(client)
    }

    /// Build a throwaway, store-less client for running one authentication request.
    ///
    /// An account's store is named after its user id, and the user id is only known
    /// once the homeserver has answered, so the request that establishes it cannot be
    /// made through the client that will own the store. Nothing is persisted here and
    /// no sync is started; the session this produces is adopted by a real client via
    /// [`ClientHandler::adopt_session`].
    ///
    /// # Arguments
    /// * `homeserver` - The homeserver URL to authenticate against.
    pub(super) async fn get_auth_client(&self, homeserver: &str) -> Result<Client> {
        Ok(Client::new(Url::parse(homeserver)?).await?)
    }

    /// Log in a user with OAuth2 authentication using their homeserver
    ///
    /// # Arguments
    /// * `homeserver_url` - The URL of the homeserver to create a client for OAuth.
    pub(super) async fn get_oauth_client(&self, new_homeserver: &String) -> Result<Client> {
        let homeserver_url: Url = Url::parse(new_homeserver)?;
        let client = Client::new(homeserver_url).await?;
        client.event_cache().subscribe()?;
        Ok(client)
    }
}
