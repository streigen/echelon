//! Command dispatcher for the developer debug console in the UI.

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
        arg_labels: &["user_id (@user:server)", "homeserver (optional)"],
    },
    CommandSpec {
        name: "list_accounts",
        arg_labels: &[],
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
    CommandSpec {
        name: "get_messages",
        arg_labels: &[
            "room_id",
            "from (optional pagination token)",
            "limit (default 50)",
        ],
    },
];

/// Run a debug command by name against positional string arguments.
pub async fn dispatch(
    command: &str,
    args: &[String],
    state: ClientState,
) -> Result<String, String> {
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
        "restore_session" => super::auth::restore_session(arg(0), opt(1), state).await,
        "list_accounts" => super::auth::list_accounts(state).await,
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
        "get_space_hierarchy" => super::spaces::get_space_hierarchy(state)
            .await
            .map(|spaces| {
                let entries: Vec<String> = spaces
                    .iter()
                    .map(|s| format!("{} ({} children)", s.room.room_id(), s.children.len()))
                    .collect();
                format!("{} spaces\n{}", spaces.len(), entries.join("\n"))
            }),
        "get_messages" => {
            let room_id = ruma::RoomId::parse(arg(0))
                .map_err(|e| format!("invalid room_id '{}': {e}", arg(0)))?;
            let limit = opt(2).and_then(|v| v.parse().ok()).unwrap_or(50);
            super::messages::get_messages_from_room_paginated(state, room_id, opt(1), limit)
                .await
                .map(|page| {
                    use crate::rooms::messages::EventEffect;
                    // Every effect is printed, edits and redactions included, so a
                    // page that folds down to few rows shows why.
                    let lines: Vec<String> = page
                        .effects
                        .iter()
                        .map(|effect| match effect {
                            EventEffect::New(m) => {
                                format!("[{}] {}: {}", m.origin_server_ts, m.sender, m.body)
                            }
                            EventEffect::Edit {
                                target, new_body, ..
                            } => {
                                format!("[edit of {target}] {new_body}")
                            }
                            EventEffect::Redact { target } => format!("[redacts {target}]"),
                            EventEffect::Ignore => "[ignored]".to_string(),
                        })
                        .collect();
                    format!(
                        "{} effects (next_token: {:?})\n{}",
                        lines.len(),
                        page.next_token,
                        lines.join("\n")
                    )
                })
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/commands/debug.rs"]
mod tests;
