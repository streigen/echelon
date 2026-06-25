// Prevent console window in addition to Slint window in Windows release builds when, e.g., starting the app via file manager. Ignored on other platforms.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use std::sync::Arc;
use tokio::sync::RwLock;

mod account;
mod app_state;
mod client;
mod commands;
mod events;
mod keyring_client;
mod rooms;
mod secret;
mod spaces;
mod store;
mod stronghold_backend;
mod sync_manager;

use app_state::{app_data_dir, AppState};
use client::ClientHandler;
use keyring_client::KeyringClient;
use secret::SecretService;
use store::EchelonStore;

slint::include_modules!();

pub type ClientState = Arc<RwLock<Option<ClientHandler>>>;

const APP_ID: &str = "net.flaxeneel2.echelon";

fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    keyring_init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let data_dir = app_data_dir(APP_ID);

    let mut stronghold_dir = data_dir.clone();
    stronghold_dir.push("stronghold");

    // Per-user session secrets: each user gets their own keyring entry
    // (keyed by blake3 hash of their user_id) and stronghold snapshot.
    let secret_service = SecretService::new(
        KeyringClient::new(APP_ID.to_string()),
        stronghold_dir.clone(),
    );

    // App-level store (account list, etc.)
    let echelon_store = EchelonStore::new(
        KeyringClient::new(APP_ID.to_string()),
        "store-key".to_string(),
        stronghold_dir,
    );

    let app_state = Arc::new(AppState {
        secret_service,
        echelon_store,
        data_dir,
    });

    let ui = AppWindow::new()?;
    let ui_handle = ui.as_weak();

    let client = rt.block_on(ClientHandler::new(app_state.clone(), ui_handle.clone()))?;
    let client_state: ClientState = Arc::new(RwLock::new(Some(client)));

    ui.on_login({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |username, password, homeserver| {
            handle.block_on(commands::auth::login(
                username.into(), password.into(), homeserver.into(), state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_logout({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move || {
            handle.block_on(commands::auth::logout(state.clone()))
                .map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_register({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |username, password, homeserver, token| {
            let token: Option<String> = if token.is_empty() { None } else { Some(token.into()) };
            handle.block_on(commands::auth::register(
                username.into(), password.into(), homeserver.into(), token, state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_restore_session({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |username, homeserver| {
            handle.block_on(commands::auth::restore_session(
                username.into(), homeserver.into(), state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_oauth_login({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |homeserver| {
            handle.block_on(commands::auth::oauth_login(
                homeserver.into(), state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_oauth_register({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |homeserver| {
            handle.block_on(commands::auth::oauth_register(
                homeserver.into(), state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_reset_account({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |reset_type, password, key_backup| {
            use crate::account::account_reset_types::AccountResetType;
            let account_reset_type = if reset_type == 0 {
                AccountResetType::IdentityReset
            } else {
                AccountResetType::KeyBackupReset
            };
            let password = if password.is_empty() { None } else { Some(password.into()) };
            let key_backup = if key_backup.is_empty() { None } else { Some(key_backup.into()) };
            handle.block_on(commands::account::reset_account(
                account_reset_type, password, key_backup, state.clone(),
            )).map_or_else(|e| e.into(), |s| s.into())
        }
    });

    ui.on_get_spaces({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move || {
            handle.block_on(commands::spaces::get_spaces(state.clone()))
                .map_or_else(|e| e, |v| serde_json::to_string(&v).unwrap_or_default())
                .into()
        }
    });

    ui.on_get_rooms({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move || {
            #[allow(deprecated)]
            let result = handle.block_on(commands::rooms::get_rooms(state.clone()));
            result.map_or_else(|e| e, |v| serde_json::to_string(&v).unwrap_or_default()).into()
        }
    });

    ui.on_get_all_spaces_with_trees({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move || {
            handle.block_on(commands::spaces::get_all_spaces_with_trees(state.clone()))
                .map_or_else(|e| e, |v| serde_json::to_string(&v).unwrap_or_default())
                .into()
        }
    });

    ui.on_get_space_tree({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move |space_id| {
            handle.block_on(commands::spaces::get_space_tree(space_id.into(), state.clone()))
                .map_or_else(|e| e, |v| serde_json::to_string(&v).unwrap_or_default())
                .into()
        }
    });

    ui.on_get_dm_rooms({
        let state = client_state.clone();
        let handle = rt.handle().clone();
        move || {
            handle.block_on(commands::dm::get_dm_rooms(state.clone()))
                .map_or_else(|e| e, |v| serde_json::to_string(&v).unwrap_or_default())
                .into()
        }
    });

    ui.run()?;

    Ok(())
}

fn keyring_init() {
    #[cfg(target_os = "android")]
    {
        use android_native_keyring_store::Store as AndroidStore;
        keyring_core::set_default_store(
            AndroidStore::new().expect("Failed to initialize Android KeyStore")
        );
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use apple_native_keyring_store::Store as AppleStore;
        keyring_core::set_default_store(
            AppleStore::new().expect("Failed to initialize Apple Keychain store")
        );
    }

    #[cfg(target_os = "windows")]
    {
        use windows_native_keyring_store::Store as WindowsStore;
        keyring_core::set_default_store(
            WindowsStore::new().expect("Failed to initialize Windows Credential Manager store")
        );
    }

    #[cfg(target_os = "linux")]
    {
        use zbus_secret_service_keyring_store::Store as SecretServiceStore;
        keyring_core::set_default_store(
            SecretServiceStore::new().expect("Failed to initialize Secret Service store")
        );
    }
}
