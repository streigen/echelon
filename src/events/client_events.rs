use matrix_sdk::Room;
use ruma::events::room::message::SyncRoomMessageEvent;
use slint::ComponentHandle;
use tracing::{error, trace};

use crate::rooms::messages::{attachment_of, cache_attachment};
use crate::{AppWindow, UiState, attachment_to_ui, format_time_of_day};

pub struct ClientEvents;

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

        // Get the content based on event type. The ids stay in their ruma
        // types, since the attachment cache is keyed by them; only the copies
        // handed to the UI are stringified.
        let (sender, body, event_id, origin_server_ts, attachment) = match event {
            SyncRoomMessageEvent::Original(original) => {
                let body = original.content.body().to_string();
                let attachment = attachment_of(&original.content.msgtype);
                (
                    original.sender.to_string(),
                    body,
                    original.event_id,
                    original.origin_server_ts,
                    attachment,
                )
            }
            SyncRoomMessageEvent::Redacted(redacted) => (
                redacted.sender.to_string(),
                "[Redacted message]".to_string(),
                redacted.event_id,
                redacted.origin_server_ts,
                None,
            ),
        };

        let room_id = room.room_id().to_owned();
        let time = format_time_of_day(origin_server_ts.0.into());

        // Emit event to frontend
        if let Err(e) = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            // Messages for other rooms are dropped anyway, since they are
            // refetched when the channel is opened. Bail out before caching
            // anything, otherwise a busy account pays for every image in
            // every joined room.
            if ui.global::<UiState>().get_active_room_id() != room_id.as_str() {
                return;
            }

            ui.invoke_matrix_message(
                sender.into(),
                room_id.as_str().into(),
                body.into(),
                event_id.as_str().into(),
                time.into(),
                attachment_to_ui(attachment.as_ref()),
            );

            // `ATTACHMENT_CACHE` is a `thread_local!`, so it has to be
            // written from the UI thread that reads it, not from this
            // handler's tokio worker. Nothing is downloaded here. The new row
            // reports its own visibility when it is constructed, and the
            // preview fetch follows from that like it does for any row.
            if let Some(attachment) = attachment {
                cache_attachment(&room_id, &event_id, &attachment);
            }
        }) {
            error!("Failed to emit message event: {}", e);
        }
    }
}
