//! Places the player in the world once the client is ready for the play state.
//!
//! The client reports that it has finished loading with the "Player Loaded" packet. That is the
//! moment to tell it where the player is: the position the player was last at, so the world is
//! picked up where it was left. The world spawn is not used for that, it would put the player in
//! the air instead of where they were.

use bevy_ecs::prelude::{Entity, Query, Res};
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_macros::block;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::set_container_content::SetContainerContent;
use ferrumc_net::packets::outgoing::set_held_slot::SetHeldItem;
use ferrumc_net::packets::outgoing::synchronize_player_position::SynchronizePlayerPositionPacket;
use ferrumc_net::PlayerLoadedReceiver;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::pos::BlockPos;
use tracing::{info, warn};

/// How many blocks the player is lifted when they end up inside the terrain.
const MAX_STUCK_LIFT: i32 = 4;
/// The window id of the player's own inventory.
const PLAYER_WINDOW: i32 = 0;

pub fn handle(
    ev: Res<PlayerLoadedReceiver>,
    state: Res<GlobalStateResource>,
    mut query: Query<(Entity, &mut Position, &Rotation, &StreamWriter)>,
    inventory_query: Query<(&Inventory, &Hotbar)>,
) {
    for (_, player) in ev.0.try_iter() {
        let Ok((entity, mut player_pos, player_rot, conn)) = query.get_mut(player) else {
            warn!("Player position not found in query.");
            continue;
        };
        if !state.0.players.is_connected(entity) {
            warn!(
                "Player {} is not connected, skipping position synchronization.",
                player
            );
            continue;
        }

        // The server is authoritative about the inventory, so the contents of the player's own
        // window are pushed here. The client adopts them, which is what keeps both sides agreeing
        // on which item the player holds - the item that decides how fast a block breaks.
        //
        // The selected hotbar slot is part of the same state: the client only reports it when the
        // selection changes, so without this the server would still be reading whatever slot the
        // player was on when they logged out.
        if let Ok((inventory, hotbar)) = inventory_query.get(entity) {
            let slots: Vec<InventorySlot> = inventory
                .slots
                .iter()
                .map(|slot| slot.clone().unwrap_or_default())
                .collect();

            let contents = SetContainerContent::new(PLAYER_WINDOW, 0, slots);
            if let Err(e) = conn.send_packet_ref(&contents) {
                warn!("Failed to send the inventory to player {}: {:?}", player, e);
            }

            let held = SetHeldItem {
                slot: hotbar.selected_slot,
            };
            if let Err(e) = conn.send_packet_ref(&held) {
                warn!(
                    "Failed to send the held item slot to player {}: {:?}",
                    player, e
                );
            }
        }

        // A player that logged out inside the terrain is lifted until there is room again, so they
        // do not get stuck in the ground.
        let position = lift_out_of_terrain(&state, *player_pos);
        if position.y != player_pos.y {
            *player_pos = position;
        }

        info!(
            "Player {} loaded at position: ({}, {}, {})",
            player, position.x, position.y, position.z
        );

        let packet = SynchronizePlayerPositionPacket::from_position_rotation(
            &position,
            player_rot,
            VarInt::new(0),
        );
        if let Err(e) = conn.send_packet_ref(&packet) {
            tracing::error!(
                "Failed to send synchronize player position packet for player {}: {:?}",
                player,
                e
            );
        }
    }
}

/// Moves the position up until the player has room for its body again.
///
/// The blocks are read from the world, so a position in a chunk that was never generated is left
/// alone: it is checked again when the chunk arrives.
fn lift_out_of_terrain(state: &Res<GlobalStateResource>, position: Position) -> Position {
    let mut position = position;

    for _ in 0..MAX_STUCK_LIFT {
        let feet = BlockPos::of(position.x as i32, position.y as i32, position.z as i32);
        let head = BlockPos::of(position.x as i32, position.y as i32 + 1, position.z as i32);

        let feet_block = state.0.world.get_block_and_fetch(feet, "overworld");
        let head_block = state.0.world.get_block_and_fetch(head, "overworld");

        let (Ok(feet_block), Ok(head_block)) = (feet_block, head_block) else {
            // The chunk is not there yet, so there is nothing to check against.
            return position;
        };

        if feet_block == block!("air") && head_block == block!("air") {
            return position;
        }

        position.y += 1.0;
    }

    position
}
