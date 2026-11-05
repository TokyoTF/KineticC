use crate::errors::WorldError;
use crate::World;
use tracing::trace;

/// The table that stores block entity payloads.
const BLOCK_ENTITY_TABLE: &str = "block_entities";

/// The table that stores the hotbar slot every player has selected.
const PLAYER_HOTBAR_TABLE: &str = "player_hotbar";

/// Packs a block position and its dimension into the key of a block entity entry.
///
/// The key is a plain `u128` because that is what the storage layer supports, so the position is
/// packed by hand: x and z in the low 64 bits, y in the middle, and the dimension id on top, which
/// keeps two dimensions from colliding on the same coordinates.
pub fn block_entity_key(dimension: &str, x: i32, y: i32, z: i32) -> u128 {
    let x_bits = u128::from(x as u32) & 0xFFFF_FFFF;
    let z_bits = u128::from(z as u32) << 32;
    let y_bits = (u128::from(y as u16)) << 64;
    let dimension_bits = u128::from(dimension_id(dimension)) << 80;
    x_bits | z_bits | y_bits | dimension_bits
}

/// A small id per dimension, so a position key can tell dimensions apart.
///
/// Unknown dimensions get an id derived from their name, which is stable for the lifetime of the
/// server and therefore good enough for a lookup key.
fn dimension_id(dimension: &str) -> u8 {
    match dimension {
        "overworld" => 0,
        "the_nether" | "nether" => 1,
        "the_end" | "end" => 2,
        other => {
            let mut hash = 3u8;
            for byte in other.as_bytes() {
                hash = hash.wrapping_mul(31).wrapping_add(*byte);
            }
            hash
        }
    }
}

impl World {
    /// Loads player data from the storage backend and decodes it.
    ///
    /// # Type Parameters
    ///
    /// * `T` - A type that implements the `bitcode::DecodeOwned` trait, representing the structure of the decoded data.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The unique identifier of the player whose data is to be loaded.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(T))` - The decoded player data if it exists and can be successfully decoded.
    /// * `Ok(None)` - If no data is found for the given player.
    /// * `Err(WorldError)` - If an error occurs during the operation or decoding fails.
    pub fn load_player_data<T: bitcode::DecodeOwned>(
        &self,
        uuid: uuid::Uuid,
    ) -> Result<Option<T>, WorldError> {
        if !self
            .storage_backend
            .table_exists("player_data".to_string())?
        {
            trace!(
                "Player data table does not exist. Returning None for player {}",
                uuid
            );
            return Ok(None);
        }
        let data = self
            .storage_backend
            .get("player_data".to_string(), uuid.as_u128())
            .map_err(WorldError::DatabaseError);
        data.map(|opt_bytes| opt_bytes.and_then(|bytes| bitcode::decode(&bytes).ok()))
    }

    /// Saves player data to the storage backend after encoding it.
    ///
    /// # Type Parameters
    ///
    /// * `T` - A type that implements the `bitcode::Encode` trait, representing the structure of the data to be encoded.
    ///
    /// # Arguments
    ///
    /// * `uuid` - The unique identifier of the player whose data is to be saved.
    /// * `data` - A reference to the data to be encoded and saved.
    ///
    /// # Returns
    ///
    /// * `Ok(true)` - If the data was successfully saved.
    /// * `Ok(false)` - If the data could not be saved.
    /// * `Err(WorldError)` - If an error occurs during the operation.
    pub fn save_player_data<T: bitcode::Encode>(
        &self,
        uuid: uuid::Uuid,
        data: &T,
    ) -> Result<bool, WorldError> {
        if !self
            .storage_backend
            .table_exists("player_data".to_string())?
        {
            self.storage_backend
                .create_table("player_data".to_string())
                .map_err(WorldError::DatabaseError)?;
        }
        self.storage_backend
            .upsert(
                "player_data".to_string(),
                uuid.as_u128(),
                bitcode::encode(data),
            )
            .map_err(WorldError::DatabaseError)
    }

