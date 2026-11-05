use crate::errors::BinaryError;
use bevy_ecs::prelude::{Commands, Entity, MessageWriter, Query, Res};
use ferrumc_components::player::abilities::PlayerAbilities;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_messages::player_digging::*;
use ferrumc_messages::BlockBrokenEvent;

use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::block_change_ack::BlockChangeAck;
use ferrumc_net::packets::outgoing::block_update::BlockUpdate;
use ferrumc_net::PlayerActionReceiver;
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::{block_state_id::BlockStateId, pos::BlockPos};
use tracing::{debug, error, warn};

/// The action of dropping the whole stack the player has selected.
const DROP_STACK: i32 = 3;
/// The action of dropping a single item of the stack the player has selected.
const DROP_ITEM: i32 = 4;

pub fn handle(
    receiver: Res<PlayerActionReceiver>,
    state: Res<GlobalStateResource>,
    mut commands: Commands,
    broadcast_query: Query<(Entity, &StreamWriter)>,
    player_query: Query<&PlayerAbilities>,
    mut players: Query<(Entity, &Hotbar, &mut Inventory, &Position, &Rotation)>,
    (mut start_dig_events, mut cancel_dig_events, mut finish_dig_events, mut block_break_events): (
        MessageWriter<PlayerStartedDigging>,
        MessageWriter<PlayerCancelledDigging>,
        MessageWriter<PlayerFinishedDigging>,
        MessageWriter<BlockBrokenEvent>,
    ),
) {
    // https://minecraft.wiki/w/Minecraft_Wiki:Projects/wiki.vg_merge/Protocol?oldid=2773393#Player_Action
    for (event, trigger_eid) in receiver.0.try_iter() {
        // Dropping the held item works the same way in every game mode.
        match event.status.0 {
            DROP_STACK => {
                drop_held_item(
                    trigger_eid,
                    true,
                    &mut commands,
                    &mut players,
                    &broadcast_query,
                );
                continue;
            }
            DROP_ITEM => {
                drop_held_item(
                    trigger_eid,
                    false,
                    &mut commands,
                    &mut players,
                    &broadcast_query,
                );
                continue;
            }
            _ => {}
        }

        // Get the player's abilities to check their gamemode
        let Ok(abilities) = player_query.get(trigger_eid) else {
            warn!(
                "PlayerAction: Player {:?} has no PlayerAbilities component",
                trigger_eid
            );
            continue;
        };

        let pos: BlockPos = event.location.clone().into();
        if abilities.creative_mode {
            // --- CREATIVE MODE LOGIC ---
            // Only instabreak (status 0) is relevant in creative.
            if event.status.0 == 0 {
                let res: Result<(), BinaryError> = try {
                    let broken_state = state
                        .0
                        .world
                        .get_block_and_fetch(pos, "overworld")
                        .unwrap_or_default();
                    {
                        let mut chunk = ferrumc_utils::world::load_or_generate_mut(
                            &state.0,
                            pos.chunk(),
                            "overworld",
                        )
                        .expect("Failed to load or generate chunk");
                        chunk.set_block(pos.chunk_block_pos(), BlockStateId::default());
                    }
                    // The chunk guard is dropped before the neighbour refresh, which loads
                    // chunks of its own.

                    // Send block broken event for un-grounding system
                    block_break_events.write(BlockBrokenEvent {
                        position: pos,
                        state: broken_state,
                    });

                    // Broadcast the removal to every connected player.
                    let block_update_packet = BlockUpdate {
                        location: event.location.clone(),
                        block_state_id: VarInt::from(BlockStateId::default()),
                    };
                    for (eid, conn) in &broadcast_query {
                        if !state.0.players.is_connected(eid) {
                            continue;
                        }
                        conn.send_packet_ref(&block_update_packet)
                            .map_err(BinaryError::Net)?;
                    }

                    // A fence or redstone wire next to the removed block may have to drop its
                    // connection, the other half of a broken door has to disappear too, and
                    // anything standing on the removed block pops and drops its loot.
                    // The refreshed states are broadcast so every client sees the new shape,
                    // exactly like the survival break path.
                    for (neighbour, block_state) in
                        crate::systems::block_updates::refresh_around(
                            &state.0,
                            "overworld",
                            pos,
                            &mut commands,
                            &broadcast_query,
                        )
                    {
                        let packet = BlockUpdate {
                            location: NetworkPosition::from(neighbour),
                            block_state_id: VarInt::from(block_state),
                        };
                        for (eid, conn) in &broadcast_query {
                            if !state.0.players.is_connected(eid) {
                                continue;
                            }
                            conn.send_packet_ref(&packet)
                                .map_err(BinaryError::Net)?;
                        }
                    }

                    // The updates must reach the client before the ack resolves its block-state
                    // prediction, so the ack is sent last.
                    if let Ok((_, conn)) = broadcast_query.get(trigger_eid) {
                        let ack_packet = BlockChangeAck {
                            sequence: event.sequence,
                        };
                        conn.send_packet_ref(&ack_packet)
                            .map_err(BinaryError::Net)?;
                    }
                };
                if res.is_err() {
                    error!("Error handling creative player action: {:?}", res);
                }
            }
        } else {
            // --- SURVIVAL MODE LOGIC ---
            // This handler's only job is to fire messages.
            match event.status.0 {
                0 => {
                    // Started digging
                    start_dig_events.write(PlayerStartedDigging {
                        player: trigger_eid,
                        position: event.location,
                        sequence: event.sequence,
                    });
                }
                1 => {
                    // Cancelled digging
                    cancel_dig_events.write(PlayerCancelledDigging {
                        player: trigger_eid,
                        position: event.location,
                        sequence: event.sequence,
                    });
                }
                2 => {
                    // Finished digging
                    finish_dig_events.write(PlayerFinishedDigging {
                        player: trigger_eid,
                        position: event.location,
                        sequence: event.sequence,
                    });
                }
                _ => {} // The remaining statuses do not apply to mining.
            }
        }
    }
}

