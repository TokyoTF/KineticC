use ferrumc_inventories::slot::InventorySlot;
use ferrumc_macros::{NetEncode, packet};
use ferrumc_net_codec::net_types::length_prefixed_vec::LengthPrefixedVec;
use ferrumc_net_codec::net_types::var_int::VarInt;

#[derive(NetEncode)]
#[packet(packet_id = "container_set_content", state = "play")]
pub struct SetContainerContent {
    pub window_id: VarInt,
    pub state_id: VarInt,
    pub slots: LengthPrefixedVec<InventorySlot>,
    pub carried_item: InventorySlot,
}

impl SetContainerContent {
    /// Builds the packet from a list of slot contents.
    pub fn new(window_id: i32, state_id: i32, slots: Vec<InventorySlot>) -> Self {
        Self {
            window_id: VarInt::new(window_id),
            state_id: VarInt::new(state_id),
            slots: LengthPrefixedVec::new(slots),
            carried_item: InventorySlot::empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrumc_inventories::item::ItemID;
    use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};

    /// Byte-exact regression test for the join-time inventory packet.
    ///
    /// Vanilla 1.21.8 layout (`ClientboundContainerSetContentPacket`): packet id 18 (0x12),
    /// container id VarInt, state id VarInt, item count VarInt, then one slot per item
    /// (count VarInt; if count <= 0 nothing more, else item id VarInt + component patch
    /// counts), then the carried item. Encoded with `WithLength`, the whole frame is
    /// prefixed with its length as a VarInt.
    ///
    /// Slot 37 mimics a saved `firework_rocket` stack whose stored component patch holds only
    /// a type id (`minecraft:fireworks` = 60) and no value. That must still be sent as an empty
    /// patch, or the client fails to decode the packet and disconnects with
    /// "Failed to decode packet 'clientbound/minecraft:container_set_content'".
    #[test]
    fn encodes_byte_exact_like_vanilla_1_21_8() {
        let mut slots = vec![InventorySlot::empty(); 46];
        // A plain stack: 3 x item 800 (VarInt 0xa0 0x06).
        slots[36] = InventorySlot {
            count: VarInt::new(3),
            item_id: Some(ItemID::new(800)),
            ..Default::default()
        };
        // 64 x firework_rocket (item 1192) with an unserializable stored component patch.
        slots[37] = InventorySlot {
            count: VarInt::new(64),
            item_id: Some(ItemID::new(1192)),
            components_to_add_count: Some(VarInt::new(1)),
            components_to_remove_count: Some(VarInt::new(0)),
            components_to_add: Some(vec![VarInt::new(60)]),
            components_to_remove: Some(vec![]),
        };

        let packet = SetContainerContent::new(0, 0, slots);
        let mut buf = Vec::new();
        packet.encode(&mut buf, &NetEncodeOpts::WithLength).unwrap();

        let mut expected: Vec<u8> = Vec::new();
        expected.push(0x3b); // frame length = 59
        expected.extend([0x12, 0x00, 0x00, 0x2e]); // id 18, window 0, state 0, 46 items
        expected.extend([0x00; 36]); // slots 0..=35: empty
        expected.extend([0x03, 0xa0, 0x06, 0x00, 0x00]); // slot 36: 3 x item 800, empty patch
        expected.extend([0x40, 0xa8, 0x09, 0x00, 0x00]); // slot 37: 64 x item 1192, patch empty
        expected.extend([0x00; 8]); // slots 38..=45: empty
        expected.push(0x00); // carried item: empty

        assert_eq!(buf, expected);
    }
}
