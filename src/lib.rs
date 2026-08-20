#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use std::sync::Arc;

mod account;
mod app_state;
mod client;
mod commands;
mod events;
mod rooms;
mod storage;

use app_state::{AppState, app_data_dir};
use client::ClientHandler;
use rooms::room_types::SpaceRoom;
use ruma::{EventId, RoomId};
use slint::Model;
use storage::keyring_client::KeyringClient;
use storage::secret::SecretService;
use storage::store::EchelonStore;

pub use client::ClientState;

slint::include_modules!();

const APP_ID: &str = "com.streigen.echelon";

/// Wraps a `Vec<T>` into a `ModelRc<T>` backed by a fresh `VecModel`.
pub(crate) fn to_model<T: Clone + 'static>(v: Vec<T>) -> slint::ModelRc<T> {
    std::rc::Rc::new(slint::VecModel::from(v)).into()
}

/// Convert a message's optional attachment into its slint generated FFI
/// shape. Slint has no optional type, so "no attachment" is carried as
/// [`AttachmentKind::Empty`] rather than as a separate flag. The `preview`
/// starts empty and is filled in later by [`spawn_image_fetch`].
pub(crate) fn attachment_to_ui(a: Option<&rooms::messages::Attachment>) -> MessageAttachment {
    use rooms::messages::AttachmentKind as Domain;
    let Some(a) = a else {
        return MessageAttachment {
            kind: AttachmentKind::Empty,
            ..Default::default()
        };
    };
    MessageAttachment {
        kind: match a.kind {
            Domain::Image => AttachmentKind::Image,
            Domain::Video => AttachmentKind::Video,
            Domain::Audio => AttachmentKind::Audio,
            Domain::File => AttachmentKind::File,
            Domain::Sticker => AttachmentKind::Sticker,
        },
        mimetype: a.mimetype.as_deref().unwrap_or_default().into(),
        filename: a.filename.as_str().into(),
        savable: a.kind.is_savable(),
        previewable: a.previewable(),
        width: a.width.unwrap_or(0) as i32,
        height: a.height.unwrap_or(0) as i32,
        preview: slint::Image::default(),
    }
}

/// Get display text for a message body and optional attachment.
pub(crate) fn display_text<'a>(
    body: &'a str,
    attachment: Option<&rooms::messages::Attachment>,
) -> &'a str {
    match attachment {
        Some(a) if a.filename == body => "",
        _ => body,
    }
}

/// Download and decode an attachment's raster preview off the UI thread,
/// then patch it into the row with the matching `event_id`. If the fetch
/// outlived a channel switch, the result is dropped instead.
///
/// # Arguments
/// * `handle` - Runtime handle the download is spawned on.
/// * `client_state` - The client state used to resolve the Matrix client.
/// * `ui_handle` - Weak handle used to patch the result back into the UI.
/// * `room_id` - Room the message belongs to, checked before patching.
/// * `event_id` - Event whose row receives the preview.
/// * `hint` - Where that row sat when the fetch was started, checked before use.
/// * `attachment` - The attachment to download.
fn spawn_image_fetch(
    handle: &tokio::runtime::Handle,
    client_state: ClientState,
    ui_handle: slint::Weak<AppWindow>,
    room_id: String,
    event_id: String,
    hint: usize,
    attachment: rooms::messages::Attachment,
) {
    handle.spawn(async move {
        let result = match commands::get_active_client(&client_state).await {
            Ok(client) => {
                commands::media::fetch_image(
                    &client,
                    &attachment,
                    rooms::messages::ImageSize::Display,
                )
                .await
            }
            Err(e) => Err(e),
        };
        let _ = slint::invoke_from_event_loop(move || {
            PREVIEW_WINDOW.with(|s| s.borrow_mut().in_flight.remove(&event_id));
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let state = ui.global::<UiState>();
            if state.get_active_room_id() != room_id {
                return;
            }
            let decoded = match result {
                Ok(decoded) => decoded,
                Err(e) => {
                    eprintln!("Failed to fetch image: {e}");
                    return;
                }
            };

            // `set_row_data` on the messages model repaints just this row.
            // Slint updates the existing repeater item in place rather than
            // rebuilding it, so the row's `init` does not fire again and no
            // spurious visibility report follows from patching a preview in.
            MESSAGES.with(|messages| {
                let Some(index) = row_index(messages, &event_id, Some(hint)) else {
                    return;
                };
                let Some(mut row) = messages.row_data(index) else {
                    return;
                };
                // The decoded dimensions are the real ones. Sender declared
                // sizes are often missing or wrong, and a mismatch renders the
                // image squished.
                row.attachment.width = decoded.width() as i32;
                row.attachment.height = decoded.height() as i32;
                row.attachment.preview = decoded.into_image();
                messages.set_row_data(index, row);
                note_preview_loaded(messages, event_id);
            });
        });
    });
}

/// Debounce duration for window visibility reports.
const PREVIEW_WINDOW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// Maximum loaded previews held in memory.
const MAX_LOADED_PREVIEWS: usize = 24;

/// UI thread bookkeeping for which rows want their previews decoded.
#[derive(Default)]
struct PreviewWindow {
    pending: std::collections::HashMap<String, (usize, bool)>,
    in_flight: std::collections::HashSet<String>,
    loaded: std::collections::VecDeque<String>,
    flush_queued: bool,
}

/// Edits and redactions whose target is not in the model yet.
#[derive(Default)]
struct PendingChanges {
    edits: std::collections::HashMap<ruma::OwnedEventId, String>,
    redactions: std::collections::HashSet<ruma::OwnedEventId>,
}

thread_local! {
    static PREVIEW_WINDOW: std::cell::RefCell<PreviewWindow> =
        std::cell::RefCell::new(PreviewWindow::default());

    /// The message model backing `UiState.messages`.
    static MESSAGES: std::rc::Rc<slint::VecModel<Message>> =
        std::rc::Rc::new(slint::VecModel::from(Vec::new()));

    /// Event IDs currently in `MESSAGES` for deduplication.
    static ROW_INDEX: std::cell::RefCell<std::collections::HashSet<ruma::OwnedEventId>> =
        std::cell::RefCell::new(std::collections::HashSet::new());

    /// Pending edits and redactions awaiting target rows.
    static PENDING: std::cell::RefCell<PendingChanges> =
        std::cell::RefCell::new(PendingChanges::default());

    /// Counter incremented when the message model is reset.
    static MESSAGE_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Cached own display name for the active room.
    static OWN_DISPLAY_NAME: std::cell::RefCell<slint::SharedString> =
        std::cell::RefCell::new(slint::SharedString::new());
}

