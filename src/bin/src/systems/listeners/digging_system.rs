use bevy_ecs::prelude::*;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_messages::PlayerExhausted;
use ferrumc_world::pos::BlockPos;
use std::time::{Duration, Instant};

use ferrumc_components::player::abilities::PlayerAbilities;
use ferrumc_components::player::gameplay_state::digging::PlayerDigging;
use ferrumc_messages::player_digging::*;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::{block_change_ack::BlockChangeAck, block_update::BlockUpdate};
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::block_state_id::BlockStateId;
use tracing::{debug, error, warn};

/// A player's connection and the digging timer they are currently running.
type DiggingPlayerQuery<'a> = (Entity, &'a StreamWriter, Option<&'a PlayerDigging>);

/// The components that decide how fast a player mines: the hotbar slot they selected and the
/// inventory it indexes into.
type HeldItemQuery<'a> = (&'a Hotbar, &'a Inventory);

/// The connections every gameplay packet is broadcast over, filtered to the players still online.
type ConnectionQuery<'a> = (Entity, &'a StreamWriter);

/// Exhaustion gained per broken block. Vanilla uses 0.005 per mined block.
const EXHAUSTION_PER_BLOCK_MINED: f32 = 0.005;

/// Converts a number of game ticks into wall-clock time.
///
/// The tick rate comes from the server configuration, so the break timers stay correct when the
/// server runs at a different tick rate than the default 20.
fn tick_duration(ticks: u64) -> Duration {
    if ticks == u64::MAX {
        return Duration::MAX;
    }
    let tps = ferrumc_config::server_config::get_global_config().tps;
    let tps = if tps == 0 { 20 } else { tps };
    Duration::from_secs_f64(ticks as f64 / f64::from(tps))
}

/// The item the player is holding, read from the hotbar slot they have selected.
fn held_item_of(hotbar: &Hotbar, inventory: &Inventory) -> Option<ItemID> {
    hotbar
        .get_selected_item(inventory)
        .ok()
        .flatten()
        .and_then(|slot| slot.item_id)
}

