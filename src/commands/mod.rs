use crate::ClientState;
use crate::client::ClientHandler;

pub mod account;
pub mod auth;
pub mod dm;
pub mod rooms;
pub mod spaces;

#[deprecated(
    note = "loan architecture doesnt meld well with async closures. please no more lifetime hell"
)]
pub(crate) async fn with_active_client<T, F>(state: ClientState, f: F) -> Result<T, String>
where
    F: FnOnce(&ClientHandler) -> T,
{
    let state_r = state.read().await;
    let Some(client_handler) = state_r.as_ref() else {
        return Err("No active client session".to_string());
    };
    Ok(f(client_handler))
}
