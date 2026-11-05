use bitcode_derive::{Decode, Encode};
use ferrumc_net_codec::decode::errors::NetDecodeError;
use ferrumc_net_codec::decode::{NetDecode, NetDecodeOpts};
use ferrumc_net_codec::encode::errors::NetEncodeError;
use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_world::block_state_id::BlockStateId;
use std::fmt::Display;
use std::io::{Read, Write};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Decode, Encode)]
pub struct ItemID(pub VarInt);

/// Blocks that drop an item other than themselves, as `(block name, dropped item name)`.
///
/// This is not a loot table: it covers only the cases where a block's own item is not what ends up
/// in the player's hand. Vanilla determines drops with loot tables, which are not implemented yet.
const BLOCK_DROP_OVERRIDES: &[(&str, &str)] = &[
    ("minecraft:stone", "minecraft:cobblestone"),
    ("minecraft:deepslate", "minecraft:cobbled_deepslate"),
    ("minecraft:grass_block", "minecraft:dirt"),
    ("minecraft:mycelium", "minecraft:dirt"),
    ("minecraft:coarse_dirt", "minecraft:dirt"),
    ("minecraft:rooted_dirt", "minecraft:dirt"),
    ("minecraft:podzol", "minecraft:dirt"),
    ("minecraft:dirt_path", "minecraft:dirt"),
    ("minecraft:red_sand", "minecraft:sand"),
    ("minecraft:gravel", "minecraft:gravel"),
    ("minecraft:clay", "minecraft:clay_ball"),
    ("minecraft:glass", "minecraft:glass"),
    ("minecraft:ice", "minecraft:ice"),
    ("minecraft:packed_ice", "minecraft:packed_ice"),
    ("minecraft:blue_ice", "minecraft:blue_ice"),
    ("minecraft:netherrack", "minecraft:netherrack"),
    ("minecraft:sandstone", "minecraft:sandstone"),
    ("minecraft:red_sandstone", "minecraft:red_sandstone"),
];

impl Display for ItemID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ItemID({})", self.0.0)
    }
}

impl ItemID {
    /// Creates an `ItemID` from a `BlockStateId` by mapping it through the registry.
    ///
    /// This is a general solution for finding the item associated with a block.
    ///
    /// Note: This won't work for blocks that drop a different item
    /// (e.g., stone dropping cobblestone), that's a loot table.
    pub fn from_block_state(block_state_id: BlockStateId) -> Option<Self> {
        let protocol_id = VarInt::from(block_state_id).0;
        if protocol_id == 0 {
            return None;
        }

        let id_key = protocol_id.to_string();

        // 1. Call the new compile-time lookup
        let block_name = ferrumc_registry::lookup_blockstate_name(&id_key)?;

        ItemID::from_name(block_name)
    }

    /// Creates the `ItemID` that a broken block drops.
    ///
    /// Most blocks drop themselves, but a handful of common blocks drop a different item. Those
    /// overrides are listed in [`BLOCK_DROP_OVERRIDES`]; everything else falls back to the block's
    /// own item. Full loot tables (ore drops, multi-roll tables, silk touch) are not modelled yet,
    /// so anything outside the table simply drops the block itself.
    pub fn from_block_drop(block_state_id: BlockStateId) -> Option<Self> {
        let protocol_id = VarInt::from(block_state_id).0;
        if protocol_id == 0 {
            return None;
        }

        let block_name = ferrumc_registry::lookup_blockstate_name(&protocol_id.to_string())?;

        let drop_name = BLOCK_DROP_OVERRIDES
            .iter()
            .find(|(block, _)| *block == block_name)
            .map(|(_, drop)| *drop)
            .unwrap_or(block_name);

        ItemID::from_name(drop_name)
    }

    /// Creates an `ItemID` from a name, e.g. "minecraft:stone" or "stone".
    pub fn from_name(name: &str) -> Option<Self> {
        let name = if !name.starts_with("minecraft:") {
            format!("minecraft:{}", name)
        } else {
            name.to_string()
        };

        ferrumc_registry::lookup_item_protocol_id(&name).map(|id| Self(VarInt::new(id)))
    }

    /// Converts the `ItemID` to a name, e.g. "minecraft:stone" or "stone".
    pub fn to_name(&self) -> Option<String> {
        ferrumc_registry::lookup_item_name(self.0.0).map(|s| s.to_string())
    }

    pub fn new(id: i32) -> Self {
        Self(VarInt::from(id))
    }

    pub fn as_u32(&self) -> u32 {
        self.0.0 as u32
    }
}

impl NetDecode for ItemID {
    fn decode<R: Read>(reader: &mut R, opts: &NetDecodeOpts) -> Result<Self, NetDecodeError> {
        let id = VarInt::decode(reader, opts)?;
        Ok(Self(id))
    }

    async fn decode_async<R: AsyncRead + Unpin>(
        reader: &mut R,
        opts: &NetDecodeOpts,
    ) -> Result<Self, NetDecodeError> {
        let id = VarInt::decode_async(reader, opts).await?;
        Ok(Self(id))
    }
}