/// Index of the row standing in for the send `txn_id`, or `None` if it is no
/// longer there.
///
/// A pending row holds its transaction id in `event_id` until the homeserver
/// hands a real one back. Rows come out of a Slint model by value, so every row
/// walked past is a clone; searching from the newest end means the row a send
/// just put up is found in a step or two rather than after the whole scrollback.
fn find_pending_row(messages: &slint::VecModel<Message>, txn_id: &str) -> Option<usize> {
    (0..messages.row_count()).rev().find(|&index| {
        messages
            .row_data(index)
            .is_some_and(|row| row.pending && row.event_id == txn_id)
    })
}

/// Index of the row holding `event_id`, checking `hint` before falling back to a
/// search.
///
/// Rows are handed to Rust with the index they had when they reported themselves, and
/// the model can have been prepended to or trimmed since. Checking the hint costs one
/// row clone and is right in almost every case; when it is not, the search behind it
/// is the one that would have been needed regardless. Searching from the newest end
/// because that is where the rows being looked up almost always are: the newest page
/// is what is on screen.
fn row_index(
    messages: &slint::VecModel<Message>,
    event_id: &str,
    hint: Option<usize>,
) -> Option<usize> {
    let matches = |index: usize| {
        messages
            .row_data(index)
            .is_some_and(|row| row.event_id == event_id)
    };
    if let Some(hint) = hint
        && matches(hint)
    {
        return Some(hint);
    }
    (0..messages.row_count())
        .rev()
        .find(|&index| matches(index))
}

/// Drop the decoded preview held by `event_id`'s row, if it has one. Clears
/// only `preview` and leaves `width`/`height` alone, so the row keeps its size
/// and freeing memory never moves content under the user's cursor.
fn clear_preview_row(messages: &slint::VecModel<Message>, event_id: &str) {
    let Some(index) = row_index(messages, event_id, None) else {
        return;
    };
    let Some(mut row) = messages.row_data(index) else {
        return;
    };
    if row.attachment.preview.size().width == 0 {
        return;
    }
    row.attachment.preview = slint::Image::default();
    messages.set_row_data(index, row);
}

/// Record that `event_id`'s row just had a preview decoded into it, and evict
/// the oldest previews if that puts the count over [`MAX_LOADED_PREVIEWS`].
fn note_preview_loaded(messages: &slint::VecModel<Message>, event_id: String) {
    // The borrow is released before any eviction runs, so `clear_preview_row`
    // is never called with `PREVIEW_WINDOW` already borrowed.
    let evicted = PREVIEW_WINDOW.with(|s| {
        let mut window = s.borrow_mut();
        // A row can be decoded again after a band eviction, so drop any older
        // entry rather than letting the same id sit in the queue twice.
        window.loaded.retain(|id| id != &event_id);
        window.loaded.push_back(event_id);

        let overflow = window.loaded.len().saturating_sub(MAX_LOADED_PREVIEWS);
        window.loaded.drain(..overflow).collect::<Vec<_>>()
    });
    for id in evicted {
        clear_preview_row(messages, &id);
    }
}

/// Messages asked for per fetch, and so the size of one prepended page.
const MESSAGE_PAGE: u32 = 50;

/// Maximum number of message rows held in the UI model.
const MAX_MESSAGE_ROWS: usize = MESSAGE_PAGE as usize * 10;

/// Drop oldest or newest message rows exceeding maximum limit.
fn trim_messages(ui: &AppWindow, drop_oldest: bool) {
    MESSAGES.with(|messages| {
        let overflow = messages.row_count().saturating_sub(MAX_MESSAGE_ROWS);
        if overflow == 0 {
            return;
        }

        let mut dropped = std::collections::HashSet::with_capacity(overflow);
        for _ in 0..overflow {
            let index = if drop_oldest {
                0
            } else {
                messages.row_count() - 1
            };
            let Some(row) = messages.row_data(index) else {
                break;
            };
            dropped.insert(row.event_id.to_string());
            messages.remove(index);
        }

        // The rows took their decoded previews with them, so the queue that stands
        // for the live pixel buffers has to lose them too. Left in, they would be
        // counted against the cap and evict previews that are still on screen.
        // Anything still in flight for them lands on a row that no longer exists and
        // is dropped there.
        PREVIEW_WINDOW.with(|s| {
            let mut window = s.borrow_mut();
            window.loaded.retain(|id| !dropped.contains(id));
            window.pending.retain(|id, _| !dropped.contains(id));
        });

        // Dropped ids have to leave the dedup set as well, or the page that
        // fetches them again would be taken for a duplicate and install nothing.
        ROW_INDEX.with(|index| {
            let mut index = index.borrow_mut();
            index.retain(|id| !dropped.iter().any(|gone| gone == id.as_str()));
        });

        let state = ui.global::<UiState>();
        if drop_oldest {
            // The next page back is anchored on the oldest row still shown, which
            // has just changed. Left pointing at a row that was dropped, the fetch
            // would skip everything between the two.
            if let Some(oldest) = messages.row_data(0) {
                state.set_next_token(oldest.event_id);
            }
        } else {
            state.set_newer_trimmed(true);
        }
    });
}

/// Empty the open channel's rows and everything keyed to them.
///
/// Used when the rows on screen stop standing for anything: a channel switch, and a
/// reload of the latest page. The queues have to go with the rows, since their
/// entries would otherwise claim buffers belonging to rows on their way out, and
/// evicting those later would clear rows belonging to whatever replaced them.
///
/// # Arguments
/// * `ui` - The window whose message view to reset.
fn reset_message_view(ui: &AppWindow) {
    clear_message_rows();
    // Before any fetch this reset goes on to start, so that one carries the new
    // generation and every page still in flight against the old rows carries the old.
    MESSAGE_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    PREVIEW_WINDOW.with(|s| {
        let mut window = s.borrow_mut();
        window.loaded.clear();
        window.pending.clear();
    });

    let state = ui.global::<UiState>();
    state.set_messages_loading(true);
    state.set_next_token(slint::SharedString::new());
    state.set_loading_more(false);
    state.set_newer_trimmed(false);
}

