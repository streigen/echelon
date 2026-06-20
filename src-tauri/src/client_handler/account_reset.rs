use matrix_sdk::encryption::CrossSigningResetAuthType;
use ruma::api::client::uiaa::{AuthData, Password, UserIdentifier};
use tracing::debug;

use crate::account::account_reset_types::AccountResetType;

use super::ClientHandler;

impl ClientHandler {
    pub async fn reset_account(
        &self,
        account_reset_type: AccountResetType,
        password: Option<String>,
        key_backup: Option<String>,
    ) -> anyhow::Result<()> {
        let client = &self.matrix_client;
        let recovery = client.encryption().recovery();

        match account_reset_type {
            AccountResetType::IdentityReset => {
                debug!("Starting identity reset...");
                if let Some(handle) = recovery.reset_identity().await? {
                    match handle.auth_type() {
                        CrossSigningResetAuthType::Uiaa(uiaa_info) => {
                            debug!("UIAA authentication required for identity reset");
                            if let Some(pwd) = password {
                                // Create password authentication data
                                let user_id = client
                                    .user_id()
                                    .ok_or_else(|| anyhow::anyhow!("No user ID available"))?;

                                let mut password_auth = Password::new(
                                    UserIdentifier::UserIdOrLocalpart(user_id.to_string()),
                                    pwd,
                                );

                                // Set the session if available
                                if let Some(session) = &uiaa_info.session {
                                    password_auth.session = Some(session.clone());
                                }

                                // Perform the reset with password authentication
                                handle
                                    .reset(Some(AuthData::Password(password_auth)))
                                    .await?;
                                debug!("Identity reset completed successfully");
                            } else {
                                return Err(anyhow::anyhow!(
                                    "Password required for UIAA authentication"
                                ));
                            }
                        }
                        CrossSigningResetAuthType::OAuth(oauth_info) => {
                            debug!("OAuth authentication required: {:?}", oauth_info);
                            // For OAuth, the user needs to complete authentication via browser
                            // This typically requires opening a browser and completing the OAuth flow
                            handle.reset(None).await?;
                            debug!("Identity reset initiated with OAuth");
                        }
                    }
                }
            }
            AccountResetType::KeyBackupReset => {
                debug!("Starting key backup reset...");

                if let Some(key_backup) = key_backup {
                    recovery.recover(&key_backup).await?;
                } else {
                    Err(anyhow::anyhow!(
                        "KeyBackup reset required for key backup reset"
                    ))?;
                }
            }
        }

        Ok(())
    }
}

