use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::{AuthSession, SessionMeta, SessionTokens};
use ruma::{OwnedDeviceId, OwnedUserId};
use tauri::{Manager, Url};

use crate::events::client_events::ClientEvents;
use crate::secret::Session;
use crate::sync_manager::SyncManager;
use crate::{SecretState, StoreState};

use super::ClientHandler;

impl ClientHandler {
    /// Log in a user with the given username, password, and homeserver.
    ///
    /// # Arguments
    /// * `username` - The username of the account to log in to.
    /// * `password` - The password of the account to log in to.
    /// * `homeserver` - The URL of the homeserver to log in to.
    pub async fn login(
        &self,
        username: String,
        password: String,
        homeserver: String,
    ) -> anyhow::Result<Option<ClientHandler>> {
        // Derive the full Matrix user ID so we can look up / generate the sqlite password
        // before we even open the store, ensuring the DB is always encrypted from first open.
        let user_id = format!(
            "@{}:{}",
            username,
            Url::parse(&homeserver)?.domain().unwrap_or(&homeserver)
        );
        let secrets = self.app_handle.state::<SecretState>();
        let sqlite_pwd = secrets.0.get_or_create_sqlite_pwd(&user_id)?;

        let new_client = self
            .get_new_client(&username, &homeserver, Some(sqlite_pwd))
            .await?;
        new_client
            .matrix_auth()
            .login_username(&username, &password)
            .initial_device_display_name("Echelon")
            .send()
            .await?;

        ClientEvents::register_events(&new_client, self.app_handle.clone());

        // store the session tokens in stronghold
        let session_tokens = new_client
            .session_tokens()
            .ok_or_else(|| anyhow::anyhow!("Missing session tokens after login"))?;
        let user_id = new_client
            .user_id()
            .ok_or_else(|| anyhow::anyhow!("Missing user_id after login"))?
            .to_string();
        secrets.0.set_session(&Session {
            user_id: user_id.clone(),
            device_id: new_client.device_id().map(|d| d.to_string()).unwrap_or_default(),
            access_token: session_tokens.access_token,
            refresh_token: session_tokens.refresh_token,
        })?;

        // store the new username
        let echelon_store = self.app_handle.state::<StoreState>();
        echelon_store.0.add_account(&user_id)?;

        Ok(Some(ClientHandler {
            matrix_client: new_client,
            sync_manager: SyncManager::new(),
            app_handle: self.app_handle.clone(),
        }))
    }

    /// Restore a previous session for the given username and homeserver. This will attempt to load
    /// the session from the client's store, and if successful, will start the sync loop for that
    /// session. This is used for session persistence across app restarts.
    ///
    /// # Arguments
    /// * `username` - The username of the session to restore.
    /// * `homeserver` - The homeserver of the session to restore, used to disambiguate sessions.
    pub async fn restore_session(
        &self,
        username: String,
        homeserver: String,
    ) -> anyhow::Result<Option<ClientHandler>> {
        let user_id = format!(
            "@{}:{}",
            username,
            Url::parse(&homeserver)?.domain().unwrap_or(&homeserver)
        );
        let secrets = self.app_handle.state::<SecretState>();
        let sqlite_pwd = secrets.0.get_sqlite_pwd(&user_id)?;

        let new_client = self.get_new_client(&username, &homeserver, sqlite_pwd).await?;
        let session = secrets
            .0
            .get_session(&user_id)?
            .ok_or_else(|| anyhow::anyhow!("No stored session found for user"))?;

        new_client
            .restore_session(AuthSession::Matrix(MatrixSession {
                meta: SessionMeta {
                    user_id: OwnedUserId::try_from(session.user_id)?,
                    device_id: OwnedDeviceId::try_from(session.device_id)?,
                },
                tokens: SessionTokens {
                    access_token: session.access_token,
                    refresh_token: session.refresh_token,
                },
            }))
            .await?;

        ClientEvents::register_events(&new_client, self.app_handle.clone());

        Ok(Some(ClientHandler {
            matrix_client: new_client,
            sync_manager: SyncManager::new(),
            app_handle: self.app_handle.clone(),
        }))
    }
}


