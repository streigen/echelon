use crate::ClientState;
use tracing::{debug, trace};

async fn oauth_impl(homeserver: String, state: ClientState) -> Result<String, String> {
    if homeserver.trim().is_empty() {
        return Err("homeserver is required".to_string());
    }

    // Call oauth_login in a separate scope to drop the read lock
    let result = {
        let state_r = state.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.oauth_login(homeserver).await
    };

    match result {
        Ok(Some(handler)) => {
            handler.start_sync().await;
            let mut write_guard = state.write().await;
            *write_guard = Some(handler);
            Ok("OAuth login successful".to_string())
        }
        Ok(None) => Err("OAuth failed: no handler returned".to_string()),
        Err(e) => Err(format!("OAuth failed: {e}")),
    }
}

/// Log in a user with OAuth2 authentication using their homeserver
///
/// # Arguments
/// * `homeserver` - The URL of the homeserver to log in to.
/// * `state` - The client state containing the Matrix client to perform the login on.
pub async fn oauth_login(homeserver: String, state: ClientState) -> Result<String, String> {
    trace!("Starting OAuth login for homeserver: {}", homeserver);
    oauth_impl(homeserver, state).await
}

/// Register a new user with the given username, password, and homeserver. Optionally takes a
/// registration token if the homeserver requires it.
///
/// # Arguments
/// * `username` - The desired username for the new account.
/// * `password` - The desired password for the new account.
/// * `homeserver` - The URL of the homeserver to register the account on.
/// * `registration_token` - Optional token used by homeservers that restrict registration.
/// * `state` - The client state containing the Matrix client to perform registration on.
pub async fn register(
    username: String,
    password: String,
    homeserver: String,
    registration_token: Option<String>,
    state: ClientState,
) -> Result<String, String> {
    trace!("Registering user: {} with password", username);

    if username.trim().is_empty() || password.trim().is_empty() {
        return Err("username and password are required".into());
    }

    // Call register in a separate scope so the read lock is dropped before write access.
    let handler = {
        let state_r = state.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler
            .register(username, password, homeserver, registration_token)
            .await
    };

    let handler = handler.map_err(|e| format!("Registration failed: {}", e))?;

    // Start sync before swapping the state handler.
    handler.start_sync().await;

    // Persist the new handler once the read lock scope has ended.
    let mut write_guard = state.write().await;
    *write_guard = Some(handler);

    Ok("registered".into())
}

/// Log in a user with the given username, password, and homeserver.
///
/// # Arguments
/// * `username` - The username of the account to log in to.
/// * `password` - The password of the account to log in to.
/// * `homeserver` - The URL of the homeserver to log in to.
/// * `state` - The client state containing the Matrix client to perform the login on.
pub async fn login(
    username: String,
    password: String,
    homeserver: String,
    state: ClientState,
) -> Result<String, String> {
    trace!("Logging user: {} with password", username);
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err("username and password are required".to_string());
    }

    // Call login in a separate scope so the read lock is dropped before write access.
    let result = {
        let state_r = state.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.login(username, password, homeserver).await
    };

    match result {
        Ok(Some(handler)) => {
            // Start sync before swapping the state handler.
            handler.start_sync().await;

            // Persist the new handler once the read lock scope has ended.
            let mut write_guard = state.write().await;
            *write_guard = Some(handler);

            Ok("logged in".into())
        }
        Ok(None) => Err("Login failed: No client handler returned".into()),
        Err(e) => Err(format!("Login failed: {}", e)),
    }
}