/// How long a toast stays up before clearing itself.
const TOAST_DURATION: std::time::Duration = std::time::Duration::from_secs(5);

thread_local! {
    /// Bumped by every toast, so a timer left over from a replaced message cannot cut the new one
    /// short.
    static TOAST_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Show a transient status line at the bottom of the window. Must be called on the UI thread.
///
/// # Arguments
/// * `ui` - The window to show the toast in.
/// * `text` - The message to display.
/// * `is_error` - Whether to style the toast as a failure.
fn show_toast(ui: &AppWindow, text: String, is_error: bool) {
    let generation = TOAST_GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    let global = ui.global::<UiState>();
    global.set_toast_error(is_error);
    global.set_toast_text(text.into());

    let ui_handle = ui.as_weak();
    slint::Timer::single_shot(TOAST_DURATION, move || {
        // A newer toast is on screen and owns its own expiry.
        if TOAST_GENERATION.with(|g| g.get()) != generation {
            return;
        }
        if let Some(ui) = ui_handle.upgrade() {
            ui.global::<UiState>()
                .set_toast_text(slint::SharedString::new());
        }
    });
}

/// Fetch previews for rows inside the keep band, and drop the decoded
/// buffers of rows outside it. Eviction clears only `preview` and leaves
/// `width` and `height` alone, so the row keeps its size. Freeing memory
/// must never move content under the user's cursor.
fn flush_preview_window(
    handle: &tokio::runtime::Handle,
    client_state: &ClientState,
    ui_handle: &slint::Weak<AppWindow>,
) {
    let Some(ui) = ui_handle.upgrade() else {
        return;
    };
    let pending = PREVIEW_WINDOW.with(|s| {
        let mut state = s.borrow_mut();
        state.flush_queued = false;
        std::mem::take(&mut state.pending)
    });

    let state = ui.global::<UiState>();
    let room_id = state.get_active_room_id().to_string();
    // Validated once for the whole pass, borrowed rather than owned. The
    // string form is kept too: `spawn_image_fetch` compares it back against
    // the UI's active room to spot a channel switch.
    let Ok(parsed_room_id) = <&RoomId>::try_from(room_id.as_str()) else {
        return;
    };
    // Walked over the rows that reported, not over the model. Only rows that
    // crossed the band are in here, which during a scroll is a handful, while the
    // model behind them can be the whole scrollback. A pending id with no row left
    // belongs to a room that has since been switched away from, or to a row that
    // trimming dropped.
    MESSAGES.with(|messages| {
        for (event_id, (hint, inside)) in pending {
            let Some(index) = row_index(messages, &event_id, Some(hint)) else {
                continue;
            };
            let Some(mut row) = messages.row_data(index) else {
                continue;
            };
            let loaded = row.attachment.preview.size().width > 0;
            if !inside {
                if loaded {
                    row.attachment.preview = slint::Image::default();
                    messages.set_row_data(index, row);
                    // Dropped here rather than left to age out, so the queue's
                    // length keeps matching the number of live pixel buffers.
                    PREVIEW_WINDOW.with(|s| s.borrow_mut().loaded.retain(|id| id != &event_id));
                }
                continue;
            }
            if loaded {
                continue;
            }
            // Attachments that are not rendered inline never load, so skip them
            // before claiming an in flight slot that would only be released again.
            // Borrowed rather than `EventId::parse`, which would allocate an owned
            // id just to probe the map and drop it again.
            let Some(source) = <&EventId>::try_from(event_id.as_str())
                .ok()
                .and_then(|parsed| rooms::messages::get_cached_attachment(parsed_room_id, parsed))
                .filter(rooms::messages::Attachment::previewable)
            else {
                continue;
            };
            if PREVIEW_WINDOW.with(|s| s.borrow_mut().in_flight.insert(event_id.clone())) {
                spawn_image_fetch(
                    handle,
                    client_state.clone(),
                    ui_handle.clone(),
                    room_id.clone(),
                    event_id,
                    index,
                    source,
                );
            }
        }
    });
}

/// Stands in for the body of a message that has been deleted.
const REDACTED_TEXT: &str = "[message deleted]";

/// Where an [`EventEffect::New`] row lands in the model.
#[derive(Clone, Copy)]
enum RowPosition {
    /// The newest end, for a live event.
    Newest,
    /// The top, for a page walked newest-first so it ends up oldest-first above
    /// the rows already shown.
    Oldest,
}

/// What [`apply_effect`] did.
enum Applied {
    /// A row was added.
    Inserted,
    /// An existing row was changed in place.
    Patched,
    /// An edit or redaction whose target has no row, held in [`PENDING`].
    Buffered,
    /// An event that already has a row, or nothing the list shows.
    Skipped,
}

/// Fold one [`EventEffect`] into the message model.
fn apply_effect(
    messages: &slint::VecModel<Message>,
    room_id: &RoomId,
    effect: rooms::messages::EventEffect,
    names: &std::collections::HashMap<ruma::OwnedUserId, String>,
    position: RowPosition,
) -> Applied {
    use rooms::messages::EventEffect;

    match effect {
        EventEffect::New(message) => {
            if !ROW_INDEX.with(|index| index.borrow_mut().insert(message.event_id.clone())) {
                return Applied::Skipped;
            }
            if let Some(attachment) = &message.attachment {
                rooms::messages::cache_attachment(room_id, &message.event_id, attachment);
            }

            let index = match position {
                RowPosition::Newest => messages.row_count(),
                RowPosition::Oldest => 0,
            };
            messages.insert(index, message_row(&message, names));
            settle_pending(messages, &message.event_id, index);
            Applied::Inserted
        }

        EventEffect::Edit { target, new_body } => {
            let Some(index) = row_index(messages, target.as_str(), None) else {
                PENDING.with(|p| p.borrow_mut().edits.insert(target, new_body));
                return Applied::Buffered;
            };
            set_row_text(messages, index, &new_body);
            Applied::Patched
        }

        EventEffect::Redact { target } => {
            let Some(index) = row_index(messages, target.as_str(), None) else {
                PENDING.with(|p| p.borrow_mut().redactions.insert(target));
                return Applied::Buffered;
            };
            redact_row(messages, index);
            Applied::Patched
        }

        EventEffect::Ignore => Applied::Skipped,
    }
}

/// Build the row for a new message.
fn message_row(
    message: &rooms::messages::NewMessage,
    names: &std::collections::HashMap<ruma::OwnedUserId, String>,
) -> Message {
    let user = names
        .get(&*message.sender)
        .map_or_else(|| message.sender.as_str(), String::as_str);

    Message {
        user: user.into(),
        time: format_time_of_day(message.origin_server_ts).into(),
        text: display_text(&message.body, message.attachment.as_ref()).into(),
        repliedTo: "".into(),
        event_id: message.event_id.as_str().into(),
        attachment: attachment_to_ui(message.attachment.as_ref()),
        pending: false,
        edited: false,
    }
}

/// Apply a buffered edit or redaction for a newly inserted row.
fn settle_pending(messages: &slint::VecModel<Message>, event_id: &EventId, index: usize) {
    let (edit, redacted) = PENDING.with(|p| {
        let mut pending = p.borrow_mut();
        (
            pending.edits.remove(event_id),
            pending.redactions.remove(event_id),
        )
    });

    if redacted {
        redact_row(messages, index);
        return;
    }
    if let Some(new_body) = edit {
        set_row_text(messages, index, &new_body);
    }
}

/// Replace a row's body and mark it edited.
fn set_row_text(messages: &slint::VecModel<Message>, index: usize, text: &str) {
    let Some(mut row) = messages.row_data(index) else {
        return;
    };
    row.text = text.into();
    row.edited = true;
    messages.set_row_data(index, row);
}

/// Blank a deleted message's row in place.
fn redact_row(messages: &slint::VecModel<Message>, index: usize) {
    let Some(mut row) = messages.row_data(index) else {
        return;
    };
    let had_preview = row.attachment.preview.size().width > 0;
    let event_id = row.event_id.to_string();

    row.text = REDACTED_TEXT.into();
    row.attachment = attachment_to_ui(None);
    row.edited = false;
    messages.set_row_data(index, row);

    if had_preview {
        PREVIEW_WINDOW.with(|s| s.borrow_mut().loaded.retain(|id| id != &event_id));
    }
}

/// Fold a live event's effect into the open room's rows.
pub(crate) fn apply_live_effect(
    ui_handle: &slint::Weak<AppWindow>,
    room_id: &RoomId,
    effect: rooms::messages::EventEffect,
    names: &std::collections::HashMap<ruma::OwnedUserId, String>,
) {
    let Some(ui) = ui_handle.upgrade() else {
        return;
    };
    if ui.global::<UiState>().get_active_room_id() != room_id.as_str() {
        return;
    }
    MESSAGES.with(|messages| {
        apply_effect(messages, room_id, effect, names, RowPosition::Newest);
    });
}

/// Empty the message rows and reset associated indices.
fn clear_message_rows() {
    MESSAGES.with(|messages| messages.set_vec(Vec::new()));
    ROW_INDEX.with(|index| index.borrow_mut().clear());
    PENDING.with(|pending| *pending.borrow_mut() = PendingChanges::default());
}

/// Resolve the account's display name in `room_id`.
fn resolve_own_display_name(
    handle: &tokio::runtime::Handle,
    client_state: ClientState,
    ui_handle: slint::Weak<AppWindow>,
    room_id: String,
) {
    handle.spawn(async move {
        let Ok(parsed) = ruma::RoomId::parse(&room_id) else {
            return;
        };
        let name = match commands::messages::own_display_name(client_state, parsed).await {
            Ok(name) => name,
            Err(e) => {
                // Only costs the pending row its label, so there is nothing to
                // report to the user here.
                eprintln!("Failed to resolve own display name: {e}");
                return;
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            // The channel may have been switched while this was in flight, and
            // the name of the room being left must not label a message sent to
            // the one now open.
            if ui.global::<UiState>().get_active_room_id() != room_id.as_str() {
                return;
            }
            OWN_DISPLAY_NAME.with(|held| *held.borrow_mut() = name.into());
        });
    });
}

/// Fetch one page of a room's messages and install it in the UI.
///
/// # Arguments
/// * `handle` - Runtime handle the fetch is spawned on.
/// * `client_state` - The client state to fetch through.
/// * `ui_handle` - Weak handle to the window that receives the page.
/// * `room_id` - The room to fetch messages for.
/// * `token` - `None` to replace the model when opening a channel, or the
///   pagination token to prepend an older page when scrolling up.
fn fetch_message_page(
    handle: &tokio::runtime::Handle,
    client_state: ClientState,
    ui_handle: slint::Weak<AppWindow>,
    room_id: String,
    token: Option<String>,
) {
    // Read before the spawn, on the UI thread that owns it, so it names the rows this
    // page is being fetched against rather than whatever has replaced them by the time
    // it lands.
    let generation = MESSAGE_GENERATION.with(|g| g.get());

    handle.spawn(async move {
        let result = match ruma::RoomId::parse(&room_id) {
            Ok(parsed) => {
                commands::messages::get_messages_from_room_paginated(
                    client_state,
                    parsed,
                    token.clone(),
                    MESSAGE_PAGE,
                )
                .await
            }
            Err(e) => Err(format!("Invalid room id '{room_id}': {e}")),
        };

        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let state = ui.global::<UiState>();
            // Guard against a stale response. If the user switched channels
            // while this fetch was in flight, dropping the result is better
            // than clobbering whatever is now loading for the open channel.
            //
            // The generation covers the case the room id cannot: a reload of the
            // latest page empties the model without leaving the room, so a page still
            // on its way back would pass the room check and be prepended onto rows it
            // does not join up with. Both paths reset `loading-more` on their way
            // through `reset_message_view`, so returning here strands nothing.
            if state.get_active_room_id() != room_id.as_str()
                || MESSAGE_GENERATION.with(|g| g.get()) != generation
            {
                return;
            }
            let prepend = token.is_some();
            state.set_messages_loading(false);
            if !prepend {
                // A prepended page holds it past the fetch instead, until the scroll
                // position has been corrected for the rows it added. See the epoch
                // handshake below and chat-main's restore-scroll-timer.
                state.set_loading_more(false);
            }

            let mut prepended = 0;
            match result {
                Ok(page) => {
                    if let Ok(parsed_room_id) = <&RoomId>::try_from(room_id.as_str()) {
                        state.set_next_token(page.next_token.unwrap_or_default().into());
                        // The latest page stands for the whole view rather than
                        // joining onto it. The reset that started this fetch has
                        // already emptied the model in the paths that go through
                        // it, so this only guards the ones that do not.
                        if !prepend {
                            clear_message_rows();
                        }
                        MESSAGES.with(|messages| {
                            // Walked newest-first, with every row inserted at the
                            // top of the page's span, so the page lands oldest-first
                            // above the rows already shown. Inserting row by row is
                            // what lets the list view update its scroll anchor
                            // cleanly, which a whole-model reset does not.
                            for effect in page.effects.into_iter().rev() {
                                if matches!(
                                    apply_effect(
                                        messages,
                                        parsed_room_id,
                                        effect,
                                        &page.display_names,
                                        RowPosition::Oldest,
                                    ),
                                    Applied::Inserted
                                ) {
                                    prepended += 1;
                                }
                            }
                        });
                    }
                }
                Err(e) => {
                    eprintln!("Failed to fetch messages: {e}");
                }
            }

            // Every way out of the arm above lands here, including the ones that
            // installed nothing. The UI is waiting on this to release `loading-more`,
            // so a page that failed or came back empty has to say so as plainly as one
            // that worked, or scrolling up would never ask again.
            if prepend {
                state.set_prepend_count(prepended);
                state.set_prepend_epoch(state.get_prepend_epoch().wrapping_add(1));
            }
        });
    });
}

