use anyhow::Result;
use iota_stronghold::{ClientError, KeyProvider, SnapshotPath, Stronghold};

/// Shared helpers for opening/committing Stronghold stores used by both
/// per-user secrets and app-level store persistence.
pub(crate) fn open_store(
    key_provider: &KeyProvider,
    snapshot_path: &SnapshotPath,
    client_name: &str,
    create_if_missing: bool,
) -> Result<Option<(Stronghold, iota_stronghold::Store)>> {
    let stronghold = Stronghold::default();

    match stronghold.load_snapshot(key_provider, snapshot_path) {
        Ok(()) => {}
        Err(ClientError::SnapshotFileMissing(_)) if create_if_missing => {
            // Snapshot does not exist yet; it will be created on commit.
        }
        Err(ClientError::SnapshotFileMissing(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }

    let client = match stronghold.load_client(client_name) {
        Ok(c) => c,
        Err(ClientError::ClientDataNotPresent) if create_if_missing => {
            stronghold.create_client(client_name)?
        }
        Err(ClientError::ClientDataNotPresent) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    Ok(Some((stronghold, client.store())))
}

pub(crate) fn commit_store(
    stronghold: &Stronghold,
    key_provider: &KeyProvider,
    snapshot_path: &SnapshotPath,
) -> Result<()> {
    Ok(stronghold.commit_with_keyprovider(snapshot_path, key_provider)?)
}

