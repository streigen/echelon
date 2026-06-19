use crate::user::{
    get_all_spaces_with_trees, get_dm_rooms, get_rooms, get_space_tree, get_spaces, login, logout,
    oauth_login, oauth_register, register, reset_account, restore_session,
};
use tauri::Manager;
use tokio::runtime::Runtime;
use tokio::sync::RwLock;

mod account;
mod client_handler;
mod events;
mod keyring_client;
mod rooms;
mod secret;
mod spaces;
mod store;
mod sync_manager;
mod user;

use client_handler::ClientHandler;
use keyring_client::KeyringClient;
use secret::SecretService;
use store::EchelonStore;

pub struct ClientState(pub RwLock<Option<ClientHandler>>);
pub struct SecretState(SecretService);
pub struct StoreState(EchelonStore);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {

            keyring_init();

            #[cfg(target_os = "android")]
            {
                use tracing_subscriber::util::SubscriberInitExt;
                use tracing_subscriber::prelude::__tracing_subscriber_SubscriberExt;

                let app_id = app.config().identifier.clone();
                let android_layer = tracing_android::layer(&app_id)
                    .expect("Failed to create android tracing layer");

                let fmt_layer = tracing_subscriber::fmt::layer()
                    .with_ansi(true)
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_writer(std::io::stdout);

                tracing_subscriber::registry()
                    .with(android_layer)
                    .with(fmt_layer)
                    .with(tracing_subscriber::filter::LevelFilter::DEBUG)
                    .init();
            }

            let rt = Runtime::new().expect("failed to create runtime");
            let app_handle = app.handle().clone();
            let client = rt.block_on(ClientHandler::new(app_handle));
            let client_state = ClientState(RwLock::new(Some(client)));
            let app_data_dir = app.path().app_data_dir()?;
            let app_id = app.config().identifier.clone();

            let mut stronghold_dir = app_data_dir.clone();
            stronghold_dir.push("stronghold");

            // Per-user session secrets: each user gets their own keyring entry
            // (keyed by blake3 hash of their user_id) and stronghold snapshot.
            let secret_service = SecretService::new(
                KeyringClient::new(app_id.clone()),
                stronghold_dir.clone(),
            );

            // App-level store (account list, etc.)
            let echelon_store = EchelonStore::new(
                KeyringClient::new(app_id),
                "store-key".to_string(),
                stronghold_dir,
            );

            let secret_state = SecretState(secret_service);
            let store_state = StoreState(echelon_store);

            app.manage(client_state);
            app.manage(secret_state);
            app.manage(store_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            register,
            login,
            logout,
            restore_session,
            reset_account,
            oauth_login,
            oauth_register,
            get_spaces,
            get_rooms,
            get_all_spaces_with_trees,
            get_space_tree,
            get_dm_rooms,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
        use linux_keyutils_keyring_store::Store as LinuxStore;
        keyring_core::set_default_store(
            LinuxStore::new().expect("Failed to initialize Linux Keyutils store")
        );
    }
}