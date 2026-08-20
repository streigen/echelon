use std::sync::Arc;

use matrix_sdk::Client;
use tokio::sync::RwLock;
use url::Url;

use crate::AppWindow;
use crate::app_state::AppState;
use crate::storage::secret::Session;
use sync_manager::SyncManager;

mod account_reset;
pub(crate) mod active_room;
mod factory;
mod oauth;
mod password_auth;
mod registration;
pub mod sync_manager;

use active_room::ActiveRoomSlot;

pub type ClientState = Arc<RwLock<Option<ClientHandler>>>;

pub struct ClientHandler {
    matrix_client: Client,
    sync_manager: SyncManager,
    /// The open room's event cache subscription slot.
    active_room: ActiveRoomSlot,
    pub(crate) app_state: Arc<AppState>,
    pub(crate) ui_handle: slint::Weak<AppWindow>,
}

impl ClientHandler {
    pub async fn new(
        app_state: Arc<AppState>,
        ui_handle: slint::Weak<AppWindow>,
    ) -> anyhow::Result<Self> {
        let homeserver: Url = Url::parse("https://matrix.org")?;
        let matrix_client = Client::new(homeserver).await?;
        Ok(ClientHandler {
            matrix_client,
            sync_manager: SyncManager::new(),
            active_room: ActiveRoomSlot::default(),
            app_state,
            ui_handle,
        })
    }

    pub fn get_client(&self) -> &Client {
        &self.matrix_client
    }

    /// A handle on the open room's subscription slot.
    ///
    /// Handed out rather than borrowed so the caller can drop the client state
    /// read guard before locking it. See [`ActiveRoomSlot`].
    pub(crate) fn active_room_slot(&self) -> ActiveRoomSlot {
        self.active_room.clone()
    }

    pub async fn start_sync(&self) {
        self.sync_manager
            .start_sync(self.matrix_client.clone())
            .await;
    }

    pub async fn stop_sync(&self) {
        self.sync_manager.stop_sync().await;
    }
}

/// Read the session an authentication request just established off the client it was
/// made on.
///
/// The ids come from the homeserver's answer rather than from anything the user
/// typed. They name the account's store and its stronghold snapshot, and a guess
/// would name a different one on any server whose name differs from its client URL,
/// or whenever the server normalizes the localpart.
///
/// # Arguments
/// * `client` - The client the request was made on, now carrying the session.
pub(crate) fn session_of(client: &Client) -> anyhow::Result<Session> {
    let tokens = client
        .session_tokens()
        .ok_or_else(|| anyhow::anyhow!("Missing session tokens after authentication"))?;
    Ok(Session {
        user_id: client
            .user_id()
            .ok_or_else(|| anyhow::anyhow!("Missing user id after authentication"))?
            .to_string(),
        device_id: client
            .device_id()
            .ok_or_else(|| anyhow::anyhow!("Missing device id after authentication"))?
            .to_string(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    })
}
