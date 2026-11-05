use bevy_ecs::prelude::{Commands, Entity, Query, Res, ResMut};
use bevy_ecs::system::SystemParam;
use ferrumc_components::player::fall_flying::FallFlying;
use ferrumc_components::player::gamemode::{GameMode, GameModeComponent};
use ferrumc_core::collisions::bounds::CollisionBounds;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::tick::TickCounter;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_net::PlaceBlockReceiver;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::block_change_ack::BlockChangeAck;
use ferrumc_net::packets::outgoing::block_update::BlockUpdate;
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::pos::BlockPos;
use tracing::{debug, error, trace, warn};

use crate::systems::chests::{self, ChestLocation, ChestStore, ChestViewer, Facing, OpenChest};
use crate::systems::fluids::{ActiveDimension, FluidScheduler, seed_fluid_tick};

use ferrumc_config::server_config::get_global_config;
use ferrumc_core::mq;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_text::{Color, NamedColor, TextComponentBuilder};
use ferrumc_world::block_state_id::BlockStateId;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::str::FromStr;

const ITEM_TO_BLOCK_MAPPING_FILE: &str =
    include_str!("../../../../../assets/data/item_to_block_mapping.json");
static ITEM_TO_BLOCK_MAPPING: Lazy<HashMap<i32, BlockStateId>> = Lazy::new(|| {
    let str_form: HashMap<String, String> = serde_json::from_str(ITEM_TO_BLOCK_MAPPING_FILE)
        .expect("Failed to parse item_to_block_mapping.json");
    str_form
        .into_iter()
        .map(|(k, v)| {
            (
                i32::from_str(&k).unwrap(),
                BlockStateId::new(u32::from_str(&v).unwrap()),
            )
        })
        .collect()
});

/// The player components the placement handler reads, including the screen they have open.
type PlacementPlayer<'a> = (
    Entity,
    &'a StreamWriter,
    &'a mut Inventory,
    &'a Hotbar,
    &'a Position,
    Option<&'a Rotation>,
    Option<&'a OpenChest>,
    &'a PlayerIdentity,
    Option<&'a Velocity>,
    Option<&'a OnGround>,
    Option<&'a FallFlying>,
    Option<&'a GameModeComponent>,
);