fn room_data_of(room: &matrix_sdk::Room) -> RoomData {
    let id = room.room_id().to_string();
    let name = room.name().unwrap_or_else(|| id.clone());
    RoomData {
        id: id.into(),
        name: name.into(),
        r_type: "text".into(),
        encrypted: room.encryption_state().is_encrypted(),
    }
}

/// Converts a `get_space_hierarchy` tree into `(tab, categories)` pairs for the
/// UI, one pair per root space only. Subspaces are never their own tab.
/// Within a tab, a root's direct channels become a "CHANNELS" category and
/// each (possibly nested) subspace becomes its own named category, giving a
/// folder-like grouping without needing more than one level of UI nesting.
fn space_hierarchy_to_ui(roots: Vec<SpaceRoom>) -> Vec<(SpaceTab, Vec<CategoryData>)> {
    roots.into_iter().map(space_root_to_ui).collect()
}

fn space_root_to_ui(node: SpaceRoom) -> (SpaceTab, Vec<CategoryData>) {
    let room_id = node.room.room_id().to_string();
    let name = node.room.name().unwrap_or_else(|| room_id.clone());
    let abbrev: String = name.chars().take(2).collect::<String>().to_lowercase();

    let mut categories = Vec::new();
    let mut root_channels = Vec::new();
    let mut subspaces = Vec::new();
    for child in node.children {
        if child.room.is_space() {
            subspaces.push(child);
        } else {
            root_channels.push(room_data_of(&child.room));
        }
    }
    if !root_channels.is_empty() {
        categories.push(CategoryData {
            name: "CHANNELS".into(),
            collapsed: false,
            rooms: to_model(root_channels),
        });
    }
    for subspace in subspaces {
        append_subspace_categories(subspace, &mut categories);
    }

    let tab = SpaceTab {
        id: room_id.into(),
        name: name.into(),
        abbrev: abbrev.into(),
    };
    (tab, categories)
}

