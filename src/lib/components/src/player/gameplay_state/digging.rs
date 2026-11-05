use bevy_ecs::prelude::Component;
use ferrumc_inventories::item::ItemID;
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use std::time::{Duration, Instant};

/// An "action component" added to a player when they start digging.
#[derive(Component, Debug, Clone)]
pub struct PlayerDigging {
    pub block_pos: NetworkPosition,
    pub start_time: Instant,
    pub break_time: Duration,
    /// The item the player was holding when they started digging, recorded so the finish handler
    /// does not have to re-read the hotbar: switching tools mid-dig must not change the outcome.
    pub held_item: Option<ItemID>,
}
