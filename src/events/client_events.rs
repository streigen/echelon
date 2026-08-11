use matrix_sdk::Room;
use ruma::events::room::message::SyncRoomMessageEvent;
use serde::Serialize;
use tracing::{error, trace};

use crate::{AppWindow, format_time_of_day};

pub struct ClientEvents;

#[derive(Clone, Serialize)]
struct MessagePayload {
    sender: String,
    room_id: String,
    body: String,
    event_id: String,
    time: String,
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

    async fn on_message(
        event: SyncRoomMessageEvent,
        room: Room,
        ui_handle: slint::Weak<AppWindow>,
    ) {
        trace!("Received message: {:?}", event);

        // Get the content based on event type
        let (sender, body, event_id, origin_server_ts) = match event {
            SyncRoomMessageEvent::Original(original) => {
                let body = original.content.body().to_string();
                (
                    original.sender.to_string(),
                    body,
                    original.event_id.to_string(),
                    original.origin_server_ts,
                )
            }
            SyncRoomMessageEvent::Redacted(redacted) => (
                redacted.sender.to_string(),
                "[Redacted message]".to_string(),
                redacted.event_id.to_string(),
                redacted.origin_server_ts,
            ),
        };

        // Extract message details. Time is formatted as HH:MM
        let payload = MessagePayload {
            sender,
            room_id: room.room_id().to_string(),
            body,
            event_id,
            time: format_time_of_day(origin_server_ts.0.into()),
        };

        // Emit event to frontend
        if let Err(e) = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.invoke_matrix_message(
                    payload.sender.into(),
                    payload.room_id.into(),
                    payload.body.into(),
                    payload.event_id.into(),
                    payload.time.into(),
                );
            }
        }) {
            error!("Failed to emit message event: {}", e);
        }
    }
}
