// Prevent console window in addition to Slint window in Windows release builds when, e.g., starting the app via file manager. Ignored on other platforms.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;
use slint::Model;

slint::include_modules!();

fn main() -> Result<(), Box<dyn Error>> {
    let ui = AppWindow::new()?;

    ui.on_login({
        let ui_handle = ui.as_weak();
        move |username, password, homeserver| {
            let ui = ui_handle.unwrap();
            println!("Login request: username={}, password={}, homeserver={}", username, password, homeserver);
            ui.set_loading(true);
            // Simulate a login process
            // ui.set_loading(false);
        }
    });

    ui.on_oauth_action({
        let _ui_handle = ui.as_weak();
        move |action, provider| {
            println!("OAuth request: action={}, provider={}", action, provider);
        }
    });

    ui.on_open_chat({
        let _ui_handle = ui.as_weak();
        move || {
            println!("Open chat (dev)");
        }
    });

    ui.on_forgot_password({
        let _ui_handle = ui.as_weak();
        move || {
            println!("Forgot password");
        }
    });

    let mut initial_db = std::collections::HashMap::new();
    initial_db.insert("general".to_string(), vec![
        Message { user: "Clumsy ☆".into(), time: "12:02 pm".into(), text: "hello world".into(), repliedTo: "".into(), image: false },
        Message { user: "flaxeneel2".into(), time: "12:03 pm".into(), text: "hello clumsy".into(), repliedTo: "Clumsy ☆: hello world".into(), image: false },
        Message { user: "Clumsy ☆".into(), time: "12:03 pm".into(), text: "look, cool img:".into(), repliedTo: "".into(), image: true },
        Message { user: "Clumsy ☆".into(), time: "12:05 pm".into(), text: "very nice image".into(), repliedTo: "flaxeneel2: hello clumsy".into(), image: false },
    ]);
    initial_db.insert("announcements".to_string(), vec![
        Message { user: "System".into(), time: "09:00 am".into(), text: "Welcome to echelon beta v0.1.0".into(), repliedTo: "".into(), image: false },
        Message { user: "Clumsy ☆".into(), time: "09:05 am".into(), text: "Please report any bugs to the dev team!".into(), repliedTo: "".into(), image: false },
    ]);
    initial_db.insert("mission-control".to_string(), vec![
        Message { user: "Commander".into(), time: "18:00 pm".into(), text: "Operation Nightfall commences in T-minus 10 hours.".into(), repliedTo: "".into(), image: false },
        Message { user: "Clumsy ☆".into(), time: "18:01 pm".into(), text: "Roger that.".into(), repliedTo: "Commander: Operation Nightfall commences in T-minus 10 hours.".into(), image: false },
    ]);
    initial_db.insert("intel".to_string(), vec![
        Message { user: "Agent X".into(), time: "02:00 am".into(), text: "Data secured.".into(), repliedTo: "".into(), image: false },
    ]);
    initial_db.insert("very trustworthy".to_string(), vec![
        Message { user: "Clumsy ☆".into(), time: "14:00 pm".into(), text: "This room is highly classified.".into(), repliedTo: "".into(), image: false },
    ]);
    initial_db.insert("very trustworthy x2".to_string(), vec![
        Message { user: "flaxeneel2".into(), time: "15:00 pm".into(), text: "Even more classified in here.".into(), repliedTo: "".into(), image: false },
    ]);

    let db = std::rc::Rc::new(std::cell::RefCell::new(initial_db));
    ui.set_messages(std::rc::Rc::new(slint::VecModel::from(db.borrow().get("general").unwrap().clone())).into());

    ui.on_room_switched({
        let ui_handle = ui.as_weak();
        let db = db.clone();
        move |room_name| {
            if let Some(ui) = ui_handle.upgrade() {
                let msgs = db.borrow().get(room_name.as_str()).cloned().unwrap_or_default();
                ui.set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
            }
        }
    });

    ui.on_send_message({
        let ui_handle = ui.as_weak();
        let db = db.clone();
        move |msg_text| {
            if let Some(ui) = ui_handle.upgrade() {
                let current_room = ui.get_active_room().to_string();
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
                ui.set_messages(std::rc::Rc::new(slint::VecModel::from(msgs)).into());
            }
        }
    });

    ui.run()?;

    Ok(())
}