/// Everything the placement handler needs besides the packet receiver.
///
/// Grouped into one parameter so the receiver stays owned by a single system: the packet channel is
/// a plain crossbeam queue, so a second system reading it would steal this system's events. Right
/// clicks on chests are part of this handler for the same reason.
#[derive(SystemParam)]
pub struct Placement<'w, 's> {
    state: Res<'w, GlobalStateResource>,
    fluid_scheduler: ResMut<'w, FluidScheduler>,
    dim: Res<'w, ActiveDimension>,
    tick: Res<'w, TickCounter>,
    chests: Res<'w, ChestStore>,
    commands: Commands<'w, 's>,
    players: Query<'w, 's, PlacementPlayer<'static>>,
    entities: Query<'w, 's, (&'static Position, &'static CollisionBounds)>,
    /// Every connection, for broadcasting spawns (a thrown pearl is visible to everyone). Kept
    /// separate from `players` so it can be iterated while `players` is mutably borrowed.
    conns: Query<'w, 's, (Entity, &'static StreamWriter)>,
}

/// Takes one unit off the stack the placed block came from, if it is still there.
///
/// The slot and the item are values the caller already read, so nothing from the query is kept
/// borrowed across the call. The read and the write are a single short mutable access for the same
/// reason: the caller iterates the query to broadcast the placement.
fn spend_placed_item(
    players: &mut Query<'_, '_, PlacementPlayer<'_>>,
    player: Entity,
    selected_slot: usize,
    item_id: ferrumc_inventories::item::ItemID,
) {
    let Ok((entity, _, mut inventory, ..)) = players.get_mut(player) else {
        return;
    };

    let Ok(Some(held)) = inventory.get_item(selected_slot) else {
        return;
    };

    if held.item_id != Some(item_id) {
        // The stack changed underneath us, so there is nothing to spend.
        return;
    }

    let remaining = held.count.0 - 1;
    if remaining <= 0 {
        // The stack is exhausted: clear the slot instead of leaving a ghost entry with a
        // count of zero. A zero-count slot still reports an item id, which made the server
        // believe the player was still holding the block and kept placing it on right-click.
        if let Err(err) = inventory.clear_slot_with_update(selected_slot, entity) {
            warn!(
                "Failed to clear the spent item for player {:?}: {:?}",
                entity, err
            );
        }
    } else {
        let mut spent = held.clone();
        spent.count = VarInt::new(remaining);

        if let Err(err) = inventory.set_item_with_update(selected_slot, spent, entity) {
            warn!(
                "Failed to spend the placed item for player {:?}: {:?}",
                entity, err
            );
        }
    }
}

pub fn handle(receiver: Res<PlaceBlockReceiver>, mut placement: Placement) {
    'ev_loop: for (event, eid) in receiver.0.try_iter() {
        // Everything the placement reads up front comes from a read-only access: the query is also
        // iterated to broadcast the result, and a long-lived mutable item would collide with that.
        // The inventory is written to in one short mutable access when the item is spent.
        let Ok((
            entity,
            conn,
            inventory,
            hotbar,
            position,
            rotation,
            open,
            _identity,
            _vel,
            _on_ground,
            _fall_flying,
            gamemode,
        )) = placement.players.get(eid)
        else {
            debug!("Could not get connection for entity {:?}", eid);
            continue;
        };

        // Creative mode never consumes the placed item: remember this up front and drop the
        // borrow before the inventory is written to below.
        let is_creative = gamemode
            .map(|gm| gm.0 == GameMode::Creative)
            .unwrap_or(false);

        if !placement.state.0.players.is_connected(entity) {
            trace!("Entity {:?} is not connected", entity);
            continue;
        }
        let clicked: BlockPos = event.position.into();
        let dimension = placement.dim.0;

        // A right click is the same packet whether the player places a block or interacts with one.
        // A chest therefore turns into a screen instead of a placement, whatever is in hand.
        if placement
            .state
            .0
            .world
            .get_block_and_fetch(clicked, dimension.as_str())
            .is_ok_and(chests::is_chest)
        {
            let listeners: Vec<(&StreamWriter, &Position)> = placement
                .players
                .iter()
                .map(|(_, conn, _, _, pos, ..)| (conn, pos))
                .collect();
            ChestViewer {
                entity,
                conn,
                inventory,
                open: open.copied(),
            }
            .open(
                &mut placement.commands,
                ChestLocation::new(clicked, dimension),
                &placement.chests,
                &placement.state,
                &listeners,
            );
            continue 'ev_loop;
        }

        // A right click on a door, fence gate or trapdoor toggles it. Vanilla runs block
        // interaction before item placement, so the held block is not placed.
        if !within_reach(position, clicked) {
            trace!("Block interaction out of reach");
            let ack = BlockChangeAck {
                sequence: event.sequence,
            };
            let _ = conn.send_packet_ref(&ack);
            continue 'ev_loop;
        }
        if let Some(updates) = crate::systems::block_updates::interact(
            &placement.state.0,
            dimension.as_str(),
            clicked,
            rotation.map(|rotation| rotation.yaw),
        ) {
            broadcast_block_updates(&placement.players, &updates);
            // The client predicts the state change and only keeps it until the matching
            // BlockChangeAck. The update above must arrive first, otherwise the ack reverts the
            // prediction to the old state (the door visibly closes later, when any other ack
            // ends it).
            let ack = BlockChangeAck {
                sequence: event.sequence,
            };
            if let Err(err) = conn.send_packet_ref(&ack) {
                error!("Failed to ack a block interaction for {eid:?}: {err:?}");
            }
            continue 'ev_loop;
        }

        match event.hand.0 {
            0 => {
                // The item id is read first and the borrow dropped, because a successful
                // placement has to spend one unit of the same stack below.
                let Some(item_id) = hotbar
                    .get_selected_item(inventory)
                    .ok()
                    .flatten()
                    .filter(|slot| slot.count.0 > 0)
                    .and_then(|slot| slot.item_id)
                else {
                    trace!("Nothing to place in hand for entity {:?}", eid);
                    // Vanilla acknowledges every `use_item_on`, even when nothing happens, so
                    // the client's pending block-state prediction is resolved.
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    let _ = conn.send_packet_ref(&ack);
                    continue 'ev_loop;
                };

                // The slot index is kept as a value so the inventory can be written to later
                // without holding a borrow of the hotbar across the call.
                let selected_slot = hotbar.get_selected_inventory_index();

                let Some(mapped_block_state_id) = ITEM_TO_BLOCK_MAPPING.get(&item_id.0.0) else {
                    // Not every item places a block: firework rockets, food, tools and projectiles
                    // only look like one. Vanilla acknowledges the click before checking whether
                    // anything can be placed, so the client's sequence is consumed either way.
                    debug!(
                        "Item {} ({:?}) does not place a block",
                        item_id.0.0, item_id
                    );
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    if let Err(err) = conn.send_packet_ref(&ack) {
                        error!("Failed to ack a non-placeable item for {eid:?}: {err:?}");
                    }

                    // Vanilla does not stop at "places nothing": the click falls through to using
                    // the item itself, which is how an ender pearl, snowball or egg is thrown and
                    // a firework rocket boosts an elytra flight. The immutable borrow from the top
                    // of the loop is dead by now, so the inventory can be written to directly.
                    if let Ok((
                        _,
                        _,
                        mut inventory,
                        hotbar,
                        position,
                        rotation,
                        _,
                        player_identity,
                        velocity,
                        on_ground,
                        fall_flying,
                        _gamemode,
                    )) = placement.players.get_mut(eid)
                    {
                        let rotation = rotation.copied().unwrap_or_default();
                        crate::systems::projectiles::use_held_item(
                            &mut placement.commands,
                            &mut inventory,
                            hotbar,
                            crate::systems::projectiles::Thrower {
                                entity: eid,
                                protocol_id: player_identity.short_uuid,
                                position,
                                rotation: &rotation,
                                velocity,
                                on_ground,
                                fall_flying: fall_flying.is_some(),
                                dimension,
                            },
                            &placement.conns,
                        );
                    }
                    continue 'ev_loop;
                };

                debug!(
                    "Placing block with item ID: {}, mapped to block state ID: {}",
                    item_id.0, mapped_block_state_id
                );
                let pos: BlockPos = clicked;
                if pos.pos.y >= 319 {
                    mq::queue(
                        TextComponentBuilder::new(
                            "Build limit is 319! Cannot place block here.".to_string(),
                        )
                        .color(Color::Named(NamedColor::Red))
                        .bold()
                        .build(),
                        true,
                        entity,
                    );
                    trace!("Block placement out of bounds: {}", pos);
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    let _ = conn.send_packet_ref(&ack);
                    continue 'ev_loop;
                } else if pos.pos.y <= -64 {
                    mq::queue(
                        TextComponentBuilder::new("Cannot place block below Y=-64.".to_string())
                            .color(Color::Named(NamedColor::Red))
                            .bold()
                            .build(),
                        true,
                        entity,
                    );
                    trace!("Block placement out of bounds: {}", pos);
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    let _ = conn.send_packet_ref(&ack);
                    continue 'ev_loop;
                }
                let offset_pos = pos
                    + match event.face.0 {
                        0 => (0, -1, 0),
                        1 => (0, 1, 0),
                        2 => (0, 0, -1),
                        3 => (0, 0, 1),
                        4 => (-1, 0, 0),
                        5 => (1, 0, 0),
                        _ => (0, 0, 0),
                    };

                // Server-side reach check: the client can target (and report) an arbitrarily far
                // block. Vanilla only allows placement within ~4.5-5.0 blocks, so reject anything
                // beyond a generous 6.0 and acknowledge the sequence so the client's prediction is
                // cleared.
                if !within_reach(position, offset_pos) {
                    trace!("Block placement out of reach");
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    if let Err(err) = conn.send_packet_ref(&ack) {
                        error!("Failed to ack out-of-reach placement for {eid:?}: {err:?}");
                    }
                    continue 'ev_loop;
                }

                // Vanilla `CollisionGetter.isUnobstructed` returns true for a block whose
                // collision shape is empty, regardless of the entities in its cell. Redstone
                // dust, torches, plants... can therefore be placed inside the player's own
                // cell - e.g. redstone on the ground under the player's feet.
                let empty_collision = mapped_block_state_id
                    .to_block_data()
                    .is_some_and(|data| {
                        crate::systems::block_updates::has_empty_collision(
                            &data.name,
                            data.properties.as_ref(),
                        )
                    });

                // Check if the block collides with any entities
                let does_collide = !empty_collision && {
                    placement.entities.iter().any(|(pos, bounds)| {
                        bounds.collides(
                            (pos.x, pos.y, pos.z),
                            &CollisionBounds {
                                x_offset_start: 0.0,
                                x_offset_end: 1.0,
                                y_offset_start: 0.0,
                                y_offset_end: 1.0,
                                z_offset_start: 0.0,
                                z_offset_end: 1.0,
                            },
                            (
                                offset_pos.pos.x as f64,
                                offset_pos.pos.y as f64,
                                offset_pos.pos.z as f64,
                            ),
                        )
                    })
                };

                // The placing player's own bounding box is not in `placement.entities`, so check it
                // explicitly: a block may not be placed where the player is standing. Blocks with
                // an empty collision shape are exempt (see above).
                let player_collides = !empty_collision && {
                    let bx = offset_pos.pos.x as f64;
                    let by = offset_pos.pos.y as f64;
                    let bz = offset_pos.pos.z as f64;
                    let ox = position.x + 0.3 > bx && position.x - 0.3 < bx + 1.0;
                    let oy = position.y + 1.8 > by && position.y < by + 1.0;
                    let oz = position.z + 0.3 > bz && position.z - 0.3 < bz + 1.0;
                    ox && oy && oz
                };

                if does_collide || player_collides {
                    trace!("Block placement collided with entity or player");
                    let ack = BlockChangeAck {
                        sequence: event.sequence,
                    };
                    let _ = conn.send_packet_ref(&ack);
                    continue 'ev_loop;
                }

                // A chest joins a matching neighbour, which rewrites both block states; any
                // other block is written as placed.
                let updates = if chests::is_chest(*mapped_block_state_id) {
                    let facing = rotation
                        .map(|rotation| Facing::from_player_yaw(rotation.yaw).opposite())
                        .unwrap_or(Facing::North);
                    chests::place_chest(&placement.state, dimension, offset_pos, facing)
                } else {
                    // Compute the blocks the placement writes, taking neighbours into account so
                    // fences connect, redstone wires join, torches pick the right orientation and
                    // a door is placed as both halves. `None` means vanilla would refuse the
                    // placement (a torch with nothing to attach to, a door without room above,
                    // ...), so acknowledge the click and place nothing.
                    let Some(placed_states) =
                        crate::systems::block_updates::compute_placement(
                            &placement.state.0,
                            dimension.as_str(),
                            crate::systems::block_updates::PlaceContext {
                                pos: offset_pos,
                                clicked,
                                face: event.face.0 as i8,
                                cursor: (event.cursor_x, event.cursor_y, event.cursor_z),
                            },
                            rotation.copied(),
                            *mapped_block_state_id,
                        )
                    else {
                        let ack = BlockChangeAck {
                            sequence: event.sequence,
                        };
                        let _ = conn.send_packet_ref(&ack);
                        continue 'ev_loop;
                    };

                    for (placed_pos, placed_state) in &placed_states {
                        let mut chunk = ferrumc_utils::world::load_or_generate_mut(
                            &placement.state.0,
                            placed_pos.chunk(),
                            dimension.as_str(),
                        )
                        .expect("Failed to load or generate chunk");
                        chunk.set_block(placed_pos.chunk_block_pos(), *placed_state);
                    }

                    // Neighbouring fences, redstone wires and fence gates have to be rewritten
                    // too, and the client only learns about them through the block updates
                    // below.
                    let mut updates = placed_states;
                    let additional: Vec<_> = updates
                        .iter()
                        .flat_map(|(placed_pos, _)| {
                            crate::systems::block_updates::refresh_around(
                                &placement.state.0,
                                dimension.as_str(),
                                *placed_pos,
                                &mut placement.commands,
                                &placement.conns,
                            )
                        })
                        .collect();
                    updates.extend(additional);
                    updates
                };

                let ack_packet = BlockChangeAck {
                    sequence: event.sequence,
                };

                // Broadcast the authoritative block state to every nearby player FIRST, then
                // acknowledge the placing client's sequence LAST. Order matters: on block
                // placement the client predicts the block locally and, when it receives the
                // BlockChangeAck, reconciles that prediction against the authoritative state it
                // currently knows. If the ack arrives before the BlockUpdate, the client sees
                // the cell as still empty, discards its prediction (the block flickers out),
                // then the BlockUpdate arrives and it reappears. Sending the update before the
                // ack keeps the client's authoritative state in sync at reconcile time, so the
                // placed block never flickers. (The digging path already sends update-then-ack.)
                broadcast_block_updates(&placement.players, &updates);

                if let Err(err) = conn.send_packet_ref(&ack_packet) {
                    error!("Failed to send block change ack packet: {:?}", err);
                    continue 'ev_loop;
                }

                // Placing a block spends one unit of the stack it came from. Skipping this
                // leaves the server with a stack the client has already counted down, so the
                // server keeps believing the player is holding a block that is gone on the
                // client - which is what decided the tool used to judge every later dig.
                //
                // This runs after the broadcast above, which needs the query immutably, so the
                // write takes a short mutable access of its own here.
                // Creative mode never consumes the placed item, so only spend in survival (and
                // other non-creative) gamemodes.
                if !is_creative {
                    spend_placed_item(&mut placement.players, eid, selected_slot, item_id);
                }

                // Seed fluid simulation. If the placed block is itself a fluid it will begin
                // to spread; in all cases, neighbouring fluids may need to react to the new
                // block (e.g. flow around or be blocked by it). The chunk borrow above is
                // released before seeding because seeding loads chunks itself.
                let current_tick = placement.tick.get();
                let scheduler = &mut placement.fluid_scheduler.0;
                seed_fluid_tick(
                    scheduler,
                    &placement.state.0,
                    ActiveDimension(dimension),
                    current_tick,
                    offset_pos,
                );
                for neighbour in [
                    offset_pos + (0, 1, 0),
                    offset_pos + (0, -1, 0),
                    offset_pos + (1, 0, 0),
                    offset_pos + (-1, 0, 0),
                    offset_pos + (0, 0, 1),
                    offset_pos + (0, 0, -1),
                ] {
                    seed_fluid_tick(
                        scheduler,
                        &placement.state.0,
                        ActiveDimension(dimension),
                        current_tick,
                        neighbour,
                    );
                }
            }
            1 => {
                trace!("Offhand block placement not implemented");
            }
            _ => {
                debug!("Invalid hand");
            }
        }
    }
}