/// Recursively turns a subspace (and any subspaces nested inside it) into one
/// `CategoryData` per space node, named after that space, holding its direct
/// non-space children.
fn append_subspace_categories(node: SpaceRoom, out: &mut Vec<CategoryData>) {
    let room_id = node.room.room_id().to_string();
    let name = node.room.name().unwrap_or_else(|| room_id.clone());

    let mut channels = Vec::new();
    let mut subspaces = Vec::new();
    for child in node.children {
        if child.room.is_space() {
            subspaces.push(child);
        } else {
            channels.push(room_data_of(&child.room));
        }
    }

    out.push(CategoryData {
        name: name.into(),
        collapsed: false,
        rooms: to_model(channels),
    });

    for subspace in subspaces {
        append_subspace_categories(subspace, out);
    }
}

/// Copies `UiState.space-channels[idx].categories` into `UiState.categories`,
/// and updates `UiState.active-space-name` from `UiState.space-tabs[idx]` so
/// the sidebar header tracks whichever space is active.
fn apply_space_categories(ui: &AppWindow, idx: usize) {
    let state = ui.global::<UiState>();

    let categories = state
        .get_space_channels()
        .row_data(idx)
        .map(|sc| sc.categories.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    state.set_categories(to_model(categories));

    let space_name = state
        .get_space_tabs()
        .row_data(idx)
        .map(|tab| tab.name)
        .unwrap_or_default();
    state.set_active_space_name(space_name);
}

/// Formats a millisecond Matrix timestamp (always UTC per the Matrix spec) as
/// a "HH:MM" time-of-day string in the system's local timezone.
fn format_time_of_day(origin_server_ts_ms: u64) -> String {
    jiff::Timestamp::from_millisecond(origin_server_ts_ms as i64)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| "--:--".to_string())
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    slint::android::init(app).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to build android tokio runtime");
    rt.block_on(async {
        use tracing::error;
        if let Err(e) = run_app().await {
            error!("Echelon crashed with error: {:?}", e);
        }
    })
}

