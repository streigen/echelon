use crate::sync_manager::SyncManager;
use matrix_sdk::Client;
use tauri::{AppHandle, Url};

mod account_reset;
mod factory;
mod oauth_auth;
mod password_auth;
mod registration;

pub struct ClientHandler {
    matrix_client: Client,
    pub sync_manager: SyncManager,
    app_handle: AppHandle,
}

impl ClientHandler {
    pub async fn new(app_handle: AppHandle) -> anyhow::Result<Self> {
        let homeserver: Url = Url::parse("https://matrix.org")?;
        let matrix_client = Client::new(homeserver).await?;
        Ok(ClientHandler {
            matrix_client,
            sync_manager: SyncManager::new(),
            app_handle,
        })
    }

    pub fn get_client(&self) -> &Client {
        &self.matrix_client
    }
}