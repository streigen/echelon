use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum AccountResetType {
    IdentityReset,
    KeyBackupReset,
}