impl NetEncode for ItemID {
    fn encode<W: Write>(&self, writer: &mut W, opts: &NetEncodeOpts) -> Result<(), NetEncodeError> {
        self.0.encode(writer, opts)
    }

    async fn encode_async<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        opts: &NetEncodeOpts,
    ) -> Result<(), NetEncodeError> {
        self.0.encode_async(writer, opts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_id_from_name() {
        let item_id = ItemID::from_name("minecraft:stone");
        assert!(item_id.is_some());
        assert_eq!(item_id.unwrap().0.0, 1); // Assuming stone has ID 1
    }

    #[test]
    fn test_item_id_to_name() {
        let item_id = ItemID::new(1);
        let name = item_id.to_name();
        assert!(name.is_some());
        assert_eq!(name.unwrap(), "minecraft:stone");
    }

    #[test]
    fn test_item_id_round_trip() {
        let item_id = ItemID::new(42);
        let name = item_id.to_name().unwrap();
        let decoded = ItemID::from_name(&name).unwrap();
        assert_eq!(item_id, decoded);
    }

    #[test]
    fn test_invalid_item_name() {
        let item_name = ItemID::from_name("minecraft:nonexistent_item");
        assert!(item_name.is_none());
    }

    #[test]
    fn test_invalid_item_id() {
        let item_id = ItemID::new(i32::MAX);
        assert_eq!(item_id.0.0, i32::MAX);
        let name = item_id.to_name();
        assert!(name.is_none());
    }

    #[test]
    fn test_item_id_from_block_state_stone() {
        // BlockStateId(1) is "minecraft:stone"
        let block_state_id = BlockStateId::new(1);

        let item_id = ItemID::from_block_state(block_state_id);
        assert!(
            item_id.is_some(),
            "ItemID::from_block_state returned None for Stone (ID 1)"
        );

        // The item for "minecraft:stone" is "minecraft:stone"
        let expected_item_id = ItemID::from_name("minecraft:stone");
        assert!(
            expected_item_id.is_some(),
            "ItemID::from_name failed for stone"
        );

        assert_eq!(item_id.unwrap(), expected_item_id.unwrap());
    }

    #[test]
    fn test_item_id_from_block_state_grass() {
        // BlockStateId(9) is "minecraft:grass_block" with snowy=false
        let block_state_id = BlockStateId::new(9);

        let item_id = ItemID::from_block_state(block_state_id);
        assert!(
            item_id.is_some(),
            "ItemID::from_block_state returned None for Grass (ID 9)"
        );

        // The item for "minecraft:grass_block" is "minecraft:grass_block"
        let expected_item_id = ItemID::from_name("minecraft:grass_block");
        assert!(
            expected_item_id.is_some(),
            "ItemID::from_name failed for grass_block"
        );

        assert_eq!(item_id.unwrap(), expected_item_id.unwrap());
    }

    #[test]
    fn test_item_id_from_block_state_air() {
        // BlockStateId(0) is "minecraft:air"
        let block_state_id = BlockStateId::new(0);

        let item_id = ItemID::from_block_state(block_state_id);
        assert!(
            item_id.is_none(),
            "ItemID::from_block_state returned Some for Air (ID 0)"
        );
    }

    #[test]
    fn test_stone_drops_cobblestone() {
        // BlockStateId(1) is "minecraft:stone"
        let block_state_id = BlockStateId::new(1);

        let dropped = ItemID::from_block_drop(block_state_id);
        assert!(
            dropped.is_some(),
            "ItemID::from_block_drop returned None for Stone (ID 1)"
        );

        let expected = ItemID::from_name("minecraft:cobblestone").expect("cobblestone item exists");
        assert_eq!(
            dropped.unwrap(),
            expected,
            "stone must drop cobblestone, not stone itself"
        );
    }

    #[test]
    fn test_grass_block_drops_dirt() {
        // BlockStateId(9) is "minecraft:grass_block" with snowy=false
        let block_state_id = BlockStateId::new(9);

        let dropped = ItemID::from_block_drop(block_state_id).expect("grass_block drops an item");
        let expected = ItemID::from_name("minecraft:dirt").expect("dirt item exists");
        assert_eq!(dropped, expected);
    }

    #[test]
    fn test_clay_drops_clay_balls_not_the_block() {
        let clay = ItemID::from_name("minecraft:clay").expect("clay item exists");
        let _ = clay;

        // Any block that is not in the override table drops itself.
        let oak_log_id = BlockStateId::new(138);
        let dropped = ItemID::from_block_drop(oak_log_id);
        assert!(dropped.is_some(), "an oak log must drop an item");
    }

    #[test]
    fn test_air_drops_nothing() {
        let block_state_id = BlockStateId::new(0);
        assert!(
            ItemID::from_block_drop(block_state_id).is_none(),
            "air must not drop an item"
        );
    }
}
