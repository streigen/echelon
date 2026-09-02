use std::path::PathBuf;

use crate::storage::secret::SecretService;
use crate::storage::store::EchelonStore;

pub struct AppState {
    pub secret_service: SecretService,
    pub echelon_store: EchelonStore,
    pub data_dir: PathBuf,
}

/// Get the app data directory for the current target OS.
pub fn app_data_dir(app_id: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        linux_data_dir(
            std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
            app_id,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").expect("HOME not set");
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(app_id)
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").expect("APPDATA not set");
        PathBuf::from(appdata).join(app_id)
    }
    #[cfg(target_os = "android")]
    {
        PathBuf::from("/data/data").join(app_id)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows",
        target_os = "android"
    )))]
    {
        PathBuf::from(".").join(app_id)
    }
}

/// Where a Linux desktop keeps this application's data.
///
/// `$XDG_DATA_HOME` wins when the session sets it; otherwise the spec's default
/// of `$HOME/.local/share` applies.
///
/// Split from [`app_data_dir`] so that precedence is reachable without arranging
/// the environment: `std::env::set_var` is unsound to call while other test
/// threads are running, so a test cannot set either variable for itself.
///
/// # Arguments
/// * `xdg_data_home` - `$XDG_DATA_HOME`, if set.
/// * `home` - `$HOME`, read only when `xdg_data_home` is unset.
/// * `app_id` - The application's reverse-DNS id.
#[cfg(target_os = "linux")]
fn linux_data_dir(xdg_data_home: Option<PathBuf>, home: Option<PathBuf>, app_id: &str) -> PathBuf {
    xdg_data_home
        .unwrap_or_else(|| home.expect("HOME not set").join(".local/share"))
        .join(app_id)
}

#[cfg(test)]
#[path = "../tests/unit/app_state.rs"]
mod tests;
