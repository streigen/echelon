//! Generic dispatcher for the dev-only debug console (Debug console page in the UI).
//!
//! `COMMANDS` is the single source of truth for what shows up in the console: each entry
//! names a command and its positional argument labels (max 4 — the UI renders that many
//! input slots). `dispatch` maps a command name + string args back onto the real
//! `commands::*` functions. Adding a new backend command to the console means adding one
//! `CommandSpec` here and one match arm in `dispatch` — no UI changes required.

use crate::ClientState;
use crate::account::account_reset_types::AccountResetType;

pub struct CommandSpec {
    pub name: &'static str,
    pub arg_labels: &'static [&'static str],
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "login",
        arg_labels: &["username", "password", "homeserver"],
    },
    CommandSpec {
        name: "register",
        arg_labels: &[
            "username",
            "password",
            "homeserver",
            "registration_token (optional)",
        ],
    },
    CommandSpec {
        name: "oauth_login",
        arg_labels: &["homeserver"],
    },
    CommandSpec {
        name: "oauth_register",
        arg_labels: &["homeserver"],
    },
    CommandSpec {
        name: "logout",
        arg_labels: &[],
    },
    CommandSpec {
        name: "restore_session",
        arg_labels: &["username", "homeserver"],
    },
    CommandSpec {
        name: "reset_account",
        arg_labels: &[
            "reset_type (IdentityReset|KeyBackupReset)",
            "password (optional)",
            "key_backup (optional)",
        ],
    },
    CommandSpec {
        name: "get_dm_rooms",
        arg_labels: &[],
    },
    CommandSpec {
        name: "get_space_hierarchy",
        arg_labels: &[],
    },
];

/// Run a command by name against the given positional string args.
///
/// `args` are matched positionally to each command's `arg_labels`; missing trailing args
/// are treated as empty strings, and empty strings map to `None` for `Option<String>`
/// parameters. Results that aren't already `String` are formatted for display.
pub async fn dispatch(command: &str, args: &[String], state: ClientState) -> Result<String, String> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let opt = |i: usize| {
        let v = arg(i);
        (!v.is_empty()).then_some(v)
    };

    match command {
        "login" => super::auth::login(arg(0), arg(1), arg(2), state).await,
        "register" => super::auth::register(arg(0), arg(1), arg(2), opt(3), state).await,
        "oauth_login" => super::auth::oauth_login(arg(0), state).await,
        "oauth_register" => super::auth::oauth_register(arg(0), state).await,
        "logout" => super::auth::logout(state).await,
        "restore_session" => super::auth::restore_session(arg(0), arg(1), state).await,
        "reset_account" => {
            let reset_type = match arg(0).as_str() {
                "IdentityReset" => AccountResetType::IdentityReset,
                "KeyBackupReset" => AccountResetType::KeyBackupReset,
                other => {
                    return Err(format!(
                        "unknown reset type '{other}', expected IdentityReset or KeyBackupReset"
                    ));
                }
            };
            super::account::reset_account(reset_type, opt(1), opt(2), state).await
        }
        "get_dm_rooms" => super::dm::get_dm_rooms(state).await.map(|rooms| {
            let ids: Vec<String> = rooms.iter().map(|r| r.room_id().to_string()).collect();
            format!("{} dm rooms\n{}", rooms.len(), ids.join("\n"))
        }),
        "get_space_hierarchy" => super::spaces::get_space_hierarchy(state).await.map(|spaces| {
            let entries: Vec<String> = spaces
                .iter()
                .map(|s| format!("{} ({} children)", s.room.room_id(), s.children.len()))
                .collect();
            format!("{} spaces\n{}", spaces.len(), entries.join("\n"))
        }),
        other => Err(format!("unknown command '{other}'")),
    }
}
