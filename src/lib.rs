#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use std::future::Future;
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

fn spawn_ui_command<F, Fut>(handle: &tokio::runtime::Handle, _ui: slint::Weak<AppWindow>, f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = slint::SharedString> + Send + 'static,
{
    handle.spawn(async move {
        let start = std::time::Instant::now();
        let msg = f().await;
        println!(
            "Backend Result: {} ({}ms)",
            msg,
            start.elapsed().as_millis()
        );
    });
}

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
        width: a.width.unwrap_or(0) as i32,
        height: a.height.unwrap_or(0) as i32,
        preview: slint::Image::default(),
    }
}

/// The text to show above a message's attachment, which for a bare attachment is nothing.
///
/// A media event's `body` is the file name unless the sender also sent a caption, in which case a
/// separate `filename` field carries the name instead (MSC2530, stable since Matrix 1.10). Only a
/// caption is worth showing, since the file card already displays the name and an image needs no
/// label at all.
///
/// # Arguments
/// * `body` - The message body from the event.
/// * `attachment` - The message's attachment, if it has one.
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
/// * `attachment` - The attachment to download.
fn spawn_image_fetch(
    handle: &tokio::runtime::Handle,
    client_state: ClientState,
    ui_handle: slint::Weak<AppWindow>,
    room_id: String,
    event_id: String,
    attachment: rooms::messages::Attachment,
) {
    handle.spawn(async move {
        let result = match commands::get_active_client(&client_state).await {
            Ok(client) => {
                commands::media::fetch_image(
                    &client,
                    &attachment,
                    commands::media::ImageSize::Display,
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
                let Some(index) = messages.iter().position(|r| r.event_id == event_id) else {
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

/// How long visibility reports are collected before being acted on. This is
/// long enough that a burst collapses into a single pass over the settled
/// values. A scroll gesture is one such burst, as is a page of rows all
/// reporting their pre-layout guess when they are constructed.
const PREVIEW_WINDOW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// Hard ceiling on how many rows hold a decoded preview at once, and with it
/// on the process's pixel memory: a display preview is at most 640x640 RGBA,
/// so this caps them near 38 MB.
///
/// The keep band is the first line of defence and evicts on scroll, but it
/// only fires for rows that exist to report themselves. Under a virtualized
/// list a row is destroyed while still inside the band, so it never reports
/// out. This cap is what bounds previews in that case, since it is driven by
/// loads rather than by visibility.
const MAX_LOADED_PREVIEWS: usize = 24;

/// UI thread bookkeeping for which rows want their previews decoded. It is
/// written by the `preview-window-changed` callback and drained by
/// [`flush_preview_window`].
#[derive(Default)]
struct PreviewWindow {
    /// Whether each reported event id is inside the keep band. This is a map
    /// rather than a list so a row that flaps during a scroll leaves only its
    /// final answer behind.
    pending: std::collections::HashMap<String, bool>,
    /// Event ids whose fetch is already running, so a row crossing the
    /// boundary repeatedly does not stack up duplicate downloads.
    in_flight: std::collections::HashSet<String>,
    /// Event ids whose row currently holds a decoded preview, least recently
    /// loaded first. Kept in step with both eviction paths, so its length is
    /// the real count of live pixel buffers.
    loaded: std::collections::VecDeque<String>,
    flush_queued: bool,
}

thread_local! {
    static PREVIEW_WINDOW: std::cell::RefCell<PreviewWindow> =
        std::cell::RefCell::new(PreviewWindow::default());

    /// The one model behind `UiState.messages`, installed once at startup and
    /// mutated in place from then on.
    ///
    /// Replacing the model instead makes Slint tear down and rebuild every row
    /// in the repeater, which on a long scrollback means thousands of
    /// components destroyed and recreated for a single arriving message. It is
    /// a `thread_local` because `Rc` is not `Send`, and every touch happens on
    /// the UI thread.
    static MESSAGES: std::rc::Rc<slint::VecModel<Message>> =
        std::rc::Rc::new(slint::VecModel::from(Vec::new()));
}

/// Drop the decoded preview held by `event_id`'s row, if it has one. Clears
/// only `preview` and leaves `width`/`height` alone, so the row keeps its size
/// and freeing memory never moves content under the user's cursor.
fn clear_preview_row(messages: &slint::VecModel<Message>, event_id: &str) {
    let Some(index) = messages.iter().position(|r| r.event_id == event_id) else {
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
    // Rows missing from `pending` did not move. Pending ids missing from the
    // model belong to a room that has since been switched away from.
    MESSAGES.with(|messages| {
        for (index, mut row) in messages.iter().enumerate() {
            let event_id = row.event_id.to_string();
            let Some(&inside) = pending.get(&event_id) else {
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
            // Kinds with no preview never load, so skip them before claiming an
            // in flight slot that would only be released again. Borrowed rather
            // than `EventId::parse`, which would allocate an owned id just to
            // probe the map and drop it again.
            let Some(source) = <&EventId>::try_from(event_id.as_str())
                .ok()
                .and_then(|parsed| rooms::messages::get_cached_attachment(parsed_room_id, parsed))
                .filter(|a| !loaded && a.kind.has_preview())
            else {
                continue;
            };
            if PREVIEW_WINDOW.with(|s| s.borrow_mut().in_flight.insert(event_id.clone())) {
                spawn_image_fetch(
                    handle,
                    client_state.clone(),
                    ui_handle.clone(),
                    room_id.clone(),
                    event_id.clone(),
                    source,
                );
            }
        }
    });
}

/// Convert a page of messages into the UI's display shape, caching their
/// attachments so the media source can be found again when the row is
/// clicked or scrolled into view. Nothing is downloaded here, since fetching
/// follows visibility instead.
///
/// # Arguments
/// * `room_id` - The room the page belongs to, used as the attachment cache key.
/// * `messages` - The page to convert.
/// * `display_names` - Sender display names resolved by the fetch, keyed by user id. A sender
///   missing from it falls back to their user id.
fn stored_messages_to_ui(
    room_id: &RoomId,
    messages: Vec<rooms::messages::StoredMessage>,
    display_names: &std::collections::HashMap<ruma::OwnedUserId, String>,
) -> Vec<Message> {
    messages
        .into_iter()
        .map(|m| {
            let event_id = m.event_id.to_string();
            // Cached from the store's own typed id, not the string copy the
            // UI row gets.
            if let Some(attachment) = &m.attachment {
                rooms::messages::cache_attachment(room_id, &m.event_id, attachment);
            }

            let user = ruma::UserId::parse(m.sender.as_ref())
                .ok()
                .and_then(|id| display_names.get(&id))
                .map_or_else(|| m.sender.as_ref(), String::as_str);

            Message {
                user: user.into(),
                time: format_time_of_day(m.origin_server_ts).into(),
                text: if m.redacted {
                    "[message deleted]"
                } else {
                    display_text(&m.body, m.attachment.as_ref())
                }
                .into(),
                repliedTo: "".into(),
                event_id: event_id.into(),
                attachment: attachment_to_ui(m.attachment.as_ref()),
            }
        })
        .collect()
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
    handle.spawn(async move {
        let result = match ruma::RoomId::parse(&room_id) {
            Ok(parsed) => {
                commands::messages::get_messages_from_room_paginated(
                    client_state,
                    parsed,
                    token.clone(),
                    50,
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
            if state.get_active_room_id() != room_id.as_str() {
                return;
            }
            let prepend = token.is_some();
            state.set_messages_loading(false);
            state.set_loading_more(false);

            match result {
                Ok(paginated) => {
                    let Ok(parsed_room_id) = <&RoomId>::try_from(room_id.as_str()) else {
                        return;
                    };
                    let msgs = stored_messages_to_ui(
                        parsed_room_id,
                        paginated.messages,
                        &paginated.display_names,
                    );
                    state.set_next_token(paginated.next_token.unwrap_or_default().into());
                    MESSAGES.with(|messages| {
                        if prepend {
                            // Inserted one at a time, back to front, so the
                            // rows already on screen keep their components and
                            // their decoded previews. Rebuilding the model
                            // instead would throw away the whole scrollback to
                            // add 50 rows to the top of it.
                            for msg in msgs.into_iter().rev() {
                                messages.insert(0, msg);
                            }
                        } else {
                            messages.set_vec(msgs);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Failed to fetch messages: {e}");
                }
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
    chrono::DateTime::from_timestamp_millis(origin_server_ts_ms as i64)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string())
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
    ui.on_login({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui = ui_handle.clone();
        move |username, password, homeserver| {
            let (state, ui) = (state.clone(), ui.clone());
            let (username, password, homeserver) = (
                username.to_string(),
                password.to_string(),
                homeserver.to_string(),
            );
            spawn_ui_command(&handle, ui, move || async move {
                commands::auth::login(username, password, homeserver, state)
                    .await
                    .map_or_else(|e| e.into(), |s| s.into())
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
                // incoming page to overwrite. Doing it now is also what makes
                // the queues below safe to clear alongside them: their entries
                // would otherwise claim buffers belonging to rows on their way
                // out, and evicting those later would clear rows belonging to
                // the new room.
                //
                // It closes the one path where the buffers were never freed at
                // all. A failed fetch never reaches `set_vec`, so the old rows
                // stayed in the model holding their previews, untracked by a
                // queue that had already been cleared, with nothing left that
                // could evict them.
                MESSAGES.with(|messages| messages.set_vec(Vec::new()));
                PREVIEW_WINDOW.with(|s| {
                    let mut window = s.borrow_mut();
                    window.loaded.clear();
                    window.pending.clear();
                });

                ui.global::<UiState>().set_active_room(room_name);
                ui.global::<UiState>().set_active_room_id(room_id);
                ui.global::<UiState>().set_messages_loading(true);
                ui.global::<UiState>().set_next_token("".into());
                ui.global::<UiState>().set_loading_more(false);
            }

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

    // Live message push from the sync loop (see events::ClientEvents::on_message).
    // Only appends to UiState.messages when the event's room is the one currently open;
    // messages for other rooms are dropped here (they'll show up via the paginated
    // fetch next time that channel is opened).
    ui.on_matrix_message({
        let ui_handle = ui_handle.clone();
        move |sender, room_id, body, event_id, time, attachment| {
            if let Some(ui) = ui_handle.upgrade() {
                let state = ui.global::<UiState>();
                if state.get_active_room_id() != room_id {
                    return;
                }
                let new_msg = Message {
                    user: sender,
                    time,
                    text: body,
                    repliedTo: slint::SharedString::from(""),
                    event_id,
                    attachment,
                };
                MESSAGES.with(|messages| messages.push(new_msg));
            }
        }
    });

    // Send message callback. The composer only carries the text, so the room is
    // taken from whichever channel is open at the moment of the send.
    //
    // Nothing is pushed into the model here. The sent event comes back through
    // the sync handler like any other message (see `on_matrix_message`), which
    // is where its real event id, timestamp, and display name come from. A
    // local row as well would show the message twice, since the two carry no
    // shared key to fold them back together. Only failures surface here, as a
    // toast.
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
                    show_toast(&ui, format!("Invalid room id '{active_room_id}': {e}"), true);
                    return;
                }
            };

            let state = state.clone();
            let ui_handle = ui_handle.clone();
            let body = msg_text.to_string();
            handle.spawn(async move {
                let Err(e) = commands::messages::send_message(state, room_id, body).await else {
                    return;
                };
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    show_toast(&ui, format!("Failed to send message: {e}"), true);
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
            // Enlarging only means something for kinds that decode to a
            // raster. A click on a file or audio row must not try to decode
            // that file as an image.
            if !attachment.kind.has_preview() {
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
                            commands::media::ImageSize::Full,
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
        move |event_id, inside| {
            let queue_flush = PREVIEW_WINDOW.with(|s| {
                let mut window = s.borrow_mut();
                window.pending.insert(event_id.to_string(), inside);
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
