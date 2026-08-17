use matrix_sdk::ruma::api::client::account::register::v3::Request as RegistrationRequest;
use ruma::api::client::uiaa::{AuthData, RegistrationToken};
use tracing::debug;

use crate::client::session_of;

use super::ClientHandler;

impl ClientHandler {
    /// Register a new account and sign into it.
    ///
    /// Like [`ClientHandler::login`], the request runs on a store-less client: the
    /// homeserver picks the account's real user id, which can differ from the
    /// requested username, and that id is what names the store. Once the account
    /// exists the session is persisted and handed to a real client through
    /// [`ClientHandler::restore_session`], so a registered account gets the same
    /// encrypted store and the same persistence as one that logged in. It used to get
    /// neither.
    ///
    /// # Arguments
    /// * `username` - The desired username for the new account.
    /// * `password` - The desired password for the new account.
    /// * `homeserver` - The URL of the homeserver to register on.
    /// * `registration_token` - Token for homeservers that restrict registration.
    pub async fn register(
        &self,
        username: String,
        password: String,
        homeserver: String,
        registration_token: Option<String>,
    ) -> anyhow::Result<ClientHandler> {
        // Scoped so the store-less client is dropped before the real one is built.
        let session = {
            let auth_client = self.get_auth_client(&homeserver).await?;

            let mut request = RegistrationRequest::new();
            request.username = Some(username.clone());
            request.password = Some(password.clone());
            if let Some(token) = registration_token.clone() {
                request.auth = Some(AuthData::RegistrationToken(RegistrationToken::new(token)));
            }

            if let Err(e) = auth_client.matrix_auth().register(request.clone()).await {
                // A rejection carrying a UIAA session is the server asking for a
                // challenge round rather than a failure. Anything else is a failure.
                let uiaa_info = e
                    .as_uiaa_response()
                    .ok_or_else(|| anyhow::anyhow!("Registration failed: {e:?}"))?;
                debug!(
                    "Registration needs challenge-response, replaying with the server's session"
                );

                let token = registration_token.ok_or_else(|| {
                    anyhow::anyhow!("Registration token required for UIAA challenge-response")
                })?;
                let mut reg_token = RegistrationToken::new(token);
                reg_token.session = uiaa_info.session.clone();
                request.auth = Some(AuthData::RegistrationToken(reg_token));

                let response = auth_client.matrix_auth().register(request).await?;
                debug!("Registered {} after challenge-response", response.user_id);
            }

            // A homeserver may create the account without signing it in, in which case
            // the registration response carries no token and there is no session yet.
            if auth_client.session_tokens().is_none() {
                debug!("Registration returned no token, logging in to establish a session");
                auth_client
                    .matrix_auth()
                    .login_username(&username, &password)
                    .initial_device_display_name("Echelon")
                    .send()
                    .await?;
            }

            session_of(&auth_client)?
        };

        let user_id = session.user_id.clone();
        self.app_state.secret_service.set_session(&session)?;
        self.app_state
            .echelon_store
            .add_account(&user_id, &homeserver)?;

        self.restore_session(user_id, Some(homeserver))
            .await?
            .ok_or_else(|| anyhow::anyhow!("No client handler after registration"))
    }
}
