use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::api::client::{
    filter::{FilterDefinition, LazyLoadOptions, RoomEventFilter, RoomFilter},
    sync::sync_events::v3::Filter,
};
use matrix_sdk::ruma::uint;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, warn};

fn build_sync_filter() -> FilterDefinition {
    let mut state_filter = RoomEventFilter::default();
    state_filter.lazy_load_options = LazyLoadOptions::Enabled {
        include_redundant_members: false,
    };

    let mut timeline_filter = RoomEventFilter::default();
    timeline_filter.limit = Some(uint!(20));

    let mut room_filter = RoomFilter::default();
    room_filter.include_leave = false;
    room_filter.state = state_filter;
    room_filter.timeline = timeline_filter;

    let mut filter = FilterDefinition::default();
    filter.room = room_filter;
    filter
}

pub struct SyncManager {
    sync_handle: RwLock<Option<JoinHandle<()>>>,
}

impl SyncManager {
    pub fn new() -> Self {
        Self {
            sync_handle: RwLock::new(None),
        }
    }

    /// Start the sync loop for a given Matrix client
    pub async fn start_sync(&self, client: Client) {
        self.stop_sync().await;

        let handle = tokio::spawn(async move {
            debug!("Starting Matrix sync loop...");

            // Upload filter once. server caches it and returns a short ID we reuse every sync.
            // Falls back to inline filter on error (still applies filtering, just less efficient).
            let filter_id: Option<String> = match client
                .get_or_upload_filter("echelon_v1", build_sync_filter())
                .await
            {
                Ok(id) => {
                    debug!("Using server-side sync filter: {}", id);
                    Some(id)
                }
                Err(e) => {
                    warn!(
                        "Failed to upload sync filter, falling back to inline: {:?}",
                        e
                    );
                    None
                }
            };

            let mut since: Option<String> = None;
            loop {
                let base = match &filter_id {
                    Some(id) => SyncSettings::default().filter(Filter::FilterId(id.clone())),
                    None => SyncSettings::default()
                        .filter(Filter::FilterDefinition(build_sync_filter())),
                };
                let settings = match &since {
                    Some(token) => base.token(token.clone()),
                    None => base,
                };
                match client.sync_once(settings).await {
                    Ok(response) => {
                        since = Some(response.next_batch);
                    }
                    Err(e) => {
                        error!("Sync error: {:?}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });

        let mut sync_guard = self.sync_handle.write().await;
        *sync_guard = Some(handle);
        debug!("Sync task started");
    }

    /// Stop the current sync loop
    pub async fn stop_sync(&self) {
        let mut sync_guard = self.sync_handle.write().await;

        if let Some(handle) = sync_guard.take() {
            debug!("Stopping sync task...");
            handle.abort();
            debug!("Sync task stopped");
        }
    }

    /// Check if sync is currently running
    pub async fn is_syncing(&self) -> bool {
        let sync_guard = self.sync_handle.read().await;
        sync_guard.is_some()
    }
}

impl Drop for SyncManager {
    fn drop(&mut self) {
        // Try to stop sync when the manager is dropped
        if let Some(handle) = self
            .sync_handle
            .try_write()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            handle.abort();
        }
    }
}