    /// Saves the hotbar slot a player has selected.
    ///
    /// The slot is kept apart from the rest of the player data so that it survives a change of the
    /// data format, and so that the server knows what the player is holding right after the join:
    /// the client only reports the slot when the player changes it.
    pub fn save_player_hotbar(&self, uuid: uuid::Uuid, slot: u8) -> Result<bool, WorldError> {
        if !self
            .storage_backend
            .table_exists(PLAYER_HOTBAR_TABLE.to_string())?
        {
            self.storage_backend
                .create_table(PLAYER_HOTBAR_TABLE.to_string())
                .map_err(WorldError::DatabaseError)?;
        }
        self.storage_backend
            .upsert(
                PLAYER_HOTBAR_TABLE.to_string(),
                uuid.as_u128(),
                bitcode::encode(&slot),
            )
            .map_err(WorldError::DatabaseError)
    }

    /// Loads the hotbar slot a player has selected, if it is known.
    pub fn load_player_hotbar(&self, uuid: uuid::Uuid) -> Result<Option<u8>, WorldError> {
        if !self
            .storage_backend
            .table_exists(PLAYER_HOTBAR_TABLE.to_string())?
        {
            return Ok(None);
        }
        let data = self
            .storage_backend
            .get(PLAYER_HOTBAR_TABLE.to_string(), uuid.as_u128())
            .map_err(WorldError::DatabaseError)?;

        Ok(data.and_then(|bytes| bitcode::decode::<u8>(&bytes).ok()))
    }
}

impl World {
    /// Stores the payload of a block entity, overwriting whatever it held before.
    ///
    /// The payload is opaque to the world layer: the block entities hold inventories, which live in
    /// a crate the world cannot depend on. Encoding and decoding therefore belongs to the caller.
    pub fn set_block_entity_data(
        &self,
        dimension: &str,
        x: i32,
        y: i32,
        z: i32,
        data: &[u8],
    ) -> Result<bool, WorldError> {
        if !self
            .storage_backend
            .table_exists(BLOCK_ENTITY_TABLE.to_string())?
        {
            self.storage_backend
                .create_table(BLOCK_ENTITY_TABLE.to_string())
                .map_err(WorldError::DatabaseError)?;
        }

        let key = crate::player::block_entity_key(dimension, x, y, z);
        self.storage_backend
            .upsert(BLOCK_ENTITY_TABLE.to_string(), key, data.to_vec())
            .map_err(WorldError::DatabaseError)
    }

    /// Reads the payload of a block entity, if one is stored at that position.
    pub fn get_block_entity_data(
        &self,
        dimension: &str,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<Option<Vec<u8>>, WorldError> {
        if !self
            .storage_backend
            .table_exists(BLOCK_ENTITY_TABLE.to_string())?
        {
            return Ok(None);
        }

        let key = crate::player::block_entity_key(dimension, x, y, z);
        self.storage_backend
            .get(BLOCK_ENTITY_TABLE.to_string(), key)
            .map_err(WorldError::DatabaseError)
    }

    /// Deletes a block entity stored at that position.
    pub fn remove_block_entity_data(
        &self,
        dimension: &str,
        x: i32,
        y: i32,
        z: i32,
    ) -> Result<(), WorldError> {
        if !self
            .storage_backend
            .table_exists(BLOCK_ENTITY_TABLE.to_string())?
        {
            return Ok(());
        }

        let key = crate::player::block_entity_key(dimension, x, y, z);
        self.storage_backend
            .delete(BLOCK_ENTITY_TABLE.to_string(), key)
            .map_err(WorldError::DatabaseError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_positions_get_different_keys() {
        assert_ne!(
            block_entity_key("overworld", 0, 0, 0),
            block_entity_key("overworld", 1, 0, 0)
        );
        assert_ne!(
            block_entity_key("overworld", 0, 0, 0),
            block_entity_key("overworld", 0, 1, 0)
        );
        assert_ne!(
            block_entity_key("overworld", 0, 0, 0),
            block_entity_key("overworld", 0, 0, 1)
        );
    }

    #[test]
    fn dimensions_do_not_share_keys() {
        let overworld = block_entity_key("overworld", 10, 20, 30);
        let nether = block_entity_key("the_nether", 10, 20, 30);
        assert_ne!(overworld, nether);
    }

    #[test]
    fn the_same_position_gives_the_same_key() {
        assert_eq!(
            block_entity_key("overworld", -5, 64, 12),
            block_entity_key("overworld", -5, 64, 12)
        );
    }

    #[test]
    fn negative_coordinates_are_distinct_from_positive_ones() {
        assert_ne!(
            block_entity_key("overworld", -1, 0, 0),
            block_entity_key("overworld", 1, 0, 0)
        );
    }
}
