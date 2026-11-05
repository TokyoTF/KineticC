use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use ferrumc_net_codec::net_types::var_int::VarInt;

/// The clientbound **Block Event** packet.
///
/// Asks the client to run a small, hard-coded animation or sound tied to a block. Unlike
/// [`super::block_update::BlockUpdate`] it does not change the block state: the client only reacts to
/// it visually or audibly, which is how a chest lid opens and closes without the block itself
/// changing.
///
/// The fields are named after their vanilla counterparts: `action` selects the event kind, and
/// `parameter` is that event's argument. For chests the action is `1` and the parameter is `1` to
/// open or `0` to close.
#[derive(NetEncode)]
#[packet(packet_id = "block_event", state = "play")]
pub struct BlockEvent {
    pub position: NetworkPosition,
    pub action: u8,
    pub parameter: u8,
    /// The *block* registry id (not a block state id), e.g. `188` for `minecraft:chest`.
    pub block_type: VarInt,
}

impl BlockEvent {
    /// Action id vanilla uses for chest and trapped-chest lid animation.
    pub const ACTION_CHEST_LID: u8 = 1;

    /// Builds a chest lid event.
    pub fn chest(position: NetworkPosition, open: bool, block_type: i32) -> Self {
        Self {
            position,
            action: Self::ACTION_CHEST_LID,
            parameter: u8::from(open),
            block_type: VarInt::from(block_type),
        }
    }
}
