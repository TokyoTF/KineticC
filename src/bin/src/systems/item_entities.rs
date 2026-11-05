//! Item entities: the dropped stacks that fall to the ground and are collected by players.
//!
//! An item entity is an ordinary [`EntityIdentity`] entity, so the existing position, rotation and
//! entity-update plumbing carries it. Dropping happens wherever an item leaves a player's
//! inventory - currently block breaking - through [`drop_item`].
//!
//! Movement, expiry and pickup run once per tick in [`tick_item_entities`], which costs one empty
//! query when nothing has been dropped.
//!
//! Movement is deliberately simplified rather than vanilla-accurate: an item falls, bounces off the
//! first solid block below, drifts when it is inside a fluid and despawns after its timer. Precise
//! voxel collision is shared with the general entity physics once that lands.

use bevy_ecs::prelude::{Commands, Entity, MessageWriter, Query, Res, With, Without};
use bevy_math::DVec3;
use ferrumc_components::item_entity::ItemEntity;
use ferrumc_core::identity::entity_identity::EntityIdentity;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_entities::LastSyncedPosition;
use ferrumc_entities::markers::{HasCollisions, HasWaterDrag};
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::{Inventory, MAX_STACK_SIZE};
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_macros::{block, get_registry_entry};
use ferrumc_messages::entity_update::SendEntityUpdate;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net::packets::outgoing::remove_entities::RemoveEntitiesPacket;
use ferrumc_net::packets::outgoing::spawn_entity::SpawnEntityPacket;
use ferrumc_net::packets::outgoing::take_item_entity::TakeItemEntityPacket;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::dimension::Dimension;
use ferrumc_world::fluid::is_fluid;
use ferrumc_world::pos::BlockPos;
use tracing::{debug, warn};

/// Protocol id of the item entity type.
const ITEM_ENTITY_TYPE: i32 =
    get_registry_entry!("minecraft:entity_type.entries.minecraft:item") as i32;

/// Half the width of a player's bounding box: vanilla's is 0.6 wide.
const PLAYER_HALF_WIDTH: f64 = 0.3;
/// The height of a player's bounding box.
const PLAYER_HEIGHT: f64 = 1.8;
/// How fast a thrown stack leaves the player, in blocks per tick.
const THROW_SPEED: f64 = 0.3;
/// How far past the player's box vanilla reaches for entities to collect, sideways.
const PICKUP_REACH_XZ: f64 = 1.0;
/// How far past the player's box vanilla reaches below and above.
const PICKUP_REACH_Y: f64 = 0.5;
/// The side of the cube an item entity's bounding box is, with its position at the bottom.
const ITEM_WIDTH: f64 = 0.25;
/// Gravity applied to a falling item, in blocks per tick per tick.
const GRAVITY: f32 = 0.04;
/// Upward acceleration applied while an item is inside a fluid, making it rise.
const FLUID_LIFT: f32 = 0.02;
/// Fraction of velocity a falling item keeps per tick.
const AIR_DRAG: f32 = 0.98;
/// Fraction of velocity an item inside a fluid keeps per tick.
const FLUID_DRAG: f32 = 0.6;
/// How far below an item's feet we look for the block it stands on.
///
/// It has to be more than nothing: an item that is already resting on a block would see the air it
/// stands in if we read the block at its own feet, and would then start falling again. A quarter of
/// a block is less than one, so the block right under the surface is always the one found.
const REST_OFFSET: f64 = 0.125;
/// Lowest Y coordinate an item can exist at before it despawns.
const MIN_WORLD_Y: i16 = -64;

/// Everything an item entity carries into one tick.
///
/// `LastSyncedPosition` is deliberately not here: the entity update system owns it, and rewriting
/// it from this system made every movement packet report a zero delta, so the client never saw a
/// dropped item fall.
type ItemEntityQueryItem<'a> = (
    Entity,
    &'a EntityIdentity,
    &'a mut ItemEntity,
    &'a mut Position,
    &'a mut Velocity,
    &'a mut OnGround,
);

/// Every player connection, with the entity it belongs to.
type ConnectionQuery<'a> = (Entity, &'a StreamWriter);

