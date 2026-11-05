use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// Tells the client that a player picked up an item entity.
///
/// This is what makes the pickup visible: on receipt the client plays
/// `SoundEvents.ITEM_PICKUP`, spawns the item-flying-to-the-player particle and removes the entity
/// once the stack is empty (see `ClientPacketListener.handleTakeItemEntity`). Removing the entity
/// without this packet makes the item disappear silently, which reads as a bug in-game even though
/// the inventory was updated correctly.
#[derive(NetEncode)]
#[packet(packet_id = "take_item_entity", state = "play")]
pub struct TakeItemEntityPacket {
    /// The network id of the item entity that was collected.
    pub item_entity_id: VarInt,
    /// The network id of the player that collected it.
    pub player_entity_id: VarInt,
    /// How many items were taken from the stack.
    pub amount: VarInt,
}
