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

    // Mock Chat Database
    let mut initial_db = std::collections::HashMap::new();
    initial_db.insert(
        "general".to_string(),
        vec![
            Message {
                user: "Clumsy ☆".into(),
                time: "12:02 pm".into(),
                text: "hello world".into(),
                repliedTo: "".into(),
                image: false,
            },
            Message {
                user: "flaxeneel2".into(),
                time: "12:03 pm".into(),
                text: "hello clumsy".into(),
                repliedTo: "Clumsy ☆: hello world".into(),
                image: false,
            },
            Message {
                user: "Clumsy ☆".into(),
                time: "12:03 pm".into(),
                text: "look, cool img:".into(),
                repliedTo: "".into(),
                image: true,
            },
            Message {
                user: "Clumsy ☆".into(),
                time: "12:05 pm".into(),
                text: "very nice image".into(),
                repliedTo: "flaxeneel2: hello clumsy".into(),
                image: false,
            },
        ],
    );
    initial_db.insert(
        "announcements".to_string(),
        vec![
            Message {
                user: "System".into(),
                time: "09:00 am".into(),
                text: "Welcome to echelon beta v0.1.0".into(),
                repliedTo: "".into(),
                image: false,
            },
            Message {
                user: "Clumsy ☆".into(),
                time: "09:05 am".into(),
                text: "Please report any bugs to the dev team!".into(),
                repliedTo: "".into(),
                image: false,
            },
        ],
    );
    initial_db.insert(
        "mission-control".to_string(),
        vec![
            Message {
                user: "Commander".into(),
                time: "18:00 pm".into(),
                text: "Operation Nightfall commences in T-minus 10 hours.".into(),
                repliedTo: "".into(),
                image: false,
            },
            Message {
                user: "Clumsy ☆".into(),
                time: "18:01 pm".into(),
                text: "Roger that.".into(),
                repliedTo: "Commander: Operation Nightfall commences in T-minus 10 hours.".into(),
                image: false,
            },
        ],
    );
    initial_db.insert(
        "intel".to_string(),
        vec![Message {
            user: "Agent X".into(),
            time: "02:00 am".into(),
            text: "Data secured.".into(),
            repliedTo: "".into(),
            image: false,
        }],
    );
    initial_db.insert(
        "very trustworthy".to_string(),
        vec![Message {
            user: "Clumsy ☆".into(),
            time: "14:00 pm".into(),
            text: "This room is highly classified.".into(),
            repliedTo: "".into(),
            image: false,
        }],
    );
    initial_db.insert(
        "very trustworthy x2".to_string(),
        vec![Message {
            user: "flaxeneel2".into(),
            time: "15:00 pm".into(),
            text: "Even more classified in here.".into(),
            repliedTo: "".into(),
            image: false,
        }],
    );

    let db = std::rc::Rc::new(std::cell::RefCell::new(initial_db));

    // Set initial messages via AppState global
    ui.global::<UiState>().set_messages(
        std::rc::Rc::new(slint::VecModel::from(
            db.borrow().get("general").unwrap().clone(),
        ))
        .into(),
    );

    // Room switched callback — via AppState global
    ui.global::<UiState>().on_room_switched({
        let ui_handle = ui_handle.clone();
        let db = db.clone();
        move |room_name: slint::SharedString| {
            if let Some(ui) = ui_handle.upgrade() {
                let msgs = db
                    .borrow()
                    .get(room_name.as_str())
                    .cloned()
                    .unwrap_or_default();
                ui.global::<UiState>()
                    .set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
            }
        }
    });

    // Send message callback — via AppState global
    ui.global::<UiState>().on_send_message({
        let ui_handle = ui_handle.clone();
        let db = db.clone();
        move |msg_text| {
            if let Some(ui) = ui_handle.upgrade() {
                let current_room = ui.global::<UiState>().get_active_room().to_string();
                let new_msg = Message {
                    user: slint::SharedString::from("Clumsy ☆"),
                    time: slint::SharedString::from("just now"),
                    text: slint::SharedString::from(msg_text),
                    repliedTo: slint::SharedString::from(""),
                    image: false,
                };

                {
                    let mut db_mut = db.borrow_mut();
                    let room_msgs = db_mut.entry(current_room.clone()).or_insert_with(Vec::new);
                    room_msgs.push(new_msg);
                }

                let msgs = db.borrow().get(&current_room).cloned().unwrap_or_default();
                ui.global::<UiState>()
                    .set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
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
