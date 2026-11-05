use bevy_ecs::prelude::Message;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::pos::BlockPos;

/// Message sent when a block is broken in the world.
///
/// Carries the state the block had before it was removed, so listeners can tell what was destroyed
/// (a chest, a log, a supported block) without racing the write that clears the position.
#[derive(Message)]
pub struct BlockBrokenEvent {
    pub position: BlockPos,
    pub state: BlockStateId,
}
