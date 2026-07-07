use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawRoom {
    pub id: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub avatar_url: Option<String>,
    pub is_space: bool
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceRoom {
    #[serde(flatten)]
    pub base: RawRoom,
    pub parent_spaces: Vec<String>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DmRoom {
    #[serde(flatten)]
    pub base: RawRoom,
    pub members: Vec<String>
}