pub async fn run_app() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    keyring_init();
    let data_dir = app_data_dir(APP_ID);

    let mut stronghold_dir = data_dir.clone();
    stronghold_dir.push("stronghold");

    let secret_service = SecretService::new(
        KeyringClient::new(APP_ID.to_string()),
        stronghold_dir.clone(),
    );

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

    // Installed once. Nothing calls `set_messages` after this, so the model
    // behind `UiState.messages` stays the same object for the process's life
    // and every update is a mutation of it.
    ui.global::<UiState>()
        .set_messages(MESSAGES.with(|messages| messages.clone().into()));

    // Setup client state
    let client = ClientHandler::new(app_state.clone(), ui_handle.clone()).await?;
    let client_state: ClientState = Arc::new(tokio::sync::RwLock::new(Some(client)));
    let rt_handle = tokio::runtime::Handle::current();

    // UI Auth hooks (mapping to backend)
    // Both outcomes are reported as a toast. Until this, a failed login and a
    // successful one looked exactly alike from the window: the result went to stdout
    // and nothing on screen moved.
    ui.on_login({
        let client_state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |username, password, homeserver| {
            let (client_state, ui_handle) = (client_state.clone(), ui_handle.clone());
            let (username, password, homeserver) = (
                username.to_string(),
                password.to_string(),
                homeserver.to_string(),
            );
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_loading(true);
            }
            handle.spawn(async move {
                let result =
                    commands::auth::login(username, password, homeserver, client_state).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    ui.set_loading(false);
                    match result {
                        Ok(message) => show_toast(&ui, message, false),
                        Err(e) => show_toast(&ui, e, true),
                    }
                });
            });
        }
    });

    // Populate the sidebar from the live space hierarchy, select the first space
    // tab, and open its first channel (if any).
    ui.on_open_chat({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let state = state.clone();
            let ui_handle = ui_handle.clone();
            handle.spawn(async move {
                let result = commands::spaces::get_space_hierarchy(state).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(hierarchy) => {
                            let flat = space_hierarchy_to_ui(hierarchy);

                            let (tabs, channels): (Vec<SpaceTab>, Vec<SpaceChannels>) = flat
                                .into_iter()
                                .map(|(tab, categories)| {
                                    (
                                        tab,
                                        SpaceChannels {
                                            categories: to_model(categories),
                                        },
                                    )
                                })
                                .unzip();

                            ui.global::<UiState>().set_space_tabs(to_model(tabs));
                            ui.global::<UiState>()
                                .set_space_channels(to_model(channels));
                            ui.global::<UiState>().set_active_space_index(0);
                            apply_space_categories(&ui, 0);
                        }
                        Err(e) => {
                            eprintln!("Failed to fetch space hierarchy: {e}");
                        }
                    }
                });
            });
        }
    });

    // Space tab clicked. Swap in that space's cached channel list, no backend call,
    // then auto-open the space's first channel (first non-empty category's first room)
    // by driving the same callback a click on it would fire.
    ui.global::<UiState>().on_space_clicked({
        let ui_handle = ui_handle.clone();
        move |idx| {
            if let Some(ui) = ui_handle.upgrade() {
                let idx = idx.max(0) as usize;
                ui.global::<UiState>().set_active_space_index(idx as i32);
                apply_space_categories(&ui, idx);

                let state = ui.global::<UiState>();
                let first_room = state
                    .get_categories()
                    .iter()
                    .find_map(|cat| cat.rooms.iter().next());
                if let Some(room) = first_room {
                    state.invoke_channel_clicked(room.id, room.name);
                }
            }
        }
    });

    // Channel clicked. Fetch its messages via commands::messages and show them.
    ui.global::<UiState>().on_channel_clicked({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |room_id, room_name| {
            let room_id_str = room_id.to_string();

            if let Some(ui) = ui_handle.upgrade() {
                // Drop the room being left, and the one being opened. The
                // open refetches its first page and rebuilds those entries,
                // so keeping them would only hold the scrollback of the last
                // visit alive behind a model that no longer shows it. On the
                // very first open the previous id is empty, which simply
                // fails to parse and clears nothing.
                let previous_room_id = ui.global::<UiState>().get_active_room_id();
                for id in [previous_room_id.as_str(), room_id_str.as_str()] {
                    if let Ok(parsed) = <&RoomId>::try_from(id) {
                        rooms::messages::clear_room_attachments(parsed);
                    }
                }

                // Drop the rows of the room being left, and every decoded
                // preview they hold, here rather than leaving them for the
                // incoming page to overwrite.
                //
                // It closes the one path where the buffers were never freed at
                // all. A failed fetch never reaches `set_vec`, so the old rows
                // stayed in the model holding their previews, untracked by a
                // queue that had already been cleared, with nothing left that
                // could evict them.
                reset_message_view(&ui);
                // Belongs to the room being left. The resolve below replaces it.
                OWN_DISPLAY_NAME.with(|name| *name.borrow_mut() = slint::SharedString::new());

                ui.global::<UiState>().set_active_room(room_name);
                ui.global::<UiState>().set_active_room_id(room_id);
                // Set in step with the property above, so the sync loop's worker
                // never sees the two disagree. It is what lets the live message
                // handler drop another room's message before resolving anything
                // for it. An id that does not parse clears the mirror rather than
                // leaving the last room's in place: nothing can be live for a room
                // this client cannot name, and the page fetch below fails for it
                // too.
                rooms::set_active_room(ruma::RoomId::parse(&room_id_str).ok());
            }

            resolve_own_display_name(
                &handle,
                state.clone(),
                ui_handle.clone(),
                room_id_str.clone(),
            );
            fetch_message_page(&handle, state.clone(), ui_handle.clone(), room_id_str, None);
        }
    });

    // User scrolled near the top of the currently open room. Fetch the next
    // and older page using the token from the previous fetch, then prepend it.
    ui.global::<UiState>().on_load_older_messages({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let ui_state = ui.global::<UiState>();
            let room_id_str = ui_state.get_active_room_id().to_string();
            let token = ui_state.get_next_token().to_string();
            if token.is_empty() {
                ui_state.set_loading_more(false);
                return;
            }

            fetch_message_page(
                &handle,
                state.clone(),
                ui_handle.clone(),
                room_id_str,
                Some(token),
            );
        }
    });

    // The user scrolled back down to the bottom after trimming dropped the newest
    // rows. Nothing paginates forwards, so the latest page is refetched whole, which
    // is the same work opening the channel does and is served from the local event
    // cache the same way.
    ui.global::<UiState>().on_load_latest_messages({
        let client_state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move || {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let room_id = ui.global::<UiState>().get_active_room_id().to_string();
            if room_id.is_empty() {
                return;
            }
            // Clears `newer-trimmed`, which is what stops the scroll handler from
            // asking again before this fetch has landed.
            reset_message_view(&ui);
            fetch_message_page(
                &handle,
                client_state.clone(),
                ui_handle.clone(),
                room_id,
                None,
            );
        }
    });

    // Fired by the scroll timers once the position they were restoring has settled,
    // so rows only ever go away on the side the user has scrolled away from.
    ui.global::<UiState>().on_trim_messages({
        let ui_handle = ui_handle.clone();
        move |drop_oldest| {
            if let Some(ui) = ui_handle.upgrade() {
                trim_messages(&ui, drop_oldest);
            }
        }
    });

    // Send message callback. The composer only carries the text, so the room is
    // taken from whichever channel is open at the moment of the send.
    //
    // The row goes up here, dimmed, before the request is made, so a message
    // appears as it is typed rather than a round trip later. The send's own
    // response settles it: on success the row takes the event id the homeserver
    // assigned and stops being pending, and on failure it comes back down with
    // the reason shown as a toast. The homeserver's echo of the same event is
    // dropped by `on_matrix_message`, since this row already stands for it.
    ui.global::<UiState>().on_send_message({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |msg_text| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let active_room_id = ui.global::<UiState>().get_active_room_id();
            if active_room_id.is_empty() {
                show_toast(&ui, "No channel open".to_string(), true);
                return;
            }
            let room_id = match ruma::RoomId::parse(active_room_id.as_str()) {
                Ok(room_id) => room_id,
                Err(e) => {
                    show_toast(
                        &ui,
                        format!("Invalid room id '{active_room_id}': {e}"),
                        true,
                    );
                    return;
                }
            };

            // Names both the send and the row standing in for it, and is what
            // ties the two back together once the homeserver answers.
            let txn_id = ruma::TransactionId::new();
            MESSAGES.with(|messages| {
                messages.push(Message {
                    // Empty only if the channel's resolve has not landed yet, in
                    // which case the row is labelled when the send settles.
                    user: OWN_DISPLAY_NAME.with(|name| name.borrow().clone()),
                    // The homeserver stamps the event itself, but has not been
                    // asked yet. Both are shown to the minute, so the local
                    // clock reads the same as the stamp that replaces it.
                    time: format_time_of_day(jiff::Timestamp::now().as_millisecond() as u64).into(),
                    text: msg_text.clone(),
                    repliedTo: slint::SharedString::from(""),
                    event_id: txn_id.as_str().into(),
                    attachment: attachment_to_ui(None),
                    pending: true,
                    edited: false,
                });
            });

            let state = state.clone();
            let ui_handle = ui_handle.clone();
            let body = msg_text.to_string();
            handle.spawn(async move {
                let result =
                    commands::messages::send_message(state, room_id, body, txn_id.clone()).await;
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    MESSAGES.with(|messages| {
                        // Gone if the channel was switched while the send was in
                        // flight, which clears the model. The message is in the
                        // room regardless and shows up when it is reopened.
                        let Some(index) = find_pending_row(messages, txn_id.as_str()) else {
                            return;
                        };
                        let Ok(event_id) = &result else {
                            // Takes the text the user typed down with it. It is
                            // in the toast, and keeping it properly is what a
                            // resend queue would be for.
                            messages.remove(index);
                            return;
                        };
                        let Some(mut row) = messages.row_data(index) else {
                            return;
                        };
                        if row.user.is_empty() {
                            row.user = OWN_DISPLAY_NAME.with(|name| name.borrow().clone());
                        }
                        row.event_id = event_id.as_str().into();
                        row.pending = false;
                        messages.set_row_data(index, row);

                        // The row only takes a real event id here, so this is
                        // where it joins the dedup set. Without it a later page
                        // covering this message would install a second row for
                        // it, and an edit or redaction of it would be buffered
                        // against a row that is already on screen.
                        ROW_INDEX.with(|index| index.borrow_mut().insert(event_id.clone()));
                    });
                    if let Err(e) = result {
                        show_toast(&ui, format!("Failed to send message: {e}"), true);
                    }
                });
            });
        }
    });

    // Click to enlarge. The lightbox opens immediately at the cached
    // attachment's known size with a loading state, then swaps in the full
    // resolution image once the fetch lands.
    ui.global::<UiState>().on_open_lightbox({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |event_id| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            let global = ui.global::<UiState>();
            // Only the open room's rows are on screen to be clicked, so its
            // id is the one the clicked attachment was cached under.
            let room_id = global.get_active_room_id();
            let (Ok(room_id), Ok(event_id)) = (
                <&RoomId>::try_from(room_id.as_str()),
                <&EventId>::try_from(event_id.as_str()),
            ) else {
                return;
            };
            let Some(attachment) = rooms::messages::get_cached_attachment(room_id, event_id) else {
                return;
            };
            // Enlarging only means something for an attachment that is rendered
            // inline in the first place. A click on a file card must not try to
            // decode that file as an image, and a row only offers this click when
            // it holds a preview to enlarge.
            if !attachment.previewable() {
                return;
            }

            global.set_lightbox_visible(true);
            global.set_lightbox_loading(true);
            global.set_lightbox_image(slint::Image::default());
            global.set_lightbox_width(attachment.width.unwrap_or(0) as i32);
            global.set_lightbox_height(attachment.height.unwrap_or(0) as i32);
            // The save control acts on this event, and is only offered for kinds worth saving.
            global.set_lightbox_event_id(event_id.as_str().into());
            global.set_lightbox_savable(attachment.kind.is_savable());

            let state = state.clone();
            let ui_handle = ui_handle.clone();
            handle.spawn(async move {
                let result = match commands::get_active_client(&state).await {
                    Ok(client) => {
                        commands::media::fetch_image(
                            &client,
                            &attachment,
                            rooms::messages::ImageSize::Full,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    let global = ui.global::<UiState>();
                    // The user may have closed the lightbox, or opened a
                    // different one, by the time this lands.
                    if !global.get_lightbox_visible() {
                        return;
                    }
                    match result {
                        Ok(decoded) => {
                            global.set_lightbox_width(decoded.width() as i32);
                            global.set_lightbox_height(decoded.height() as i32);
                            global.set_lightbox_image(decoded.into_image());
                            global.set_lightbox_loading(false);
                        }
                        Err(e) => {
                            eprintln!("Failed to fetch full-resolution image: {e}");
                            global.set_lightbox_visible(false);
                        }
                    }
                });
            });
        }
    });

    // Rows report when they cross the preview keep band, and once when they
    // are constructed. Acting a beat later makes a scroll gesture cost one
    // pass instead of one per row per frame, and it keeps model mutation out
    // of the property change handler that produced the report.
    ui.global::<UiState>().on_preview_window_changed({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |index, event_id, inside| {
            let queue_flush = PREVIEW_WINDOW.with(|s| {
                let mut window = s.borrow_mut();
                window
                    .pending
                    .insert(event_id.to_string(), (index.max(0) as usize, inside));
                let queue = !window.flush_queued;
                window.flush_queued = true;
                queue
            });
            if !queue_flush {
                return;
            }
            let state = state.clone();
            let handle = handle.clone();
            let ui_handle = ui_handle.clone();
            slint::Timer::single_shot(PREVIEW_WINDOW_DEBOUNCE, move || {
                flush_preview_window(&handle, &state, &ui_handle);
            });
        }
    });

    ui.global::<UiState>().on_close_lightbox({
        let ui_handle = ui_handle.clone();
        move || {
            if let Some(ui) = ui_handle.upgrade() {
                let global = ui.global::<UiState>();
                global.set_lightbox_visible(false);
                global.set_lightbox_event_id(slint::SharedString::new());
                global.set_lightbox_savable(false);
                // Drop the held image now, rather than leaving a full
                // resolution decode on the model until the next open
                // overwrites it.
                global.set_lightbox_image(slint::Image::default());
            }
        }
    });

    // Save an attachment to disk, from either a file card in the timeline or the lightbox's save
    // control. The dialog runs off the UI thread, so sitting on the file picker does not freeze the
    // app behind it.
    ui.global::<UiState>().on_save_attachment({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |event_id| {
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };
            // Same as open_lightbox: only the open room's rows are on screen to be clicked.
            let room_id = ui.global::<UiState>().get_active_room_id();
            let (Ok(room_id), Ok(event_id)) = (
                <&RoomId>::try_from(room_id.as_str()),
                <&EventId>::try_from(event_id.as_str()),
            ) else {
                return;
            };
            let Some(attachment) = rooms::messages::get_cached_attachment(room_id, event_id) else {
                return;
            };
            // The UI already hides the control for these, so this is only a backstop.
            if !attachment.kind.is_savable() {
                return;
            }

            let state = state.clone();
            let ui_handle = ui_handle.clone();
            handle.spawn(async move {
                let result = match commands::get_active_client(&state).await {
                    Ok(client) => commands::media::save_attachment(&client, &attachment).await,
                    Err(e) => Err(e),
                };
                // Report the path even on desktop, where the dialog can be pointed somewhere the
                // user did not mean. On mobile it is the only way they learn where the file went.
                let toast = match result {
                    Ok(Some(path)) => Some((format!("Saved to {}", path.display()), false)),
                    // The user dismissed the dialog, which is not worth reporting back.
                    Ok(None) => None,
                    Err(e) => Some((format!("Failed to save attachment: {e}"), true)),
                };
                let Some((text, is_error)) = toast else {
                    return;
                };
                if is_error {
                    eprintln!("{text}");
                }
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        show_toast(&ui, text, is_error);
                    }
                });
            });
        }
    });

    // Generic debug console: populate the command list from the registry, then wire a
    // single callback that dispatches by name. Adding a command to
    // `commands::debug::COMMANDS` is all that's needed to make it show up here.
    let debug_commands: Vec<DebugCommand> = commands::debug::COMMANDS
        .iter()
        .map(|spec| DebugCommand {
            name: spec.name.into(),
            arg_labels: spec
                .arg_labels
                .iter()
                .map(|l| slint::SharedString::from(*l))
                .collect::<Vec<_>>()
                .as_slice()
                .into(),
        })
        .collect();
    ui.set_debug_commands(std::rc::Rc::new(slint::VecModel::from(debug_commands)).into());

    ui.on_debug_run_command({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |command, arg0, arg1, arg2, arg3| {
            let state = state.clone();
            let command = command.to_string();
            let args: Vec<String> = vec![
                arg0.to_string(),
                arg1.to_string(),
                arg2.to_string(),
                arg3.to_string(),
            ];
            let ui_handle = ui_handle.clone();
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_debug_busy(true);
            }
            handle.spawn(async move {
                let result = commands::debug::dispatch(&command, &args, state).await;
                let msg: slint::SharedString =
                    result.map_or_else(|e| format!("Error: {e}").into(), |s| s.into());
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_debug_output(msg);
                        ui.set_debug_busy(false);
                    }
                });
            });
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
            AndroidStore::new().expect("Failed to initialize Android KeyStore"),
        );
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        use apple_native_keyring_store::Store as AppleStore;
        keyring_core::set_default_store(
            AppleStore::new().expect("Failed to initialize Apple Keychain store"),
        );
    }

    #[cfg(target_os = "windows")]
    {
        use windows_native_keyring_store::Store as WindowsStore;
        keyring_core::set_default_store(
            WindowsStore::new().expect("Failed to initialize Windows Credential Manager store"),
        );
    }

    #[cfg(target_os = "linux")]
    {
        use zbus_secret_service_keyring_store::Store as SecretServiceStore;
        keyring_core::set_default_store(
            SecretServiceStore::new().expect("Failed to initialize Secret Service store"),
        );
    }
}