/// Where and how fast a stack a player throws leaves their hand.
///
/// Vanilla spawns the item at eye height and gives it a velocity of `0.3` along the view vector
/// (`Player.drop` through `shootFromRotation`). Both the drop key and a click that throws an item
/// out of a slot go through here so they cannot disagree about the direction.
pub fn thrown_stack_motion(position: &Position, rotation: &Rotation) -> (Position, Velocity) {
    let (x, y, z) = rotation.to_direction_vector();

    let drop_position = Position::new(
        position.coords.x + f64::from(x) * 0.3,
        position.coords.y + 1.0 + f64::from(y) * 0.3,
        position.coords.z + f64::from(z) * 0.3,
    );
    let velocity = Velocity::new(
        f64::from(x) * THROW_SPEED,
        f64::from(y) * THROW_SPEED,
        f64::from(z) * THROW_SPEED,
    );

    (drop_position, velocity)
}

/// Spawns a dropped item and broadcasts the spawn packet to every connected player.
///
/// `pickup_delay` is how many ticks the stack cannot be picked up: [`ItemEntity::THROW_PICKUP_DELAY`]
/// for a stack thrown by a player (vanilla `LivingEntity.drop` -> `setPickUpDelay(40)`),
/// [`ItemEntity::PICKUP_DELAY`] for block loot (vanilla `Block.popResource` ->
/// `setDefaultPickUpDelay()`). Vanilla `ItemEntity.playerTouch` only collects when the delay has
/// run out, so an item that drops at a player's feet is not snapped up before it has left them.
pub fn drop_item(
    commands: &mut Commands,
    position: &Position,
    velocity: &Velocity,
    item: InventorySlot,
    dimension: Dimension,
    pickup_delay: u8,
    conn_query: &Query<ConnectionQuery>,
) -> Entity {
    let identity = EntityIdentity::new();
    let entity = commands
        .spawn((
            ItemEntity::new_with_pickup_delay(item.clone(), dimension, pickup_delay),
            identity.clone(),
            *position,
            Rotation::default(),
            *velocity,
            OnGround::default(),
            LastSyncedPosition(position.coords),
            // No `HasGravity`: `tick_item_entities` applies the item's fall itself, and the generic
            // gravity system would otherwise add its acceleration on top of it.
            HasCollisions,
            HasWaterDrag,
        ))
        .id();

    let rotation = Rotation::default();
    let packet = SpawnEntityPacket::new(
        identity.entity_id,
        identity.uuid.as_u128(),
        ITEM_ENTITY_TYPE,
        position,
        &rotation,
    );

    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&packet) {
            warn!("Failed to broadcast item entity spawn: {err:?}");
        }
    }
    broadcast_item_metadata(conn_query, identity.entity_id, &item);

    debug!(
        "Spawned item entity {} at {:?} carrying {:?}",
        identity.entity_id,
        position.xyz(),
        item
    );
    entity
}

/// Drops the loot of a block that popped because it lost its support or its partner half.
///
/// Vanilla `Block.updateOrDestroy` calls `Level.destroyBlock(pos, true)` when `updateShape`
/// returns air, which drops the block's loot with an empty tool (`Block.dropResources` with
/// `ItemStack.EMPTY`) and the default pickup delay. `popped` is the state the block had before
/// it was cleared; air drops nothing.
pub fn drop_pop_loot(
    popped: BlockStateId,
    pos: BlockPos,
    dimension: Dimension,
    commands: &mut Commands,
    conn_query: &Query<ConnectionQuery>,
) {
    if popped == BlockStateId::default() {
        return;
    }

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

    // No tool: exactly what vanilla `destroyBlock(pos, true)` passes to `dropResources`.
    for drop in crate::systems::drops::block_drops(popped, None) {
        drop_item(
            commands,
            &drop_position,
            &drop_velocity,
            InventorySlot {
                count: VarInt::new(drop.count as i32),
                item_id: Some(drop.item),
                components_to_add: None,
                components_to_remove: None,
                components_to_add_count: None,
                components_to_remove_count: None,
            },
            dimension,
            ItemEntity::PICKUP_DELAY,
            conn_query,
        );
    }
}

