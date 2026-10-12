use ruma::api::client::device::Device;

use crate::ClientState;

pub async fn get_session_list(client_state: ClientState) -> Result<Vec<Device>, String> {
    let client = super::get_active_client(&client_state).await?;

    let client_devices = client
        .devices()
        .await
        .map_err(|e| format!("Failed to fetch sessions: {e}"))?;

    Ok(client_devices.devices)
}
