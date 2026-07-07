use matrix_sdk::Room;
use ruma::events::room::message::SyncRoomMessageEvent;
use serde::Serialize;
use tracing::{error, trace};

use crate::AppWindow;

pub struct ClientEvents;

#[derive(Clone, Serialize)]
struct MessagePayload {
    sender: String,
    room_id: String,
    body: String,
    event_id: String,
}

impl ClientEvents {
    pub fn register_events(client: &matrix_sdk::Client, ui_handle: slint::Weak<AppWindow>) {
        client.add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
            let handle = ui_handle.clone();
            async move {
                Self::on_message(event, room, handle).await;
            }
        });
    }

    async fn on_message(event: SyncRoomMessageEvent, room: Room, ui_handle: slint::Weak<AppWindow>) {
        trace!("Received message: {:?}", event);

        // Get the content based on event type
        let (sender, body, event_id) = match event {
            SyncRoomMessageEvent::Original(original) => {
                let body = original.content.body().to_string();
                (
                    original.sender.to_string(),
                    body,
                    original.event_id.to_string(),
                )
            },
            SyncRoomMessageEvent::Redacted(redacted) => {
                (
                    redacted.sender.to_string(),
                    "[Redacted message]".to_string(),
                    redacted.event_id.to_string(),
                )
            },
        };

        // Extract message details
        let payload = MessagePayload {
            sender,
            room_id: room.room_id().to_string(),
            body,
            event_id,
        };

        // Emit event to frontend
        if let Err(e) = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.invoke_on_matrix_message(
                    payload.sender.into(),
                    payload.room_id.into(),
                    payload.body.into(),
                    payload.event_id.into(),
                );
            }
        }) {
            error!("Failed to emit message event: {}", e);
        }
    }
}