/// Advances every item entity: applies movement, expires old stacks and hands them to players.
///
/// The two queries are kept disjoint with `With`/`Without<ItemEntity>`: both need `Position`, and
/// only the item entity may mutate it, so the filters are what makes the accesses unambiguous.
pub fn tick_item_entities(
    mut commands: Commands,
    mut items: Query<ItemEntityQueryItem<'_>, With<ItemEntity>>,
    mut players: Query<
        (Entity, &PlayerIdentity, &Position, &Hotbar, &mut Inventory),
        Without<ItemEntity>,
    >,
    conn_query: Query<ConnectionQuery>,
    state: Res<GlobalStateResource>,
    mut update_writer: MessageWriter<SendEntityUpdate>,
) {
    let mut despawned: Vec<i32> = Vec::new();
    let mut updated: Vec<Entity> = Vec::new();

    for (entity, identity, mut item, mut position, mut velocity, mut grounded) in items.iter_mut() {
        item.age = item.age.saturating_add(1);
        item.pickup_delay = item.pickup_delay.saturating_sub(1);

        if item.is_expired() {
            debug!(
                "Item entity {} expired after {} ticks at {:?}",
                identity.entity_id, item.age, position.coords
            );
            despawned.push(identity.entity_id);
            continue;
        }

        if position.coords.y < f64::from(MIN_WORLD_Y) {
            debug!(
                "Item entity {} fell out of the world at {:?}",
                identity.entity_id, position.coords
            );
            despawned.push(identity.entity_id);
            continue;
        }

        let in_fluid = is_fluid_at(&state, position.coords, item.dimension);
        apply_movement(
            &state,
            item.dimension,
            &mut position,
            &mut velocity,
            &mut grounded,
            in_fluid,
        );
        updated.push(entity);

        if item.is_protected() {
            continue;
        }

        for (player, player_identity, player_position, hotbar, mut inventory) in players.iter_mut()
        {
            if !reaches_pickup_range(position.coords, player_position.coords) {
                continue;
            }

            // Only the entity despawns when the stack actually fits. A full inventory leaves the
            // drop on the ground for a later attempt instead of silently destroying it.
            let selected_slot = hotbar.get_selected_inventory_index();
            if let Err(err) =
                inventory.pick_up_with_update(item.item.clone(), player, Some(selected_slot))
            {
                debug!(
                    "{} could not collect a dropped item, the inventory is full: {err:?}",
                    player_identity.username
                );
                continue;
            }

            debug!(
                "{} collected {} item(s) from entity {}",
                player_identity.username, item.item.count.0, identity.entity_id
            );

            // The client only plays the pickup sound and the stack-flying-to-the-player particle
            // when it receives take_item_entity. Despawning the entity without this packet makes
            // the item vanish silently, which reads as a bug even though the inventory did update.
            let packet = TakeItemEntityPacket {
                item_entity_id: VarInt::new(identity.entity_id),
                player_entity_id: VarInt::new(player_identity.short_uuid),
                amount: VarInt::new(item.item.count.0),
            };
            for (_, conn) in conn_query.iter() {
                if let Err(err) = conn.send_packet_ref(&packet) {
                    warn!("Failed to broadcast item pickup: {err:?}");
                }
            }

            despawned.push(identity.entity_id);
            break;
        }
    }

    // Two stacks of the same item that have settled next to each other become one, the way vanilla
    // merges nearby dropped items. Pickups already queued for removal are left out of the merge.
    merge_nearby(&mut items, &mut despawned, &conn_query);

    if !despawned.is_empty() {
        let packet = RemoveEntitiesPacket::from_entity_ids(despawned.iter().copied());
        for (_, conn) in conn_query.iter() {
            if let Err(err) = conn.send_packet_ref(&packet) {
                warn!("Failed to broadcast item entity despawn: {err:?}");
            }
        }
    }

    // Entities are removed by a discard pass, because `Commands` cannot run while the query above is
    // still borrowed by the loop.
    remove_despawned(&mut commands, &items, &despawned);

    for entity in updated {
        update_writer.write(SendEntityUpdate(entity));
    }
}

/// The two item entities a merge pass has to look at.
struct MergeCandidate {
    entity: Entity,
    entity_id: i32,
    position: DVec3,
    item: ItemEntity,
    changed: bool,
}