/// Log out the currently active user session.
///
/// Revokes the session on the homeserver, removes the account entry and stored
/// session secrets, stops the sync task, and clears the client state.
///
/// # Arguments
/// * `state` - The client state containing the Matrix client to perform the logout on.
pub async fn logout(state: ClientState) -> Result<String, String> {
    debug!("Logging out user...");

    // Keep the local logout independent of the homeserver's response.
    let (user_id, revoke_error, app_state, ui_handle) = {
        let state_r = state.read().await;
        let Some(handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };

        let revoke_error = match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            handler.revoke_session(),
        )
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("request timed out after 10 seconds".to_string()),
        };
        handler.stop_sync().await;
        let active_room = handler.active_room_slot();
        *active_room.lock().await = None;

        (
            handler.get_client().user_id().map(|id| id.to_string()),
            revoke_error,
            handler.app_state.clone(),
            handler.ui_handle.clone(),
        )
    };

    // Build the existing unauthenticated handler used by password and OAuth
    // login. If that client cannot be built, still finish local cleanup and
    // report the missing bootstrap handler to the caller.
    let bootstrap = crate::client::ClientHandler::new(app_state.clone(), ui_handle).await;

    // Replacing the handler drops this handler's SQLite client before its
    // account directory is removed. Sync has been awaited and the room
    // subscription cleared, though other in-flight SDK clones may still hold
    // database connections; the cleanup path reports a failed directory removal.
    let mut errors = Vec::new();
    match bootstrap {
        Ok(handler) => *state.write().await = Some(handler),
        Err(error) => {
            *state.write().await = None;
            errors.push(format!("could not prepare login client: {error}"));
        }
    }

    if let Some(user_id) = &user_id {
        if let Err(error) = app_state.echelon_store.remove_account(user_id) {
            errors.push(format!("could not remove account entry: {error}"));
        }

        let account_dir = app_state
            .data_dir
            .join("accounts")
            .join(crate::storage::secret::SecretService::user_id_hash(user_id));
        let database_removed = match std::fs::remove_dir_all(&account_dir) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                errors.push(format!(
                    "could not delete account database at {account_dir:?}: {error}"
                ));
                false
            }
        };

        let secret_cleanup = if database_removed {
            app_state.secret_service.delete_session(user_id)
        } else {
            app_state
                .secret_service
                .delete_session_preserving_key(user_id)
        };
        if let Err(error) = secret_cleanup {
            errors.push(format!("could not delete stored session secrets: {error}"));
        }
    }

    let mut status = "logged out".to_string();
    if let Some(error) = revoke_error {
        status.push_str(&format!(" locally; homeserver revocation failed: {error}"));
    }
    if !errors.is_empty() {
        return Err(format!(
            "{status}; local cleanup incomplete: {}",
            errors.join("; ")
        ));
    }
    Ok(status)
}

/// Restore a previous session for the given user id and optional homeserver.
///
/// This attempts to load the session from secure storage and, if successful,
/// starts the sync loop for that session. It is used for persistence across app restarts.
///
/// # Arguments
/// * `user_id` - The full Matrix user id of the session to restore, as listed by
///   [`crate::storage::store::EchelonStore::get_accounts`]. Sessions are stored under
///   the id the homeserver reported, not under anything the user typed, so a username
///   is not enough to find one.
/// * `homeserver` - Optional homeserver URL override. If omitted, the stored homeserver URL or `.well-known` discovery is used.
/// * `state` - The client state containing the Matrix client to restore on.
pub async fn restore_session(
    user_id: String,
    homeserver: Option<String>,
    state: ClientState,
) -> Result<String, String> {
    debug!("Restoring session for user: {}", user_id);
    if user_id.trim().is_empty() {
        return Err("user id is required".to_string());
    }

    // Call restore_session in a separate scope so the read lock is dropped before write access.
    let handler = {
        let state_r = state.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.restore_session(user_id, homeserver).await
    };

    match handler {
        Ok(Some(handler)) => {
            // Start sync before swapping the state handler.
            handler.start_sync().await;

            // Persist the new handler once the read lock scope has ended.
            let mut write_guard = state.write().await;
            *write_guard = Some(handler);

            Ok("session restored".into())
        }
        Ok(None) => Err("Session restoration failed: No client handler returned".into()),
        Err(e) => Err(format!("Session restoration failed: {}", e)),
    }
}

/// List all persisted accounts on device.
pub async fn list_accounts(state: ClientState) -> Result<String, String> {
    let state_r = state.read().await;
    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };
    let accounts_info = client_handler
        .app_state
        .echelon_store
        .get_accounts()
        .map_err(|e| format!("Failed to get accounts: {e}"))?;

    let last = accounts_info.last.as_deref().unwrap_or("none");
    let mut lines = vec![
        format!("Last account: {last}"),
        format!("Accounts count: {}", accounts_info.accounts.len()),
    ];
    for acc in accounts_info.accounts {
        let hs = acc.homeserver.as_deref().unwrap_or("<unknown/discover>");
        lines.push(format!(" - {} (homeserver: {})", acc.user_id, hs));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
#[path = "../../tests/unit/commands/auth.rs"]
mod tests;
