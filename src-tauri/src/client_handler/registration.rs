use matrix_sdk::ruma::api::client::account::register::v3::Request as RegistrationRequest;
use ruma::api::client::uiaa::{AuthData, RegistrationToken};
use tracing::{debug, error};

use crate::sync_manager::SyncManager;

use super::ClientHandler;

impl ClientHandler {
    pub async fn register(
        &self,
        username: String,
        password: String,
        homeserver: String,
        registration_token: Option<String>,
    ) -> anyhow::Result<ClientHandler> {
        let client = self.get_new_client(&username, &homeserver, None).await?;

        let mut registration_request = RegistrationRequest::new();
        registration_request.username = Some(username.clone());
        registration_request.password = Some(password.clone());
        if let Some(token) = registration_token.clone() {
            registration_request.auth =
                Some(AuthData::RegistrationToken(RegistrationToken::new(token)));
        }

        let auth = client.matrix_auth();
        let reg_builder = auth.register(registration_request.clone());

        match reg_builder.await {
            Ok(res) => {
                debug!(
                    "Registration worked immediately (no challenge-response), ID is {}",
                    res.user_id
                );
                Ok(ClientHandler {
                    matrix_client: client,
                    sync_manager: SyncManager::new(),
                    app_handle: self.app_handle.clone(),
                })
            }
            Err(e) => {
                debug!("Registration failed, trying challenge-response");
                if let Some(uiaa_info) = e.as_uiaa_response() {
                    let session = uiaa_info.session.clone();
                    debug!("Received UIAA response with session: {:?}", session);

                    let token = registration_token.ok_or_else(|| {
                        anyhow::anyhow!("Registration token required for UIAA challenge-response")
                    })?;
                    let mut reg_token = RegistrationToken::new(token);
                    reg_token.session = session;
                    let auth_data = AuthData::RegistrationToken(reg_token);
                    registration_request.auth = Some(auth_data);
                    let final_response = client.matrix_auth().register(registration_request).await;

                    match final_response {
                        Ok(res) => {
                            debug!(
                                "Registration successful after challenge-response, ID is {}",
                                res.user_id
                            );
                            Ok(ClientHandler {
                                matrix_client: client,
                                sync_manager: SyncManager::new(),
                                app_handle: self.app_handle.clone(),
                            })
                        }
                        Err(e) => {
                            error!("Registration failed after challenge-response: {:?}", e);
                            Err(anyhow::anyhow!(
                                "Registration failed after challenge-response: {:?}",
                                e
                            ))
                        }
                    }
                } else {
                    error!("Registration failed with error: {:?}", e);
                    Err(anyhow::anyhow!("Registration failed: {:?}", e))
                }
            }
        }
    }
}

