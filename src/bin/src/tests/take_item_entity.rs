//! Byte-level check for the packet that announces an item pickup.
//!
//! The pickup is invisible without it: the client derives both the `ITEM_PICKUP` sound and the
//! stack-flying-to-the-player particle from `ClientboundTakeItemEntityPacket`, and removes the item
//! entity itself once the stack is empty. This test pins the field order down so a mismatch cannot
//! silently turn every pickup back into a disappearing item.

use ferrumc_net::packets::outgoing::take_item_entity::TakeItemEntityPacket;
use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// Encodes a packet the way the connection does and returns the framed bytes.
fn framed(packet: &TakeItemEntityPacket) -> Vec<u8> {
    let mut bytes = Vec::new();
    packet
        .encode(&mut bytes, &NetEncodeOpts::WithLength)
        .unwrap();
    bytes
}

#[test]
fn the_frame_is_item_then_player_then_amount() {
    let packet = TakeItemEntityPacket {
        item_entity_id: VarInt::new(7),
        player_entity_id: VarInt::new(42),
        amount: VarInt::new(1),
    };

    let bytes = framed(&packet);

    assert_eq!(
        bytes,
        [
            0x04, // length prefix for the 4 bytes of body below
            0x75, //   packet id 117: take_item_entity
            0x07, //   item entity id 7
            0x2A, //   player entity id 42
            0x01, //   amount 1
        ],
        "the pickup packet must be item id, player id, amount, all VarInts"
    );
}

#[test]
fn the_amount_is_the_stack_size_that_was_collected() {
    // A full stack still fits in one VarInt byte, but the value has to reach the client: it is what
    // the client shrinks the entity's stack by.
    let packet = TakeItemEntityPacket {
        item_entity_id: VarInt::new(1_000_000),
        player_entity_id: VarInt::new(42),
        amount: VarInt::new(64),
    };

    let bytes = framed(&packet);

    assert_eq!(
        bytes,
        [
            0x06, // length prefix
            0x75, // packet id 117
            0xC0, 0x84, 0x3D, // item entity id 1_000_000
            0x2A, // player entity id 42
            0x40, // amount 64
        ],
        "the item entity id is a network id, not a slot"
    );
}
