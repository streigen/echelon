use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::utils::UrlOrQuery;
use matrix_sdk::utils::local_server::LocalServerBuilder;
use matrix_sdk::utils::local_server::LocalServerRedirectHandle;
use ruma::serde::Raw;
use std::future::IntoFuture;
use std::time::Duration;
use url::Url;

use crate::client::session_of;
use crate::client::sync_manager::SyncManager;
use crate::events::client_events::ClientEvents;

use super::ClientHandler;

const OAUTH_REDIRECT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

impl ClientHandler {
    /// Log in a user with OAuth2 authentication using their homeserver
    ///
    /// # Arguments
    /// * `homeserver` - The URL of the homeserver to log in to.
    /// * `cancel` - Optional signal that cancels while waiting for the browser redirect.
    pub async fn oauth_login(
        &self,
        homeserver: String,
        cancel: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> anyhow::Result<Option<ClientHandler>> {
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

            let url = Url::parse("https://echelon.sh")?;
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

        let auth_data = oauth.login(redirect_uri, None, None, None).build().await?;
        webbrowser::open(auth_data.url.as_str())
            .map_err(|e| anyhow::anyhow!("Failed to open URL in browser: {}", e))?;

        let _ = self
            .ui_handle
            .upgrade_in_event_loop(|ui| ui.set_oauth_waiting(true));
        let redirect =
            wait_for_oauth_redirect(redirect_handle, OAUTH_REDIRECT_TIMEOUT, cancel).await;
        let _ = self
            .ui_handle
            .upgrade_in_event_loop(|ui| ui.set_oauth_waiting(false));
        let Some(query) = redirect? else {
            return Ok(None);
        };

        oauth.finish_login(UrlOrQuery::Query(query)).await?;

        let auth_session = new_client
            .session()
            .ok_or_else(|| anyhow::anyhow!("Missing OAuth session after login"))?;
        let session = session_of(&new_client)?;
        let user_id = &session.user_id;

        self.app_state.secret_service.set_session(&session)?;
        self.app_state
            .echelon_store
            .add_account(user_id, &homeserver)?;

        drop(oauth);
        drop(new_client);

        let sqlite_pwd = self
            .app_state
            .secret_service
            .get_or_create_sqlite_pwd(user_id)?;

        let persistent_client = self
            .get_new_client(user_id, &homeserver, &sqlite_pwd)
            .await?;

        self.configure_session_persistence(&persistent_client, user_id)?;
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

async fn wait_for_oauth_redirect(
    redirect_handle: LocalServerRedirectHandle,
    timeout: Duration,
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
) -> anyhow::Result<Option<String>> {
    let redirect = tokio::time::timeout(timeout, redirect_handle.into_future());
    tokio::pin!(redirect);

    let cancelled = async move {
        if let Some(cancel) = cancel {
            let _ = cancel.await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(cancelled);

    tokio::select! {
        biased;
        result = &mut redirect => {
            match result {
                Err(_) => Err(anyhow::anyhow!("OAuth login timed out. Please try again.")),
                Ok(None) => Err(anyhow::anyhow!("OAuth redirect was cancelled")),
                Ok(Some(query)) => Ok(Some(query.to_string())),
            }
        }
        _ = &mut cancelled => {
            Ok(None)
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/client/oauth.rs"]
mod tests;