/// Vanilla allows placement and interaction within ~4.5-5.0 blocks of the player's eyes. A
/// generous 6.0 keeps laggy clients working while still rejecting clicks on arbitrarily far
/// blocks, which the client can report.
fn within_reach(position: &Position, pos: BlockPos) -> bool {
    let dx = (pos.pos.x as f64 + 0.5) - position.x;
    let dy = (pos.pos.y as f64 + 0.5) - position.y;
    let dz = (pos.pos.z as f64 + 0.5) - position.z;
    (dx * dx + dy * dy + dz * dz).sqrt() <= 6.0
}

/// Sends a set of block updates to every player whose view distance covers them.
///
/// Vanilla sends the authoritative state before the `BlockChangeAck`, because the client reverts
/// unconfirmed block-state predictions when the ack ends them. Skipping the update would make
/// fences, doors and redstone wires snap back to their old state.
fn broadcast_block_updates(
    players: &Query<'_, '_, PlacementPlayer<'_>>,
    updates: &[(BlockPos, BlockStateId)],
) {
    let render_distance = get_global_config().chunk_render_distance as i32;
    for (updated_pos, updated_state) in updates {
        let chunk_packet = BlockUpdate {
            location: NetworkPosition::from(*updated_pos),
            block_state_id: VarInt::from(*updated_state),
        };
        let updated_chunk = updated_pos.chunk();
        for (_, conn, _, _, player_pos, ..) in players.iter() {
            let player_chunk = player_pos.chunk();

            // Only send the block update if the player is within the render distance of the
            // block being updated.
            if (updated_chunk.x() - player_chunk.x).abs() <= render_distance
                && (updated_chunk.z() - player_chunk.y).abs() <= render_distance
            {
                if let Err(err) = conn.send_packet_ref(&chunk_packet) {
                    error!("Failed to send block update packet: {:?}", err);
                }
            }
        }
    }
}

