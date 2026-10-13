use ruma::api::client::device::Device;

use crate::ClientState;

pub struct SessionInfo {
    pub device: Device,
    pub is_current: bool,
    pub verified: Option<bool>,
}

pub async fn get_session_list(client_state: ClientState) -> Result<Vec<SessionInfo>, String> {
    let client = super::get_active_client(&client_state).await?;

    let user_id = client
        .user_id()
        .ok_or_else(|| "No logged in user".to_string())?;

    let current_device_id = client
        .device_id()
        .ok_or_else(|| "No current device ID".to_string())?;

    let response = client
        .devices()
        .await
        .map_err(|e| format!("Failed to fetch sessions: {e}"))?;

    let encryption = client.encryption();

    let crypto_devices = encryption
        .get_user_devices(user_id)
        .await
        .map_err(|e| format!("Failed to read device verification status: {e}"))?;

    let own_device = encryption
        .get_own_device()
        .await
        .map_err(|e| format!("Failed to read this session's verification status: {e}"))?;

    let sessions = response
        .devices
        .into_iter()
        .map(|device| {
            let is_current = device.device_id.as_str() == current_device_id.as_str();

            let verified = if is_current {
                own_device
                    .as_ref()
                    .map(|own| own.is_cross_signed_by_owner())
            } else {
                crypto_devices
                    .get(&device.device_id)
                    .map(|other| other.is_verified())
            };

            SessionInfo {
                device,
                is_current,
                verified,
            }
        })
        .collect();

    Ok(sessions)
}
