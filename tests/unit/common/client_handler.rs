//! Building a [`ClientHandler`] around a client the test controls.
//!
//! Mounted as a module of `crate::client`, which is what makes this possible:
//! the handler's fields are private to that module, and a descendant module can
//! see them. A fixture living at the crate root could not, and the alternative
//! would be a constructor in `src` that only tests ever call.
//!
//! [`ClientHandler::new`] is no use here — it builds its own client against a
//! fixed homeserver, so nothing it produces can be pointed at a test server.

use super::*;
use crate::app_state::AppState;
use std::sync::Arc;

/// Assemble a handler around an already-built client.
///
/// The sync manager and active-room slot start empty, matching a handler that
/// has authenticated but not yet begun syncing.
///
/// # Arguments
/// * `matrix_client` - The client the handler should own.
/// * `app_state` - The store and secret service the handler reads through.
/// * `ui_handle` - Weak handle to the window. [`slint::Weak::default`] gives one
///   that upgrades to nothing, which is what a test with no window wants.
pub(crate) fn handler_from_parts(
    matrix_client: Client,
    app_state: Arc<AppState>,
    ui_handle: slint::Weak<AppWindow>,
) -> ClientHandler {
    ClientHandler {
        matrix_client,
        sync_manager: SyncManager::new(),
        active_room: ActiveRoomSlot::default(),
        app_state,
        ui_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::keyring_client::KeyringClient;
    use crate::storage::secret::SecretService;
    use crate::storage::store::EchelonStore;

    #[tokio::test]
    async fn builds_a_handler_around_a_supplied_client() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let app_state = Arc::new(AppState {
            secret_service: SecretService::new(
                KeyringClient::new("echelon-test".to_owned()),
                dir.path().to_path_buf(),
            ),
            echelon_store: EchelonStore::new(
                KeyringClient::new("echelon-test".to_owned()),
                "echelon-test".to_owned(),
                dir.path().to_path_buf(),
            ),
            data_dir: dir.path().to_path_buf(),
        });
        // Nothing here reaches the network: pointing a client at a URL only
        // records it, and neither store touches the keyring until it is read.
        let client = Client::new(Url::parse("http://localhost:1").expect("a valid url"))
            .await
            .expect("building a client should not need a reachable server");

        let handler = handler_from_parts(client, app_state, slint::Weak::default());

        assert_eq!(
            handler.get_client().homeserver().as_str(),
            "http://localhost:1/"
        );
        // The window handle upgrades to nothing, which is what lets the command
        // suites run with no event loop.
        assert!(handler.ui_handle.upgrade().is_none());
    }

    /// The handler held by a mock session.
    ///
    /// # Arguments
    /// * `session` - The session to reach into.
    async fn handler_of(
        session: &crate::test_support::MockSession,
    ) -> tokio::sync::RwLockReadGuard<'_, Option<ClientHandler>> {
        session.state.read().await
    }

    #[tokio::test]
    async fn revoking_a_session_invalidates_it_on_the_homeserver() {
        let session = crate::test_support::mock_session("handler-revoke").await;
        session.server.mock_logout().ok().expect(1).mount().await;

        let state = handler_of(&session).await;
        state
            .as_ref()
            .expect("the session holds a handler")
            .revoke_session()
            .await
            .expect("the homeserver accepted the logout");

        session.server.verify_and_reset().await;
    }

    #[tokio::test]
    async fn reports_a_homeserver_that_refuses_the_logout() {
        // The caller carries on with a local logout regardless, but it can only
        // log why if the failure reaches it rather than being swallowed here.
        let session = crate::test_support::mock_session("handler-revoke-error").await;
        session.server.mock_logout().error500().mount().await;

        let state = handler_of(&session).await;
        state
            .as_ref()
            .expect("the session holds a handler")
            .revoke_session()
            .await
            .expect_err("the homeserver refused the logout");
    }
}
