use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::authentication::oauth::UrlOrQuery;
use matrix_sdk::utils::local_server::LocalServerBuilder;
use ruma::serde::Raw;
use tauri::{Manager, Url};
use tauri_plugin_opener::OpenerExt;

use crate::events::client_events::ClientEvents;
use crate::secret::Session;
use crate::sync_manager::SyncManager;
use crate::{SecretState, StoreState};

use super::ClientHandler;

impl ClientHandler {
    /// Log in a user with OAuth2 authentication using their homeserver
    ///
    /// # Arguments
    /// * `homeserver` - The URL of the homeserver to log in to.
    /// * `login` - If true, the user has registered already so log them in, otherwise register
    pub async fn oauth_login(
        &self,
        homeserver: String,
        login: bool,
    ) -> anyhow::Result<Option<ClientHandler>> {
        // Create and generate an OAuth Handler
        let new_client = self.get_oauth_client(&homeserver).await?;
        let oauth = new_client.oauth();

        // Fetch metadata from homeserver to ensure that it supports OAuth
        // If it fails, it throws an exception to user.rs::oauth_login which passes it to front-end
        oauth.server_metadata().await?;

        // Make a listener to listen on a random port to receive the GET request
        let (redirect_uri, redirect_handle) = LocalServerBuilder::new().spawn().await?;

        // If user has registered and is logging in
        if login {
            // oauth.restore_registered_client()
        }
        // If the user hasn't registered, we register them
        else {
            // Setup client metadata
            let url = Url::parse("https://github.com/flaxeneel2/echelon/")?;
            let new_client_url = Localized::new(url, Vec::new());
            let grant_types: Vec<OAuthGrantType> = vec![
                OAuthGrantType::AuthorizationCode {
                    redirect_uris: vec![redirect_uri.clone()],
                },
                OAuthGrantType::DeviceCode,
            ];
            let client_metadata =
                ClientMetadata::new(ApplicationType::Native, grant_types, new_client_url);
            let raw_client_metadata = Raw::new(&client_metadata)?;
            oauth.register_client(&raw_client_metadata).await?;
        }
        // Build authorization data and login, then build the OAuthAuthCodeUrlBuilder
        let auth_data = oauth
            .login(redirect_uri.clone(), None, None, None)
            .build()
            .await?;
        self.app_handle
            .opener()
            .open_url(auth_data.url, None::<&str>)?;

        // Wait for redirect
        let query = redirect_handle
            .await
            .ok_or_else(|| anyhow::anyhow!("OAuth redirect was cancelled or timed out"))?;

        // Finish Login, the SDK verifies the csrf token internally
        oauth
            .finish_login(UrlOrQuery::Query(query.to_string()))
            .await?;

        // store the session tokens in stronghold
        let session_tokens = new_client
            .session_tokens()
            .ok_or_else(|| anyhow::anyhow!("Missing session tokens after OAuth login"))?;
        let user_id = new_client
            .user_id()
            .ok_or_else(|| anyhow::anyhow!("Missing user_id after OAuth login"))?
            .to_string();
        let secrets = self.app_handle.state::<SecretState>();
        secrets.0.set_session(&Session {
            user_id: user_id.clone(),
            device_id: new_client.device_id().map(|d| d.to_string()).unwrap_or_default(),
            access_token: session_tokens.access_token,
            refresh_token: session_tokens.refresh_token,
        })?;

        // store the new username
        let echelon_store = self.app_handle.state::<StoreState>();
        echelon_store.0.add_account(&user_id)?;

        ClientEvents::register_events(&new_client, self.app_handle.clone());

        Ok(Some(ClientHandler {
            matrix_client: new_client,
            sync_manager: SyncManager::new(),
            app_handle: self.app_handle.clone(),
        }))
    }
}


