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
    pub async fn oauth_login(&self, homeserver: String) -> anyhow::Result<Option<ClientHandler>> {
        let new_client = self.get_oauth_client(&homeserver).await?;
        let oauth = new_client.oauth();

        let metadata = oauth.server_metadata().await?;
        let issuer = metadata.issuer.as_str();
        let (redirect_uri, redirect_handle) = LocalServerBuilder::new().spawn().await?;

        if let Some(client_id) = self.app_state.echelon_store.oauth_client_id(issuer)? {
            oauth.restore_registered_client(matrix_sdk::authentication::oauth::ClientId::new(
                client_id,
            ));
        } else {
            let mut registered_redirect_uri = redirect_uri.clone();
            registered_redirect_uri
                .set_port(None)
                .map_err(|_| anyhow::anyhow!("Invalid OAuth redirect URI"))?;

            let url = Url::parse("https://git.flaxeneel2.net/streigen/echelon/")?;
            let grant_types = vec![OAuthGrantType::AuthorizationCode {
                redirect_uris: vec![registered_redirect_uri],
            }];
            let client_metadata = ClientMetadata::new(
                ApplicationType::Native,
                grant_types,
                Localized::new(url, Vec::new()),
            );

            let response = oauth.register_client(&Raw::new(&client_metadata)?).await?;
            self.app_state
                .echelon_store
                .set_oauth_client_id(issuer, response.client_id.as_str())?;
        }

        let auth_data = oauth
            .login(redirect_uri.clone(), None, None, None)
            .build()
            .await?;
        webbrowser::open(auth_data.url.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to open URL in browser: {}", e))?;

        let query = redirect_handle
            .await
            .ok_or_else(|| anyhow::anyhow!("OAuth redirect was cancelled or timed out"))?;

        oauth
            .finish_login(UrlOrQuery::Query(query.to_string()))
            .await?;

        let auth_session = new_client
            .session()
            .ok_or_else(|| anyhow::anyhow!("Missing OAuth session after login"))?;
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

        self.configure_session_persistence(&persistent_client, &user_id)?;
        persistent_client.restore_session(auth_session).await?;
        super::ensure_oauth_device_display_name(&persistent_client, false).await;

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
