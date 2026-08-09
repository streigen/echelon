use matrix_sdk::Room;

#[derive(Debug, Clone)]
pub struct SpaceRoom {
    pub room: Room,
    pub children: Vec<Room>,
}
