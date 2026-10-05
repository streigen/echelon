use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::utils::UrlOrQuery;
use matrix_sdk::utils::local_server::LocalServerBuilder;
use ruma::serde::Raw;
use url::Url;

use crate::client::session_of;
use crate::client::sync_manager::SyncManager;
use crate::events::client_events::ClientEvents;

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
        let new_client = self.get_oauth_client(&homeserver).await?;
        let oauth = new_client.oauth();

        oauth.server_metadata().await?;

        let (redirect_uri, redirect_handle) = LocalServerBuilder::new().spawn().await?;

        if login {
            // oauth.restore_registered_client()
        } else {
            let url = Url::parse("https://git.flaxeneel2.net/streigen/echelon/")?;
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

        let auth_data = oauth
            .login(redirect_uri.clone(), None, None, None)
            .build()
            .await?;
        open::that(auth_data.url.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to open URL in browser: {}", e))?;

        let query = redirect_handle
            .await
            .ok_or_else(|| anyhow::anyhow!("OAuth redirect was cancelled or timed out"))?;

        oauth
            .finish_login(UrlOrQuery::Query(query.to_string()))
            .await?;

        let auth_session = new_client
            .session()
            .ok_or_else(|| anyhow::anyhow!("Missing OAuth session afte login"))?;
        let session = session_of(&new_client)?;
        let user_id = session.user_id.clone();

        self.app_state.secret_service.set_session(&session)?;
        self.app_state
            .echelon_store
            .add_account(&user_id, &homeserver)?;

        drop(oauth);
        drop(new_client);

        let sqlite_pwd = self
            .app_state
            .secret_service
            .get_or_create_sqlite_pwd(&user_id)?;

        let persistent_client = self
            .get_new_client(&user_id, &homeserver, &sqlite_pwd)
            .await?;

        persistent_client.restore_session(auth_session).await?;

        ClientEvents::register_events(&persistent_client, self.ui_handle.clone());

        Ok(Some(ClientHandler {
            matrix_client: persistent_client,
            sync_manager: SyncManager::new(),
            active_room: Default::default(),
            app_state: self.app_state.clone(),
            ui_handle: self.ui_handle.clone(),
        }))
    }
}
