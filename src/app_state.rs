use std::path::PathBuf;

use crate::storage::secret::SecretService;
use crate::storage::store::EchelonStore;

pub struct AppState {
    pub secret_service: SecretService,
    pub echelon_store: EchelonStore,
    pub data_dir: PathBuf,
}

pub fn app_data_dir(app_id: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").expect("HOME not set");
                PathBuf::from(home).join(".local/share")
            });
        return base.join(app_id);
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").expect("HOME not set");
        return PathBuf::from(home).join("Library/Application Support").join(app_id);
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").expect("APPDATA not set");
        return PathBuf::from(appdata).join(app_id);
    }
    #[cfg(target_os = "android")]
    {
        return PathBuf::from("/data/data").join(app_id);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows", target_os = "android")))]
    {
        PathBuf::from(".").join(app_id)
    }
}