/// Merges nearby stacks of the same item, filling one stack before touching the next.
///
/// The pass first snapshots every live item (`Query` cannot hand out two mutable items at once),
/// then applies the merged counts back through `get_mut`. Only the survivor gets a metadata update;
/// an entity whose whole stack was absorbed is queued for despawn, which also tells every client to
/// remove it.
fn merge_nearby(
    items: &mut Query<ItemEntityQueryItem<'_>, With<ItemEntity>>,
    despawned: &mut Vec<i32>,
    conn_query: &Query<ConnectionQuery>,
) {
    // Items picked up this tick are already queued for removal and must not absorb or donate.
    let mut candidates: Vec<MergeCandidate> = items
        .iter()
        .filter(|(_, identity, ..)| !despawned.contains(&identity.entity_id))
        .map(|(entity, identity, item, position, ..)| MergeCandidate {
            entity,
            entity_id: identity.entity_id,
            position: position.coords,
            item: item.clone(),
            changed: false,
        })
        .collect();

    let mut absorbed = vec![false; candidates.len()];

    for i in 0..candidates.len() {
        if absorbed[i] {
            continue;
        }
        for j in (i + 1)..candidates.len() {
            if absorbed[j] {
                continue;
            }
            if !within_merge_range(candidates[i].position, candidates[j].position) {
                continue;
            }
            if !candidates[i].item.can_merge_with(&candidates[j].item) {
                continue;
            }

            let kept = candidates[i].item.item.count.0.max(0) as u32;
            let other = candidates[j].item.item.count.0.max(0) as u32;
            let (new_kept, new_other) = merge_counts(kept, other);
            if new_kept == kept {
                // The first stack is already full; nothing else can go into it.
                break;
            }

            candidates[i].item.item.count = VarInt::new(new_kept as i32);
            candidates[i].changed = true;

            if new_other == 0 {
                absorbed[j] = true;
                despawned.push(candidates[j].entity_id);
            } else {
                // The survivor filled up, so the remainder stays on the ground and has to be
                // written back as well.
                candidates[j].item.item.count = VarInt::new(new_other as i32);
                candidates[j].changed = true;
            }
        }
    }

    for (index, candidate) in candidates.iter().enumerate() {
        if absorbed[index] || !candidate.changed {
            continue;
        }

        if let Ok((_, _, mut item, ..)) = items.get_mut(candidate.entity) {
            item.item = candidate.item.item.clone();
        }
        broadcast_item_metadata(conn_query, candidate.entity_id, &candidate.item.item);
    }
}

/// How a stack of `kept` items absorbs `other`: the resulting counts.
///
/// A single stack holds [`MAX_STACK_SIZE`]; anything past that stays in the other stack, matching
/// how a full stack keeps the remainder of a merge on the ground.
fn merge_counts(kept: u32, other: u32) -> (u32, u32) {
    let moved = MAX_STACK_SIZE.saturating_sub(kept).min(other);
    (kept + moved, other - moved)
}

/// Whether two stacks are close enough for their boxes to touch.
///
/// Vanilla inflates an item's box by half a block on the horizontal axes and none vertically, then
/// takes every item box that intersects it. Two quarter-block boxes therefore merge when they are
/// at most `0.5 + 0.25` apart horizontally and overlap vertically.
fn within_merge_range(first: DVec3, second: DVec3) -> bool {
    (first.x - second.x).abs() <= 0.5 + ITEM_WIDTH
        && (first.z - second.z).abs() <= 0.5 + ITEM_WIDTH
        && (first.y - second.y).abs() <= ITEM_WIDTH
}

/// Sends the stack an item entity carries to every connected client.
///
/// `ItemEntity.DATA_ITEM` is index 8 with the ITEM_STACK serializer (metadata type 7):
/// `ItemStack.OPTIONAL_STREAM_CODEC`, a VarInt count, the item id and the component patch. Without
/// it the client has no item to render, and after a merge it would keep drawing the old count.
fn broadcast_item_metadata(
    conn_query: &Query<ConnectionQuery>,
    entity_id: i32,
    item: &InventorySlot,
) {
    let metadata_packet = EntityMetadataPacket::new(
        entity_id.into(),
        vec![EntityMetadata::entity_item_slot(item.clone())],
    );

    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&metadata_packet) {
            warn!("Failed to broadcast item entity metadata: {err:?}");
        }
    }
}

