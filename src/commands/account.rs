use crate::ClientState;
use crate::account::account_reset_types::AccountResetType;

/// Reset the account identity or key backup.
///
/// # Arguments
/// * `account_reset_type` - The reset method to execute.
/// * `password` - Optional password for identity reset.
/// * `key_backup` - Optional key backup secret for backup reset.
/// * `state` - The client state.
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
