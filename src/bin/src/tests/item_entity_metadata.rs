//! Byte-level checks for the entity metadata that carries an item stack.
//!
//! The client rejects the whole `set_entity_data` packet when a value does not have the layout
//! of its declared type, and it drops the connection when it does. `ItemEntity`'s stack is
//! `DATA_ITEM` at index 8, serialized as an `ItemStack` (metadata type 7): a VarInt count, then
//! the item's registry id as a VarInt, then the component patch. These tests pin that layout
//! down so a wrong type id, or a stray prefix from the slot, cannot slip back in.

use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// Encodes a packet the way the connection does and returns the framed bytes.
fn framed(packet: &EntityMetadataPacket) -> Vec<u8> {
    let mut bytes = Vec::new();
    packet
        .encode(&mut bytes, &NetEncodeOpts::WithLength)
        .unwrap();
    bytes
}

/// A single dirt stack, the case that produced "Invalid tag id: 28".
fn dirt_stack() -> InventorySlot {
    InventorySlot {
        count: VarInt::new(1),
        item_id: Some(ItemID(VarInt::new(28))),
        components_to_add_count: None,
        components_to_remove_count: None,
        components_to_add: None,
        components_to_remove: None,
    }
}

/// The registry id the client resolves for dirt, taken from the protocol registry rather
/// than hardcoded, so the byte assertions below cannot drift from the real data.
#[test]
fn dirt_is_the_item_the_tests_use() {
    let dirt = ItemID::from_name("minecraft:dirt").expect("dirt item exists");

    assert_eq!(
        dirt.0,
        VarInt::new(28),
        "dirt is item registry id 28; the metadata test uses that id"
    );
}

#[test]
fn the_frame_is_index_type_count_item_patch_terminator() {
    let packet = EntityMetadataPacket::new(
        VarInt::new(7),
        std::iter::once(EntityMetadata::entity_item_slot(dirt_stack())),
    );

    let bytes = framed(&packet);

    assert_eq!(
        bytes,
        [
            0x09, // length prefix for the 9 bytes of body below
            92,   //   packet id: set_entity_data
            0x07, //   entity id 7
            0x08, //   metadata index 8: ItemEntity's DATA_ITEM
            0x07, //   metadata type 7: ItemStack (Slot). 6 is Optional Component.
            0x01, //   count 1
            0x1C, //   item id 28: dirt
            0x00, //   components added: 0
            0x00, //   components removed: 0
            0xFF, // terminator
        ],
        "the item stack must be a Slot at index 8 with an empty component patch"
    );
}

#[test]
fn an_empty_stack_still_decodes() {
    // A count of zero is vanilla's empty slot and stops the value right there.
    let packet = EntityMetadataPacket::new(
        VarInt::new(9),
        std::iter::once(EntityMetadata::entity_item_slot(InventorySlot::default())),
    );
    let bytes = framed(&packet);

    assert_eq!(
        bytes,
        [0x06, 92, 0x09, 0x08, 0x07, 0x00, 0xFF],
        "an empty stack is a VarInt count of zero with nothing after it"
    );
}
