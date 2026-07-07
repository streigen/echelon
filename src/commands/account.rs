use crate::account::account_reset_types::AccountResetType;
use crate::ClientState;

/// Reset the account based on the specified reset type and provided credentials or backup key.
///
/// This handles different reset flows (identity reset vs key backup recovery)
/// according to the provided `AccountResetType`.
///
/// # Arguments
/// * `account_reset_type` - The reset method to execute.
/// * `password` - Optional password, required for identity reset.
/// * `key_backup` - Optional key backup secret, required for key backup reset.
/// * `state` - The client state containing the Matrix client for the reset operation.
pub async fn reset_account(
    account_reset_type: AccountResetType,
    password: Option<String>,
    key_backup: Option<String>,
    state: ClientState,
) -> Result<String, String> {
    // Call reset_account in a separate scope so the read lock is dropped promptly.
    let result = {
        let state_r = state.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler
            .reset_account(account_reset_type, password, key_backup)
            .await
    };

    match result {
        Ok(_) => Ok("account reset successful".into()),
        Err(e) => Err(format!("Account reset failed: {}", e)),
    }
}
