use std::sync::Arc;

use matrix_sdk::Client;
use tokio::sync::RwLock;
use url::Url;

use crate::app_state::AppState;
use crate::sync_manager::SyncManager;
use crate::AppWindow;

mod account_reset;
mod factory;
mod oauth;
mod password_auth;
mod registration;

pub type ClientState = Arc<RwLock<Option<ClientHandler>>>;

pub struct ClientHandler {
    matrix_client: Client,
    sync_manager: SyncManager,
    pub(crate) app_state: Arc<AppState>,
    pub(crate) ui_handle: slint::Weak<AppWindow>,
}

impl ClientHandler {
    pub async fn new(app_state: Arc<AppState>, ui_handle: slint::Weak<AppWindow>) -> anyhow::Result<Self> {
        let homeserver: Url = Url::parse("https://matrix.org")?;
        let matrix_client = Client::new(homeserver).await?;
        Ok(ClientHandler {
            matrix_client,
            sync_manager: SyncManager::new(),
            app_state,
            ui_handle,
        })
    }

    pub fn get_client(&self) -> &Client {
        &self.matrix_client
    }

    pub async fn start_sync(&self) {
        self.sync_manager.start_sync(self.matrix_client.clone()).await;
    }

    pub async fn stop_sync(&self) {
        self.sync_manager.stop_sync().await;
    }
}
