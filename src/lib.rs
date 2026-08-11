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
fn to_model<T: Clone + 'static>(v: Vec<T>) -> slint::ModelRc<T> {
    std::rc::Rc::new(slint::VecModel::from(v)).into()
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
/// UI, one pair per *root* space only — subspaces are never their own tab.
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

/// Converts a fetched, edit/redaction-resolved message into the UI's display shape.
fn stored_message_to_ui(m: &rooms::messages::StoredMessage) -> Message {
    Message {
        user: m.sender.as_ref().into(),
        time: format_time_of_day(m.origin_server_ts).into(),
        text: if m.redacted {
            "[message deleted]".to_string()
        } else {
            m.body.clone()
        }
        .into(),
        repliedTo: "".into(),
        image: false,
    }
}

/// Formats a millisecond Matrix timestamp as a local "HH:MM" time-of-day string.
fn format_time_of_day(origin_server_ts_ms: u64) -> String {
    let secs_of_day = (origin_server_ts_ms / 1000) % 86400;
    format!("{:02}:{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60)
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
                let result = commands::spaces::get_space_hierarchy(state.clone()).await;
                let ui_handle2 = ui_handle.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle2.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(hierarchy) => {
                            let flat = space_hierarchy_to_ui(hierarchy);

                            let tabs: Vec<SpaceTab> =
                                flat.iter().map(|(tab, _)| tab.clone()).collect();
                            let channels: Vec<SpaceChannels> = flat
                                .into_iter()
                                .map(|(_, categories)| SpaceChannels {
                                    categories: to_model(categories),
                                })
                                .collect();

                            ui.global::<UiState>()
                                .set_space_tabs(to_model(tabs));
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

    // Space tab clicked — swap in that space's cached channel list, no backend call.
    ui.global::<UiState>().on_space_clicked({
        let ui_handle = ui_handle.clone();
        move |idx| {
            if let Some(ui) = ui_handle.upgrade() {
                let idx = idx.max(0) as usize;
                ui.global::<UiState>().set_active_space_index(idx as i32);
                apply_space_categories(&ui, idx);
            }
        }
    });

    // Channel clicked — fetch its messages via commands::messages and show them.
    ui.global::<UiState>().on_channel_clicked({
        let state = client_state.clone();
        let handle = rt_handle.clone();
        let ui_handle = ui_handle.clone();
        move |room_id, room_name| {
            let state = state.clone();
            let ui_handle = ui_handle.clone();
            let room_id_str = room_id.to_string();

            if let Some(ui) = ui_handle.upgrade() {
                ui.global::<UiState>().set_active_room(room_name);
                ui.global::<UiState>().set_active_room_id(room_id);
                ui.global::<UiState>().set_messages_loading(true);
            }

            handle.spawn(async move {
                let result = match ruma::RoomId::parse(&room_id_str) {
                    Ok(parsed) => {
                        commands::messages::get_messages_from_room_paginated(
                            state, parsed, None, 50,
                        )
                        .await
                    }
                    Err(e) => Err(format!("Invalid room id '{room_id_str}': {e}")),
                };

                let ui_handle = ui_handle.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = ui_handle.upgrade() else {
                        return;
                    };
                    let state = ui.global::<UiState>();
                    // Guard against a stale response: if the user switched channels again
                    // while this fetch was in flight, active-room-id no longer matches the
                    // room this fetch was for — drop the result instead of clobbering
                    // whatever's now loading/loaded for the newly opened channel.
                    if state.get_active_room_id() != room_id_str.as_str() {
                        return;
                    }
                    state.set_messages_loading(false);
                    match result {
                        Ok(paginated) => {
                            let msgs: Vec<Message> = paginated
                                .messages
                                .iter()
                                .map(stored_message_to_ui)
                                .collect();
                            state.set_messages(
                                std::rc::Rc::new(slint::VecModel::from(msgs)).into(),
                            );
                        }
                        Err(e) => {
                            eprintln!("Failed to fetch messages: {e}");
                        }
                    }
                });
            });
        }
    });

    // Live message push from the sync loop (see events::ClientEvents::on_message).
    // Only appends to UiState.messages when the event's room is the one currently open;
    // messages for other rooms are dropped here (they'll show up via the paginated
    // fetch next time that channel is opened).
    ui.on_matrix_message({
        let ui_handle = ui_handle.clone();
        move |sender, room_id, body, _event_id, time| {
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
                    image: false,
                };
                let mut msgs: Vec<Message> = state.get_messages().iter().collect();
                msgs.push(new_msg);
                state.set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
            }
        }
    });

    // Send message callback — no backend send command yet, so this is UI-local only.
    ui.global::<UiState>().on_send_message({
        let ui_handle = ui_handle.clone();
        move |msg_text| {
            if let Some(ui) = ui_handle.upgrade() {
                let new_msg = Message {
                    user: slint::SharedString::from("me"),
                    time: slint::SharedString::from("just now"),
                    text: msg_text,
                    repliedTo: slint::SharedString::from(""),
                    image: false,
                };
                let global = ui.global::<UiState>();
                let mut msgs: Vec<Message> = global.get_messages().iter().collect();
                msgs.push(new_msg);
                global.set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
            }
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
                let ui_handle = ui_handle.clone();
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
