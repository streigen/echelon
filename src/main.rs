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

    ui.on_send_message({
        let ui_handle = ui.as_weak();
        move |msg_text| {
            if let Some(ui) = ui_handle.upgrade() {
                let current_messages = ui.get_messages();
                let mut msgs: Vec<_> = current_messages.iter().collect();
                
                msgs.push(Message {
                    user: slint::SharedString::from("Clumsy ☆"),
                    time: slint::SharedString::from("just now"),
                    text: slint::SharedString::from(msg_text),
                    repliedTo: slint::SharedString::from(""),
                    image: false,
                });
                
                let new_model = std::rc::Rc::new(slint::VecModel::from(msgs));
                ui.set_messages(new_model.into());
            }
        }
    });

    ui.run()?;

    Ok(())
}