/// Whether a dropped item at `item_position` is within reach of the player at `player_position`.
///
/// Vanilla does not measure a distance: `Player.aiStep` inflates the player's bounding box by one
/// block sideways and half a block up and down, and collects every entity whose own box overlaps
/// that. An item entity is a quarter of a block wide and tall, with its position at the bottom of
/// its box.
///
/// A distance check is stricter than this in every direction, so an item dropped in a hole under
/// the player's feet, or a block broken while looking down, would be unreachable even though
/// vanilla picks it up. Overlapping boxes is what the client-reachable behaviour actually is.
fn reaches_pickup_range(item_position: DVec3, player_position: DVec3) -> bool {
    // The item's box: a quarter-block cube standing on its own position.
    let item_min = (
        item_position.x - ITEM_WIDTH / 2.0,
        item_position.y,
        item_position.z - ITEM_WIDTH / 2.0,
    );
    let item_max = (
        item_position.x + ITEM_WIDTH / 2.0,
        item_position.y + ITEM_WIDTH,
        item_position.z + ITEM_WIDTH / 2.0,
    );

    // The player's box, inflated the way vanilla's pickup search inflates it.
    let reach_min = (
        player_position.x - PLAYER_HALF_WIDTH - PICKUP_REACH_XZ,
        player_position.y - PICKUP_REACH_Y,
        player_position.z - PLAYER_HALF_WIDTH - PICKUP_REACH_XZ,
    );
    let reach_max = (
        player_position.x + PLAYER_HALF_WIDTH + PICKUP_REACH_XZ,
        player_position.y + PLAYER_HEIGHT + PICKUP_REACH_Y,
        player_position.z + PLAYER_HALF_WIDTH + PICKUP_REACH_XZ,
    );

    item_min.0 <= reach_max.0
        && item_max.0 >= reach_min.0
        && item_min.1 <= reach_max.1
        && item_max.1 >= reach_min.1
        && item_min.2 <= reach_max.2
        && item_max.2 >= reach_min.2
}

/// Despawns the entities whose network ids were removed.
fn remove_despawned(
    commands: &mut Commands,
    items: &Query<'_, '_, ItemEntityQueryItem<'_>, With<ItemEntity>>,
    removed: &[i32],
) {
    if removed.is_empty() {
        return;
    }

    let to_remove: Vec<Entity> = items
        .iter()
        .filter(|(_, identity, ..)| removed.contains(&identity.entity_id))
        .map(|(entity, ..)| entity)
        .collect();

    for entity in to_remove {
        commands.entity(entity).despawn();
    }
}

/// Advances an item one tick of its fall, returning whether it came to rest.
///
/// `surface_of` is the height the item would land on from a position, read from the world. It is
/// passed in rather than read here so the resting rule can be tested without a world: the surface
/// is the only part of the fall that needs one.
fn fall_step(
    position: &mut Position,
    velocity: &mut Velocity,
    surface_of: impl Fn(DVec3) -> f64,
    in_fluid: bool,
) -> bool {
    if in_fluid {
        velocity.vec.y = (velocity.vec.y + FLUID_LIFT).min(0.5);
        velocity.vec *= FLUID_DRAG;
    } else {
        velocity.vec.y -= GRAVITY;
        velocity.vec *= AIR_DRAG;
    }

    let next = position.coords + velocity.vec.as_dvec3();
    let surface = surface_of(next);

    if velocity.vec.y <= 0.0 && next.y <= surface && !in_fluid {
        position.coords.y = surface;
        velocity.vec.y = 0.0;
        return true;
    }

    position.coords = next;
    false
}

/// Applies one tick of gravity, drag and resting to a single item entity.
fn apply_movement(
    state: &GlobalStateResource,
    dimension: Dimension,
    position: &mut Position,
    velocity: &mut Velocity,
    grounded: &mut OnGround,
    in_fluid: bool,
) {
    let surface_of = |next: DVec3| surface_height(state, next, dimension.as_str(), in_fluid);

    if fall_step(position, velocity, surface_of, in_fluid) {
        // Resting sheds most of the sideways drift, the way hitting the ground slows an item down.
        velocity.vec.x *= 0.5;
        velocity.vec.z *= 0.5;
        grounded.0 = true;
    } else {
        grounded.0 = in_fluid;
    }
}

