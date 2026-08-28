//! Unit suite for [`crate::commands::account`].
//!
//! The command is a thin wrapper: it takes the handler, delegates the reset, and
//! turns the outcome into a string for the caller. What is worth pinning is the
//! guard order and the shape of both answers, since the recovery machinery
//! underneath belongs to the SDK.

use super::*;
use crate::test_support::*;

#[tokio::test]
async fn refuses_without_a_session() {
    let outcome = reset_account(
        AccountResetType::KeyBackupReset,
        None,
        Some("recovery-key".to_owned()),
        empty_state(),
    )
    .await
    .expect_err("there is nobody signed in");

    assert_eq!(outcome, "No active client session");
}

#[tokio::test]
async fn checks_for_a_session_before_the_arguments() {
    // A reset with nothing filled in and nobody signed in reports the session,
    // since that is the thing the user has to fix first.
    let outcome = reset_account(AccountResetType::KeyBackupReset, None, None, empty_state())
        .await
        .expect_err("there is nobody signed in");

    assert_eq!(outcome, "No active client session");
}

#[tokio::test]
async fn a_key_backup_reset_needs_a_recovery_key() {
    let session = mock_session("account-no-key").await;

    let outcome = reset_account(
        AccountResetType::KeyBackupReset,
        None,
        None,
        session.state.clone(),
    )
    .await
    .expect_err("a key backup reset cannot proceed without the key");

    assert!(
        outcome.starts_with("Account reset failed:"),
        "unexpected error: {outcome}"
    );
    assert!(
        outcome.contains("KeyBackup reset required"),
        "unexpected error: {outcome}"
    );
}

#[tokio::test]
async fn a_failed_reset_is_reported_with_its_cause() {
    // Recovery talks to endpoints the mock server does not answer, so this
    // exercises the error path rather than a successful reset. What matters is
    // that the failure is surfaced with its reason attached rather than
    // swallowed.
    let session = mock_session("account-identity-fails").await;

    let outcome = reset_account(
        AccountResetType::IdentityReset,
        Some("hunter2".to_owned()),
        None,
        session.state.clone(),
    )
    .await
    .expect_err("the reset cannot complete against this server");

    assert!(
        outcome.starts_with("Account reset failed:"),
        "unexpected error: {outcome}"
    );
    assert!(
        outcome.len() > "Account reset failed: ".len(),
        "the error carries no cause: {outcome}"
    );
}
