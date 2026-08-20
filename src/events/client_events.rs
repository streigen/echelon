use std::collections::HashMap;

use matrix_sdk::Room;
use ruma::OwnedUserId;
use ruma::events::room::message::SyncRoomMessageEvent;
use ruma::events::room::redaction::SyncRoomRedactionEvent;
use tracing::{error, trace};

use crate::AppWindow;
use crate::rooms;
use crate::rooms::members;
use crate::rooms::messages::{EventEffect, effect_of_redaction, effect_of_room_message};

pub struct ClientEvents;

impl ClientEvents {
    pub fn register_events(client: &matrix_sdk::Client, ui_handle: slint::Weak<AppWindow>) {
        // Registered per event type, so a message and a redaction arrive on their
        // own handlers and are classified by the matching entry point. Both fold
        // into the model through `apply`, which is the same path the paginated
        // backfill uses.
        let message_handle = ui_handle.clone();
        client.add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
            let ui_handle = message_handle.clone();
            async move {
                trace!("Received message: {:?}", event);

                // An echo of a message this client sent already has a row on
                // screen, put up by `on_send_message` and settled by the send's
                // own response, so appending it would show the message twice.
                // The homeserver hands the transaction id back to the sending
                // device only, which is what makes it safe to drop on: no one
                // else's message carries one.
                if let SyncRoomMessageEvent::Original(original) = &event
                    && original.unsigned.transaction_id.is_some()
                {
                    return;
                }

                Self::apply(effect_of_room_message(event), room, ui_handle).await;
            }
        });

        client.add_event_handler(move |event: SyncRoomRedactionEvent, room: Room| {
            let ui_handle = ui_handle.clone();
            async move {
                trace!("Received redaction: {:?}", event);
                Self::apply(effect_of_redaction(event), room, ui_handle).await;
            }
        });
    }

    /// Fold one live event into the open room's rows.
    async fn apply(effect: EventEffect, room: Room, ui_handle: slint::Weak<AppWindow>) {
        if matches!(effect, EventEffect::Ignore) {
            return;
        }

        // Events for rooms that are not open are dropped: they are refetched when
        // the channel is opened anyway, and caching their attachments would make a
        // busy account pay for every image in every joined room.
        if !rooms::is_active_room(room.room_id()) {
            return;
        }

        // Resolved here because the UI thread cannot await. Only a new message
        // needs a name; an edit or a redaction lands on a row that already has one.
        let names: HashMap<OwnedUserId, String> = match &effect {
            EventEffect::New(message) => {
                members::display_names(&room, std::iter::once(&*message.sender)).await
            }
            _ => HashMap::new(),
        };

        let room_id = room.room_id().to_owned();
        if let Err(e) = slint::invoke_from_event_loop(move || {
            crate::apply_live_effect(&ui_handle, &room_id, effect, &names);
        }) {
            error!("Failed to apply live event: {e}");
        }
    }
}
