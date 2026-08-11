use anyhow::Result;
use matrix_sdk::Client;
use std::path::Path;
use url::Url;

use crate::storage::secret::SecretService;

use super::ClientHandler;

impl ClientHandler {
    pub(super) async fn get_new_client(
        &self,
        username: &String,
        new_homeserver: &String,
        sqlite_pwd: Option<String>,
    ) -> Result<Client> {
        let client = Client::builder()
            .homeserver_url(new_homeserver)
            .sqlite_store(
                Path::join(
                    &self.app_state.data_dir.join("accounts"),
                    SecretService::user_id_hash(&format!(
                        "@{}:{}",
                        username,
                        Url::parse(new_homeserver)?
                            .domain()
                            .ok_or_else(|| anyhow::anyhow!("Invalid homeserver domain"))?
                    )),
                ),
                sqlite_pwd.as_deref(),
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