/// Drops the item the player has selected, the way the drop key does.
///
/// The protocol has two actions for it: one drops a single item of the selected stack and the
/// other drops the whole stack. The item is taken out of the inventory and spawned in front of
/// the player, thrown in the direction they are looking at.
fn drop_held_item(
    player: Entity,
    whole_stack: bool,
    commands: &mut Commands,
    players: &mut Query<(Entity, &Hotbar, &mut Inventory, &Position, &Rotation)>,
    conn_query: &Query<(Entity, &StreamWriter)>,
) {
    let Ok((_, hotbar, mut inventory, position, rotation)) = players.get_mut(player) else {
        warn!("DropItem: could not find the inventory of player {player:?}");
        return;
    };

    let index = hotbar.get_selected_inventory_index();

    let Some(slot) = inventory.get_item(index).ok().flatten().cloned() else {
        debug!("DropItem: player {player:?} has nothing to drop");
        return;
    };

    let count = if whole_stack { slot.count.0 } else { 1 };

    if count <= 0 {
        return;
    }

    // Take the dropped items out of the inventory.
    let remaining = slot.count.0 - count;
    if remaining > 0 {
        let mut updated = slot.clone();
        updated.count = VarInt::new(remaining);

        if let Err(err) = inventory.set_item_with_update(index, updated, player) {
            warn!("DropItem: could not update the slot of player {player:?}: {err:?}");
            return;
        }
    } else if let Err(err) = inventory.clear_slot_with_update(index, player) {
        warn!("DropItem: could not clear the slot of player {player:?}: {err:?}");
        return;
    }

    // The stack is thrown in front of the player.
    let mut dropped = slot;
    dropped.count = VarInt::new(count);

    let (drop_position, velocity) =
        crate::systems::item_entities::thrown_stack_motion(position, rotation);

    crate::systems::item_entities::drop_item(
        commands,
        &drop_position,
        &velocity,
        dropped,
        ferrumc_world::dimension::Dimension::default(),
        // Thrown by the player: vanilla `LivingEntity.drop` -> `setPickUpDelay(40)`.
        ferrumc_components::item_entity::ItemEntity::THROW_PICKUP_DELAY,
        conn_query,
    );
}