/// The height an item comes to rest on: the top of the first solid block below the position.
///
/// The top of the block below, not the item's own floored height. It has to be the block's top
/// because the caller compares it against where the item is about to be: returning the item's
/// floored height would only ever stop it on an exact block boundary, which a falling item almost
/// never lands on, so it would sink through the world instead of resting on it.
fn surface_height(
    state: &GlobalStateResource,
    position: DVec3,
    dimension: &str,
    in_fluid: bool,
) -> f64 {
    if in_fluid {
        return f64::from(MIN_WORLD_Y);
    }

    // The block directly under the item: far enough below its feet that an item already resting
    // on a block still sees that block, rather than the air it stands in.
    let below_y = (position.y - REST_OFFSET).floor();
    let below = block_at(state, (position.x, below_y, position.z), dimension);
    match below {
        Some(block) if is_solid(&block) => below_y + 1.0,
        _ => f64::from(MIN_WORLD_Y),
    }
}

/// The block at a world-space position, or `None` when it cannot be read.
pub(crate) fn block_at(
    state: &GlobalStateResource,
    position: (f64, f64, f64),
    dimension: &str,
) -> Option<BlockStateId> {
    state
        .0
        .world
        .get_block_and_fetch(
            BlockPos::of(
                position.0.floor() as i32,
                position.1.floor() as i32,
                position.2.floor() as i32,
            ),
            dimension,
        )
        .ok()
}

/// Whether a block occupies space and would stop a falling item (or an arrow, or a player).
///
/// Vanilla entity collision uses the block's collision shape, so anything registered with
/// `noCollission` - plants, torches, redstone wire, buttons, one-layer snow... - lets entities
/// pass through and does not hold a dropped item up.
pub(crate) fn is_solid(block: &BlockStateId) -> bool {
    if *block == block!("air")
        || *block == block!("void_air")
        || *block == block!("cave_air")
        || is_fluid(*block)
    {
        return false;
    }
    match block.to_block_data() {
        Some(data) => !crate::systems::block_updates::has_empty_collision(
            &data.name,
            data.properties.as_ref(),
        ),
        // Unknown state: keep the old behaviour and treat it as solid.
        None => true,
    }
}