/// Vanilla checks `!blockstate.isAir()` before insta-mining on START_DESTROY_BLOCK.
fn is_air_block(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// Handles the PlayerStartDiggingEvent.
/// This system starts the digging timer.
pub fn handle_start_digging(
    mut commands: Commands,
    mut events: MessageReader<PlayerStartedDigging>,
    mut player_query: Query<DiggingPlayerQuery, With<PlayerAbilities>>,
    held_item_query: Query<HeldItemQuery>,
    state: Res<GlobalStateResource>,
    (conn_query, mut block_break_writer, mut exhaustion_writer): (
        Query<ConnectionQuery>,
        MessageWriter<ferrumc_messages::BlockBrokenEvent>,
        MessageWriter<PlayerExhausted>,
    ),
) {
    for event in events.read() {
        debug!(
            "Player {:?} started digging at {:?}",
            event.player, event.position
        );

        // --- 1. Get BlockStateId from the world ---
        let pos = event.position.clone().into();
        let block_state_id = match state.0.world.get_block_and_fetch(pos, "overworld") {
            Ok(id) => id,
            Err(e) => {
                warn!(
                    "StartDigging: Failed to get block at {:?}: {:?}",
                    event.position, e
                );
                continue;
            }
        };

        // --- 2. Get Block Name ---
        let Some(block_name) =
            ferrumc_registry::lookup_blockstate_name(&VarInt::from(block_state_id).0.to_string())
        else {
            warn!("Could not find block name for state {:?}", block_state_id);
            continue;
        };

        // --- 3. Get Hardness ---
        let Some(hardness) = crate::systems::mining::block_hardness(block_state_id) else {
            warn!(
                "Could not find the hardness of BlockStateId: {}",
                block_state_id
            );
            continue;
        };

        // --- 4. Check for unbreakable block ---
        if hardness < 0.0 {
            debug!(
                "Player {:?} tried to dig an unbreakable block ({})",
                event.player, block_name
            );

            // We must still send an ACK to the client.
            // But we do not add the PlayerDigging component.
            if let Ok((_, writer, _)) = player_query.get_mut(event.player) {
                let ack_packet = BlockChangeAck {
                    sequence: event.sequence,
                };
                if let Err(e) = writer.send_packet_ref(&ack_packet) {
                    error!(
                        "Failed to send start_dig ACK to {:?}: {:?}",
                        event.player, e
                    );
                }
            }
            continue; // Move to the next event
        }

        // --- 5. Calculate break time ---
        // The vanilla formula: how much of the block is mined per tick, driven by the hardness of
        // the block and the tool the player is holding.
        let held_item = held_item_query
            .get(event.player)
            .ok()
            .and_then(|(hotbar, inventory)| held_item_of(hotbar, inventory));

        let break_ticks =
            crate::systems::mining::break_time_ticks(hardness, held_item.as_ref(), block_state_id);
        let break_time = tick_duration(break_ticks);

        debug!(
            "Digging {} (hardness {}) with {:?}: {} ticks",
            block_name, hardness, held_item, break_ticks
        );

        // --- 6. Instant break ---
        // Vanilla `ServerPlayerGameMode.handleBlockBreakAction` destroys the block right away
        // when START_DESTROY_BLOCK arrives for a block whose destroy progress is already 1.0
        // (plants, torches, ...): the client sends only START for those, never STOP, so the
        // break has to happen here, on the start packet.
        if break_ticks <= 1 && !is_air_block(block_name) {
            debug!(
                "Player {:?} insta-mined {} at {:?}",
                event.player, block_name, event.position
            );

            let broken_block = break_block(
                &state,
                event.player,
                &event.position,
                &conn_query,
                &mut block_break_writer,
                &mut exhaustion_writer,
                &mut commands,
            );
            if let Some(broken_block) = broken_block {
                drop_block_loot(
                    broken_block,
                    held_item.as_ref(),
                    &event.position,
                    &mut commands,
                    &conn_query,
                );
            }

            // --- Acknowledge the client ---
            if let Ok((_, writer, _)) = player_query.get_mut(event.player) {
                let ack_packet = BlockChangeAck {
                    sequence: event.sequence,
                };
                if let Err(e) = writer.send_packet_ref(&ack_packet) {
                    error!(
                        "Failed to send insta-mine ACK to {:?}: {:?}",
                        event.player, e
                    );
                }
            }
            continue;
        }

        // --- 7. Add the component ----
        commands.entity(event.player).insert(PlayerDigging {
            block_pos: event.position.clone(),
            start_time: Instant::now(),
            break_time,
            held_item,
        });

        // --- 7. Acknowledge the client ---
        if let Ok((_, writer, _)) = player_query.get_mut(event.player) {
            let ack_packet = BlockChangeAck {
                sequence: event.sequence,
            };
            if let Err(e) = writer.send_packet_ref(&ack_packet) {
                error!(
                    "Failed to send start_dig ACK to {:?}: {:?}",
                    event.player, e
                );
            }
        }
    }
}

/// Whether an abort for `aborted` should clear the dig currently registered for the player.
///
/// Only a dig for the same block is cancelled. The client aborts the block it was mining right
/// before it starts the next one, so a blind removal would delete the freshly started dig when both
/// actions arrive in the same tick.
fn aborts_current_dig(digging: Option<&PlayerDigging>, aborted: &NetworkPosition) -> bool {
    digging.is_some_and(|dig| dig.block_pos == *aborted)
}

/// Handles the PlayerCancelDiggingEvent.
/// This system stops the digging timer.
pub fn handle_cancel_digging(
    mut commands: Commands,
    mut events: MessageReader<PlayerCancelledDigging>,
    mut player_query: Query<DiggingPlayerQuery>,
) {
    for event in events.read() {
        debug!("Player {:?} cancelled digging.", event.player);

        let Ok((_, writer, digging)) = player_query.get_mut(event.player) else {
            warn!(
                "Player {:?} sent CancelDigging but query failed.",
                event.player
            );
            continue;
        };

        // An abort names the block the client was mining. The client also sends one when it
        // switches to another block, immediately before starting that block - and the abort and the
        // new start can arrive in the same tick. Removing the component unconditionally would then
        // delete the dig that was just registered for the new target, and its later finish would be
        // dropped as "finished digging without starting". Only the dig for the aborted block may be
        // cancelled.
        if aborts_current_dig(digging, &event.position) {
            commands.entity(event.player).remove::<PlayerDigging>();
        }

        // Acknowledge the cancellation.
        let ack_packet = BlockChangeAck {
            sequence: event.sequence,
        };
        if let Err(e) = writer.send_packet_ref(&ack_packet) {
            error!(
                "Failed to send cancel_dig ACK to {:?}: {:?}",
                event.player, e
            );
        }
    }
}

/// Handles the PlayerFinishDiggingEvent.
/// This system checks the timer and breaks the block.
pub fn handle_finish_digging(
    mut commands: Commands,
    mut events: MessageReader<PlayerFinishedDigging>,
    state: Res<GlobalStateResource>,
    mut player_query: Query<DiggingPlayerQuery>,
    conn_query: Query<ConnectionQuery>,
    mut block_break_writer: MessageWriter<ferrumc_messages::BlockBrokenEvent>,
    mut exhaustion_writer: MessageWriter<PlayerExhausted>,
) {
    for event in events.read() {
        let Ok((_, writer, digging_opt)) = player_query.get_mut(event.player) else {
            warn!(
                "Player {:?} sent FinishDigging but query failed.",
                event.player
            );
            continue;
        };

        // Check if the player was actually digging
        let Some(digging) = digging_opt else {
            warn!(
                "Player {:?} finished digging without starting.",
                event.player
            );
            let ack_packet = BlockChangeAck {
                sequence: event.sequence,
            };
            if let Err(e) = writer.send_packet_ref(&ack_packet) {
                error!("Failed to send fail_dig ACK to {:?}: {:?}", event.player, e);
            }
            continue;
        };

        // --- 1. Validate the Dig ---
        if digging.block_pos != event.position {
            warn!(
                "Player {:?} finished digging the wrong block. (Expected {:?}, got {:?})",
                event.player, digging.block_pos, event.position
            );
            // Don't break the block, but still ACK
            let ack_packet = BlockChangeAck {
                sequence: event.sequence,
            };
            if let Err(e) = writer.send_packet_ref(&ack_packet) {
                error!("Failed to send fail_dig ACK to {:?}: {:?}", event.player, e);
            }
            commands.entity(event.player).remove::<PlayerDigging>();
            continue;
        }

        let elapsed = Instant::now().duration_since(digging.start_time);

        // --- 2. Check if enough time has passed ---
        // Vanilla checks `getDestroyProgress * (ticks + 1) >= 0.7F` (see
        // `ServerPlayerGameMode.handleBlockBreakAction`, STOP_DESTROY_BLOCK): the block breaks
        // when the client has done at least 70% of the expected progress. The repo's `break_time`
        // is `ceil(1/damage)` ticks, so the threshold is 70% of that, with a one-tick tolerance
        // because the client counts the break in whole ticks and the server's timer only starts
        // when the request arrives.
        //
        // The server can only judge the time when it knows what the player is holding. Without
        // that knowledge the client is trusted, because a stale inventory would otherwise make
        // every block unbreakable.
        // Only enforce the anti-cheat timer when the server actually knows the held item AND the
        // block is not an instant break: instant breaks finish in a single tick, so any network
        // latency would otherwise make the server reject a legitimate break. Blocks that do not
        // require a correct tool are now handled by the break-time calculation above, so holding a
        // block no longer makes them unbreakable.
        let knows_the_tool = digging.held_item.is_some() && digging.break_time > tick_duration(1);
        let threshold = digging.break_time.mul_f32(0.7);

        if knows_the_tool && elapsed + tick_duration(1) < threshold {
            // --- ANTI-CHEAT ---
            warn!(
                "Player {:?} finished digging too fast! ({}ms < {}ms, holding {:?})",
                event.player,
                elapsed.as_millis(),
                digging.break_time.as_millis(),
                digging.held_item
            );

            let pos = event.position.clone().into();
            let real_block_state = state
                .0
                .world
                .get_block_and_fetch(pos, "overworld")
                .unwrap_or_else(|e| {
                    error!(
                        "Failed to get real block state for anti-cheat revert: {:?}",
                        e
                    );
                    BlockStateId::default()
                });

            let revert_packet = BlockUpdate {
                location: event.position.clone(),
                block_state_id: VarInt::from(real_block_state),
            };

            if let Err(e) = writer.send_packet_ref(&revert_packet) {
                error!(
                    "Failed to send anti-cheat revert packet to {:?}: {:?}",
                    event.player, e
                );
            }
        } else {
            // --- 3. SUCCESS: Break the Block ---
            debug!(
                "Player {:?} finished digging at {:?}",
                event.player, event.position
            );

            let held_item = digging.held_item;
            let broken_block = break_block(
                &state,
                event.player,
                &event.position,
                &conn_query,
                &mut block_break_writer,
                &mut exhaustion_writer,
                &mut commands,
            );

            if let Some(broken_block) = broken_block {
                drop_block_loot(
                    broken_block,
                    held_item.as_ref(),
                    &event.position,
                    &mut commands,
                    &conn_query,
                );
            }
        }

        // --- 4. Acknowledge and Clean up (This now runs for *both* cases) ---
        let ack_packet = BlockChangeAck {
            sequence: event.sequence,
        };
        if let Err(e) = writer.send_packet_ref(&ack_packet) {
            error!(
                "Failed to send finish_dig ACK to {:?}: {:?}",
                event.player, e
            );
        }
        commands.entity(event.player).remove::<PlayerDigging>();
    }
}

/// Removes the block, fires the block-broken event and broadcasts the change to every player.
///
/// Returns the block state that was in place, so the caller can decide what it drops; a `None`
/// return means the block could not be read, in which case nothing drops.
fn break_block(
    state: &Res<GlobalStateResource>,
    miner: Entity,
    position: &ferrumc_net_codec::net_types::network_position::NetworkPosition,
    conn_query: &Query<ConnectionQuery>,
    block_break_writer: &mut MessageWriter<ferrumc_messages::BlockBrokenEvent>,
    exhaustion_writer: &mut MessageWriter<PlayerExhausted>,
    commands: &mut Commands,
) -> Option<BlockStateId> {
    let pos: BlockPos = position.clone().into();
    let broken_block = state
        .0
        .world
        .get_block_and_fetch(pos, "overworld")
        .unwrap_or_default();

    let mut chunk = ferrumc_utils::world::load_or_generate_mut(&state.0, pos.chunk(), "overworld")
        .expect("Failed to load or generate chunk");
    chunk.set_block(pos.chunk_block_pos(), BlockStateId::default());
    // Release the chunk guard before the neighbour refresh, which loads chunks of its own.
    drop(chunk);

    debug!("Sending BlockBrokenEvent for block at {:?}", pos.pos);
    block_break_writer.write(ferrumc_messages::BlockBrokenEvent {
        position: pos,
        state: broken_block,
    });

    // Mining drains energy, whatever the block drops.
    exhaustion_writer.write(PlayerExhausted {
        player: miner,
        amount: EXHAUSTION_PER_BLOCK_MINED,
    });

    // Broadcast the block break to every connected player.
    let block_update_packet = BlockUpdate {
        location: position.clone(),
        block_state_id: VarInt::from(BlockStateId::default()),
    };
    for (eid, conn) in conn_query.iter() {
        if !state.0.players.is_connected(eid) {
            continue;
        }
        if let Err(err) = conn.send_packet_ref(&block_update_packet) {
            error!("Failed to broadcast the block break to {eid:?}: {err:?}");
        }
    }

    // A fence or redstone wire next to the removed block may have to drop its connection, and
    // anything that was standing on the removed block pops and drops its loot. The refreshed
    // states are broadcast so every client sees the new shape.
    for (neighbour, block_state) in
        crate::systems::block_updates::refresh_around(&state.0, "overworld", pos, commands, conn_query)
    {
        let packet = BlockUpdate {
            location: NetworkPosition::from(neighbour),
            block_state_id: VarInt::from(block_state),
        };
        for (eid, conn) in conn_query.iter() {
            if !state.0.players.is_connected(eid) {
                continue;
            }
            if let Err(err) = conn.send_packet_ref(&packet) {
                error!("Failed to broadcast a neighbour update to {eid:?}: {err:?}");
            }
        }
    }

    Some(broken_block)
}

/// Drops what the broken block gives the player, if anything.
fn drop_block_loot(
    broken_block: BlockStateId,
    held_item: Option<&ItemID>,
    position: &ferrumc_net_codec::net_types::network_position::NetworkPosition,
    commands: &mut Commands,
    conn_query: &Query<ConnectionQuery>,
) {
    // A block can drop more than one item, for example a bush that drops both sticks and an
    // apple.
    for drop in crate::systems::drops::block_drops(broken_block, held_item) {
        drop_stack(
            InventorySlot {
                count: VarInt::new(drop.count as i32),
                item_id: Some(drop.item),
                components_to_add: None,
                components_to_remove: None,
                components_to_add_count: None,
                components_to_remove_count: None,
            },
            position,
            commands,
            conn_query,
        );
    }
}

/// Spawns an item entity for a single stack at a block position.
fn drop_stack(
    stack: InventorySlot,
    position: &ferrumc_net_codec::net_types::network_position::NetworkPosition,
    commands: &mut Commands,
    conn_query: &Query<ConnectionQuery>,
) {
    let pos: BlockPos = position.clone().into();
    let drop_position = Position::new(
        f64::from(pos.pos.x) + 0.5,
        f64::from(pos.pos.y) + 0.25,
        f64::from(pos.pos.z) + 0.5,
    );
    // A small random nudge so several drops do not stack on exactly one point.
    let drop_velocity = Velocity::new(
        (rand::random::<f64>() - 0.5) * 0.15,
        0.2,
        (rand::random::<f64>() - 0.5) * 0.15,
    );

    crate::systems::item_entities::drop_item(
        commands,
        &drop_position,
        &drop_velocity,
        stack,
        ferrumc_world::dimension::Dimension::default(),
        // Block loot: vanilla `Block.popResource` -> `setDefaultPickUpDelay()` = 10 ticks.
        ferrumc_components::item_entity::ItemEntity::PICKUP_DELAY,
        conn_query,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dig_at(pos: NetworkPosition) -> PlayerDigging {
        PlayerDigging {
            block_pos: pos,
            start_time: Instant::now(),
            break_time: Duration::from_millis(500),
            held_item: None,
        }
    }

    #[test]
    fn aborting_the_mined_block_clears_that_dig() {
        let pos = NetworkPosition::new(1, 64, 2);
        let dig = dig_at(pos.clone());

        assert!(aborts_current_dig(Some(&dig), &pos));
    }

    #[test]
    fn aborting_another_block_keeps_the_new_dig() {
        // The client aborts the old block and starts the next one in the same tick. The abort must
        // not clear the dig that was just registered for the new target.
        let old = NetworkPosition::new(1, 64, 2);
        let new = NetworkPosition::new(2, 64, 2);
        let dig = dig_at(new);

        assert!(!aborts_current_dig(Some(&dig), &old));
    }

    #[test]
    fn aborting_without_a_dig_is_a_no_op() {
        assert!(!aborts_current_dig(None, &NetworkPosition::new(0, 0, 0)));
    }
}
