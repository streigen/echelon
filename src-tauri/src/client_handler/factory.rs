use anyhow::Result;
use matrix_sdk::Client;
use std::path::Path;
use tauri::{Manager, Url};

use crate::secret::SecretService;

use super::ClientHandler;

impl ClientHandler {
    pub(super) async fn get_new_client(
        &self,
        username: &String,
        new_homeserver: &String,
        sqlite_pwd: Option<String>,
    ) -> Result<Client> {
        Ok(Client::builder()
            .homeserver_url(new_homeserver)
            .sqlite_store(
                Path::join(
                    &self.app_handle.path().app_data_dir()?.join("accounts"),
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
            .await?)
    }

    /// Log in a user with OAuth2 authentication using their homeserver
    ///
    /// # Arguments
    /// * `homeserver_url` - The URL of the homeserver to create a client for OAuth.
    pub(super) async fn get_oauth_client(&self, new_homeserver: &String) -> Result<Client> {
        let homeserver_url: Url = Url::parse(new_homeserver)?;
        let client = Client::new(homeserver_url).await?;
        Ok(client)
    }
}


