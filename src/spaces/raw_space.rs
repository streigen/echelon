use serde::{Deserialize, Serialize};
use crate::rooms::room_types::{RawRoom, SpaceRoom};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawSpace {
    #[serde(flatten)]
    pub(crate) raw_room: RawRoom,
    pub(crate) rooms: Vec<SpaceRoom>,
}
