//! Writing an account list into a store as raw JSON.
//!
//! Mounted as a module of `crate::storage::store`, which is what makes this
//! possible: opening and committing a snapshot are private to that module, and
//! only a module inside it can call them. Suites elsewhere in the crate reach
//! this through `pub(crate)`.
//!
//! Needed because [`EchelonStore`](super::EchelonStore)'s own API cannot produce
//! every shape a snapshot on a user's disk might hold — an account written by an
//! older build carries no homeserver, and nothing in the current API will write
//! one that way.

use super::*;

/// Replace the store's account list with `json`.
///
/// # Arguments
/// * `store` - The store to write into.
/// * `json` - The account list exactly as it should sit in the snapshot.
pub(crate) fn seed_raw_accounts(store: &EchelonStore, json: serde_json::Value) {
    let (stronghold, inner, key_provider) = store.open().expect("opening the store should succeed");
    inner
        .insert(
            b"accounts".to_vec(),
            serde_json::to_vec(&json).expect("the account list should serialize"),
            None,
        )
        .expect("writing the account list should succeed");
    store
        .commit(&stronghold, &key_provider)
        .expect("committing the snapshot should succeed");
}
