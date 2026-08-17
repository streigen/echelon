use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::{AuthSession, SessionMeta, SessionTokens};
use ruma::{OwnedDeviceId, OwnedUserId};

use crate::client::session_of;
use crate::client::sync_manager::SyncManager;
use crate::events::client_events::ClientEvents;

use super::ClientHandler;

impl ClientHandler {
    /// Log in a user with the given username, password, and homeserver.
    ///
    /// The request runs on a store-less client, because an account's store is named
    /// after its real user id and that id is only known once the homeserver has
    /// answered. Deriving it from the username and the URL host instead gets a
    /// different answer whenever the server's name differs from the URL it is served
    /// on, which is the usual arrangement, and the account would then be filed under
    /// a name nothing else looks it up by.
    ///
    /// The established session is persisted and then adopted by a real, store-backed
    /// client through [`ClientHandler::restore_session`], which is the one place such
    /// a client is built.
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
        let auth_client = self.get_auth_client(&homeserver).await?;
        auth_client
            .matrix_auth()
            .login_username(&username, &password)
            .initial_device_display_name("Echelon")
            .send()
            .await?;

        let session = session_of(&auth_client)?;
        drop(auth_client);

        let user_id = session.user_id.clone();
        self.app_state.secret_service.set_session(&session)?;
        self.app_state
            .echelon_store
            .add_account(&user_id, &homeserver)?;

        self.restore_session(user_id, Some(homeserver)).await
    }

    /// Build the client that owns a stored account and restore its session into it.
    ///
    /// This is the only path that opens an account's encrypted store, so every way in
    /// (a fresh login, a registration, or an app restart) ends up here.
    ///
    /// # Arguments
    /// * `user_id` - The full Matrix user id of the account to restore, as stored by
    ///   [`crate::storage::store::EchelonStore`].
    /// * `homeserver` - Optional homeserver URL override. If `None`, the URL recorded in
    ///   the account list is used, falling back to `.well-known` discovery if none is stored.
    pub async fn restore_session(
        &self,
        user_id: String,
        homeserver: Option<String>,
    ) -> anyhow::Result<Option<ClientHandler>> {
        let homeserver_url = match homeserver {
            Some(hs) => {
                let _ = self.app_state.echelon_store.set_homeserver(&user_id, &hs);
                hs
            }
            None => {
                let account = self.app_state.echelon_store.get_account(&user_id)?;
                if let Some(hs) = account.and_then(|a| a.homeserver) {
                    hs
                } else {
                    let discovered = self.discover_homeserver(&user_id).await?;
                    self.app_state
                        .echelon_store
                        .set_homeserver(&user_id, &discovered)?;
                    discovered
                }
            }
        };

        let sqlite_pwd = self
            .app_state
            .secret_service
            .get_or_create_sqlite_pwd(&user_id)?;

        let new_client = self
            .get_new_client(&user_id, &homeserver_url, &sqlite_pwd)
            .await?;
        let mut session = self
            .app_state
            .secret_service
            .get_session(&user_id)?
            .ok_or_else(|| anyhow::anyhow!("No stored session found for user"))?;

        // Taken rather than moved out: the session's zeroizing drop makes it a `Drop`
        // type, and a `Drop` type's fields cannot be moved out of. Taking hands the
        // SDK the same buffer the token already lives in, so it is never copied; what
        // stays behind is an empty string, which the drop then wipes for free.
        let tokens = SessionTokens {
            access_token: std::mem::take(&mut session.access_token),
            refresh_token: session.refresh_token.take(),
        };

        new_client
            .restore_session(AuthSession::Matrix(MatrixSession {
                meta: SessionMeta {
                    user_id: OwnedUserId::try_from(session.user_id.as_str())?,
                    // Infallible, unlike the user id above: a device id has no grammar
                    // to violate, so any string is one.
                    device_id: OwnedDeviceId::from(session.device_id.as_str()),
                },
                tokens,
            }))
            .await?;

        ClientEvents::register_events(&new_client, self.ui_handle.clone());

        Ok(Some(ClientHandler {
            matrix_client: new_client,
            sync_manager: SyncManager::new(),
            app_state: self.app_state.clone(),
            ui_handle: self.ui_handle.clone(),
        }))
    }
}