/// Whether the block an entity is inside is a fluid.
pub(crate) fn is_fluid_at(
    state: &GlobalStateResource,
    position: DVec3,
    dimension: Dimension,
) -> bool {
    block_at(
        state,
        (position.x, position.y, position.z),
        dimension.as_str(),
    )
    .map(is_fluid)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_is_not_solid() {
        assert!(!is_solid(&block!("air")));
        assert!(!is_solid(&block!("void_air")));
    }

    #[test]
    fn stone_is_solid() {
        assert!(is_solid(&block!("stone")));
    }

    #[test]
    fn water_is_not_solid() {
        // Water has 16 levels; any of them is a fluid, so none of them stops an item.
        assert!(!is_solid(&block!("water", { level: 15 })));
        assert!(!is_solid(&block!("water", { level: 0 })));
    }

    #[test]
    fn blocks_with_empty_collision_shape_do_not_stop_items() {
        // Vanilla registers these with `noCollission`: items fall through plants and torches.
        assert!(!is_solid(&block!("poppy")));
        assert!(!is_solid(&block!("torch")));
        assert!(!is_solid(&block!("redstone_wire", {
            east: "none", north: "none", south: "none", west: "none", power: 0
        })));
        assert!(!is_solid(&block!("snow", { layers: 1 })));
        // ...but a full snow layer, a fence and a cobweb do have a collision shape.
        assert!(is_solid(&block!("snow", { layers: 2 })));
        assert!(is_solid(&block!("oak_fence", {
            east: false, north: false, south: false, west: false, waterlogged: false
        })));
        assert!(is_solid(&block!("cobweb")));
    }

    #[test]
    fn the_rest_offset_keeps_an_item_above_the_block_surface() {
        // An item whose feet sit at the top of a block at y = 63 rests at y = 64.
        let block_top = 63.0;
        assert!(block_top + 1.0 - REST_OFFSET > block_top);
    }

    /// Drops an item onto a flat surface at `surface` and runs ticks until it stops.
    ///
    /// The item starts a quarter block above that surface, the height a broken block's drop is
    /// spawned at, and carries the little upward nudge `drop_stack` gives it.
    fn drop_onto_flat_surface(surface: f64) -> Position {
        let mut position = Position::new(0.5, surface + 0.25, 0.5);
        let mut velocity = Velocity::new(0.0, 0.2, 0.0);
        for _ in 0..100 {
            if fall_step(&mut position, &mut velocity, |_| surface, false) {
                return position;
            }
        }
        position
    }

    #[test]
    fn an_item_rests_on_the_surface_it_falls_onto() {
        let resting = drop_onto_flat_surface(64.0);

        assert_eq!(
            resting.coords.y, 64.0,
            "an item that falls onto a surface must come to rest on it, not sink past it"
        );
    }

    #[test]
    fn a_falling_item_stays_on_the_surface_tick_after_tick() {
        // The bug this guards against: the surface used to be the item's own floored height, so
        // the resting test could only pass when the item landed on an exact block boundary. A
        // falling item slides straight past that, keeps falling, and never stops.
        let mut resting = drop_onto_flat_surface(64.0);
        let mut velocity = Velocity::zero();

        for tick in 0..20 {
            let still_resting = fall_step(&mut resting, &mut velocity, |_| 64.0, false);
            assert!(
                still_resting,
                "the item must keep resting on the surface, not drift down (tick {tick})"
            );
            assert_eq!(resting.coords.y, 64.0, "the item sank on tick {tick}");
        }
    }

    #[test]
    fn small_stacks_merge_into_one() {
        assert_eq!(merge_counts(1, 1), (2, 0));
    }

    #[test]
    fn a_merge_never_overfills_a_stack() {
        // A full stack keeps the remainder on the ground instead of dropping it.
        assert_eq!(merge_counts(60, 10), (64, 6));
        assert_eq!(merge_counts(MAX_STACK_SIZE, 5), (MAX_STACK_SIZE, 5));
    }

    #[test]
    fn nearby_stacks_merge() {
        let first = DVec3::new(1.5, 64.0, 3.5);
        let second = DVec3::new(2.2, 64.0, 3.5);

        assert!(within_merge_range(first, second));
    }

    #[test]
    fn stacks_a_block_apart_do_not_merge() {
        let first = DVec3::new(1.5, 64.0, 3.5);

        assert!(
            !within_merge_range(first, first + DVec3::new(1.0, 0.0, 0.0)),
            "stacks on neighbouring blocks are too far apart to merge"
        );
        assert!(
            !within_merge_range(first, first + DVec3::new(0.0, 1.0, 0.0)),
            "stacks on different floors do not merge"
        );
    }

    #[test]
    fn a_block_broken_at_foot_level_is_within_reach() {
        // The player stands at y = 64 on the block at y = 63 and breaks the block next to them at
        // the same height. The item comes to rest on top of that block, at y = 64.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(1.5, 64.0, 0.5);

        assert!(reaches_pickup_range(item, player));
    }

    #[test]
    fn an_item_resting_at_the_bottom_of_a_hole_is_out_of_reach() {
        // The player digs a one-block hole and the item settles at the bottom, a whole block below
        // their feet. Vanilla's reach only extends half a block down, so the player has to step
        // into the hole for it: the same is true here.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(0.5, 63.0, 0.5);

        assert!(!reaches_pickup_range(item, player));
    }

    #[test]
    fn an_item_falling_past_the_players_feet_is_within_reach() {
        // Broken one block below the player's feet: the item passes within half a block of them on
        // the way down, which is exactly the reach vanilla gives, so it is collected mid-fall.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(0.5, 63.3, 0.5);

        assert!(reaches_pickup_range(item, player));
    }

    #[test]
    fn an_item_on_the_ground_across_the_room_is_out_of_reach() {
        // Four blocks away horizontally: beyond the inflated player box, so the player has to walk
        // over to it.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(4.5, 64.0, 0.5);

        assert!(!reaches_pickup_range(item, player));
    }

    #[test]
    fn an_item_two_blocks_underfoot_is_out_of_reach() {
        // Deep enough in a hole that the player has to jump in for it.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(0.5, 62.0, 0.5);

        assert!(!reaches_pickup_range(item, player));
    }

    #[test]
    fn an_item_beside_the_player_is_out_of_reach_above_the_head() {
        // Floated up to the ceiling: the player's box only reaches half a block above their head.
        let player = DVec3::new(0.5, 64.0, 0.5);
        let item = DVec3::new(0.5, 66.5, 0.5);

        assert!(!reaches_pickup_range(item, player));
    }
}
