use crate::ClientState;
use tauri::State;
use tracing::{debug, trace};

/// Log in a user with OAuth2 authentication using their homeserver
///
/// # Arguments
/// * `homeserver` - The URL of the homeserver to log in to.
/// * `state` - The client state containing the Matrix client to perform the login on.
#[tauri::command]
pub async fn oauth_login(
    homeserver: String,
    state: State<'_, ClientState>,
) -> Result<String, String> {
    trace!("Starting OAuth login for homeserver: {}", homeserver);
    if homeserver.trim().is_empty() {
        return Err("homeserver is required".to_string());
    }

    // Call oauth_login in a separate scope to drop the read lock
    let result = {
        let state_r = state.0.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.oauth_login(homeserver, true).await
    };

    match result {
        Ok(Some(handler)) => {
            let client = handler.get_client().clone();
            handler.sync_manager.start_sync(client).await;

            let mut write_guard = state.0.write().await;
            *write_guard = Some(handler);
            Ok("oauth login successful".into())
        }
        Ok(None) => Err("OAuth login failed: no handler returned".into()),
        Err(e) => Err(format!("OAuth login failed: {}", e)),
    }
}

/// Register a user with OAuth2 authentication using their homeserver
///
/// # Arguments
/// * `homeserver` - The URL of the homeserver to register with.
/// * `state` - The client state containing the Matrix client to perform the login on.
#[tauri::command]
pub async fn oauth_register(
    homeserver: String,
    state: State<'_, ClientState>,
) -> Result<String, String> {
    trace!("Starting OAuth register for homeserver: {}", homeserver);
    if homeserver.trim().is_empty() {
        return Err("homeserver is required".to_string());
    }

    // Call oauth_login in a separate scope to drop the read lock
    let result = {
        let state_r = state.0.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.oauth_login(homeserver, false).await
    };

    match result {
        Ok(Some(handler)) => {
            let client = handler.get_client().clone();
            handler.sync_manager.start_sync(client).await;

            let mut write_guard = state.0.write().await;
            *write_guard = Some(handler);
            Ok("oauth registration successful".into())
        }
        Ok(None) => Err("OAuth registration failed: no handler returned".into()),
        Err(e) => Err(format!("OAuth registration failed: {}", e)),
    }
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
#[tauri::command]
pub async fn register(
    username: String,
    password: String,
    homeserver: String,
    registration_token: Option<String>,
    state: State<'_, ClientState>,
) -> Result<String, String> {
    trace!("Registering user: {} with password", username);

    if username.trim().is_empty() || password.trim().is_empty() {
        return Err("username and password are required".into());
    }

    // Call register in a separate scope so the read lock is dropped before write access.
    let handler = {
        let state_r = state.0.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler
            .register(username, password, homeserver, registration_token)
            .await
    };

    let handler = handler.map_err(|e| format!("Registration failed: {}", e))?;

    // Start sync before swapping the state handler.
    let client = handler.get_client().clone();
    handler.sync_manager.start_sync(client).await;

    // Persist the new handler once the read lock scope has ended.
    let mut write_guard = state.0.write().await;
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
#[tauri::command]
pub async fn login(
    username: String,
    password: String,
    homeserver: String,
    state: State<'_, ClientState>,
) -> Result<String, String> {
    trace!("Logging user: {} with password", username);
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err("username and password are required".to_string());
    }

    // Call login in a separate scope so the read lock is dropped before write access.
    let result = {
        let state_r = state.0.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.login(username, password, homeserver).await
    };

    match result {
        Ok(Some(handler)) => {
            // Start sync before swapping the state handler.
            let client = handler.get_client().clone();
            handler.sync_manager.start_sync(client).await;

            // Persist the new handler once the read lock scope has ended.
            let mut write_guard = state.0.write().await;
            *write_guard = Some(handler);

            Ok("logged in".into())
        }
        Ok(None) => Err("Login failed: No client handler returned".into()),
        Err(e) => Err(format!("Login failed: {}", e)),
    }
}

#[tauri::command]
pub async fn logout(state: State<'_, ClientState>) -> Result<String, String> {
    debug!("Logging out user...");

    // Stop the sync task before clearing the client state.
    {
        let state_r = state.0.read().await;
        if let Some(handler) = state_r.as_ref() {
            handler.sync_manager.stop_sync().await;
        }
    }

    // Clear the client state.
    let mut write_guard = state.0.write().await;
    *write_guard = None;

    Ok("logged out".into())
}

/// Restore a previous session for the given username and homeserver.
///
/// This attempts to load the session from secure storage and, if successful,
/// starts the sync loop for that session. It is used for persistence across app restarts.
///
/// # Arguments
/// * `username` - The username of the session to restore.
/// * `homeserver` - The homeserver used to disambiguate sessions.
/// * `state` - The client state containing the Matrix client to restore on.
#[tauri::command]
pub async fn restore_session(
    username: String,
    homeserver: String,
    state: State<'_, ClientState>,
) -> Result<String, String> {
    debug!("Restoring session for user: {}", username);
    if username.trim().is_empty() {
        return Err("username is required".to_string());
    }

    // Call restore_session in a separate scope so the read lock is dropped before write access.
    let handler = {
        let state_r = state.0.read().await;
        let Some(client_handler) = state_r.as_ref() else {
            return Err("No active client session".to_string());
        };
        client_handler.restore_session(username, homeserver).await
    };

    match handler {
        Ok(Some(handler)) => {
            // Start sync before swapping the state handler.
            let client = handler.get_client().clone();
            handler.sync_manager.start_sync(client).await;

            // Persist the new handler once the read lock scope has ended.
            let mut write_guard = state.0.write().await;
            *write_guard = Some(handler);

            Ok("session restored".into())
        }
        Ok(None) => Err("Session restoration failed: No client handler returned".into()),
        Err(e) => Err(format!("Session restoration failed: {}", e)),
    }
}


