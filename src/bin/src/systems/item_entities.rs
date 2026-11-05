//! Item entities: the dropped stacks that fall to the ground and are collected by players.
//!
//! An item entity is an ordinary [`EntityIdentity`] entity, so the existing position, rotation and
//! entity-update plumbing carries it. Dropping happens wherever an item leaves a player's
//! inventory - currently block breaking - through [`drop_item`].
//!
//! Movement, expiry and pickup run once per tick in [`tick_item_entities`], which costs one empty
//! query when nothing has been dropped.
//!
//! Movement follows vanilla `ItemEntity.tick`: gravity in air, buoyancy and drag in a fluid (chosen
//! by a latched box scan, the way `Entity.updateFluidHeightAndDoFluidPushing` works), collision
//! against solid blocks and a rest state on the first surface below. The client runs the same tick,
//! so the server integrates it identically instead of fighting it with snap corrections.

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
use ferrumc_net::packets::outgoing::set_entity_motion::SetEntityMotionPacket;
use ferrumc_net::packets::outgoing::spawn_entity::SpawnEntityPacket;
use ferrumc_net::packets::outgoing::take_item_entity::TakeItemEntityPacket;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::dimension::Dimension;
use ferrumc_world::fluid::{fluid_state, is_fluid, FluidKind};
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
/// The standing eye height of a player, in blocks (vanilla's standing-pose `getEyeHeight`).
const PLAYER_EYE_HEIGHT: f64 = 1.62;
/// How far below the eyes a player-thrown stack appears: vanilla `createItemStackToDrop` spawns it at `getEyeY() - 0.3`.
const DROP_BELOW_EYE: f64 = 0.3;
/// The upward bias of a player-thrown stack, in blocks per tick: vanilla adds `0.1F` to its y velocity.
const THROW_UPWARD_BIAS: f64 = 0.1;
/// How far past the player's box vanilla reaches for entities to collect, sideways.
const PICKUP_REACH_XZ: f64 = 1.0;
/// How far past the player's box vanilla reaches below and above.
const PICKUP_REACH_Y: f64 = 0.5;
/// The side of the cube an item entity's bounding box is, with its position at the bottom.
const ITEM_WIDTH: f64 = 0.25;
/// Gravity applied to a falling item, in blocks per tick per tick (vanilla `getDefaultGravity`).
const GRAVITY: f32 = 0.04;
/// Fraction of velocity an item keeps per tick after the move (vanilla drags by 0.98 on every
/// axis, in air and in fluid alike).
const AIR_DRAG: f32 = 0.98;
/// Horizontal velocity multiplier while submerged in water: vanilla `setUnderwaterMovement` ->
/// `setFluidMovement(0.99F)`.
const WATER_DRAG: f32 = 0.99;
/// Horizontal velocity multiplier while submerged in lava: vanilla `setUnderLavaMovement` ->
/// `setFluidMovement(0.95F)`.
const LAVA_DRAG: f32 = 0.95;
/// Buoyancy added to the vertical velocity each tick while in a fluid, as long as the item is not
/// already rising faster than vanilla's limit (`setFluidMovement`: `y < 0.06 ? +5.0E-4 : 0`).
const FLUID_BUOYANCY: f32 = 0.0005;
/// Rise speed above which the buoyancy stops applying (vanilla `vec3.y < 0.06F`).
const FLUID_RISE_LIMIT: f32 = 0.06;
/// Minimum fluid depth inside the item's box for the underwater branch to apply
/// (vanilla `isInWater() && getFluidHeight(WATER) > 0.1`).
const FLUID_BRANCH_MIN: f64 = 0.1;
/// How strongly a water current pushes an item per tick (vanilla motionScale 0.014).
const WATER_PUSH: f64 = 0.014;
/// Lava current push in the Nether (ultraWarm dimension, vanilla 0.007) and everywhere else
/// (vanilla 0.0023333333333333335).
const LAVA_PUSH_NETHER: f64 = 0.007;
const LAVA_PUSH: f64 = 0.002_333_333_333_333_333_5;
/// The fluid scan deflates the item's box by this much (vanilla `getBoundingBox().deflate(0.001)`).
const FLUID_SCAN_DEFLATE: f64 = 0.001;
/// A nearly-still item still gets at least this much push from a current (vanilla's 0.0045
/// fallback); the stillness test uses 0.003 per horizontal axis.
const FLOW_MIN_PUSH: f64 = 0.0045;
const FLOW_STILL_EPS: f64 = 0.003;
/// Vanilla `Vec3.normalize` treats vectors shorter than this as zero.
const NORMALIZE_EPS: f64 = 1.0e-4;
/// Vanilla's water surface slope constant in the flow calculation (0.8888889F).
const FLOW_SLOPE_CONST: f64 = 0.888_888_9;
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
/// This is vanilla `Player.drop` -> `LivingEntity.createItemStackToDrop` with `randomizeMotion =
/// false`: the stack appears at the player's own X/Z and `0.3` below eye height - no offset along
/// the view vector, so it never spawns inside a block the player happens to face - and leaves with
/// a velocity of `0.3` along the view vector plus a `0.1` upward bias and a small random kick.
/// Both the drop key and a click that throws an item out of a slot go through here so they cannot
/// disagree about the direction.
pub fn thrown_stack_motion(position: &Position, rotation: &Rotation) -> (Position, Velocity) {
    let (x, y, z) = rotation.to_direction_vector();

    let drop_position = Position::new(
        position.coords.x,
        position.coords.y + PLAYER_EYE_HEIGHT - DROP_BELOW_EYE,
        position.coords.z,
    );

    let angle = rand::random::<f64>() * std::f64::consts::TAU;
    let kick = 0.02 * rand::random::<f64>();
    let velocity = Velocity::new(
        f64::from(x) * THROW_SPEED + angle.cos() * kick,
        f64::from(y) * THROW_SPEED
            + THROW_UPWARD_BIAS
            + (rand::random::<f64>() - rand::random::<f64>()) * 0.1,
        f64::from(z) * THROW_SPEED + angle.sin() * kick,
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
    // The spawn packet carries the initial velocity (vanilla `ClientboundAddEntityPacket` does the
    // same): the client integrates the item's own fall from that velocity, so a zeroed one makes
    // the client-side stack lag behind the server and jitter while in the air.
    let packet = SpawnEntityPacket::item(
        identity.entity_id,
        identity.uuid.as_u128(),
        ITEM_ENTITY_TYPE,
        position,
        &rotation,
        velocity,
    );

    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&packet) {
            warn!("Failed to broadcast item entity spawn: {err:?}");
        }
    }
    broadcast_item_metadata(conn_query, identity.entity_id, &item);

    // Vanilla `ServerEntity.sendPairingData` also sends the motion packet when the entity starts
    // being tracked, so the client's velocity matches from the very first tick.
    let motion_packet = SetEntityMotionPacket::new(identity.entity_id, velocity);
    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&motion_packet) {
            warn!("Failed to broadcast item entity motion: {err:?}");
        }
    }

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
    // (entity, network id): the id is needed at write-out to skip entities removed this tick.
    let mut updated: Vec<(Entity, i32)> = Vec::new();

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

        let dimension = item.dimension;
        // Vanilla scans the fluid state at the start of the tick (`Entity.baseTick` ->
        // `updateInWaterStateAndDoFluidPushing`) and the `ItemEntity.tick` branch reads it, so the
        // scan runs before the movement and decides whether this tick has gravity or buoyancy.
        // The same scan also pushes the item along a current.
        let medium = update_fluid_state(&state, dimension, &position, &mut velocity);
        apply_movement(
            &state,
            dimension,
            &mut position,
            &mut velocity,
            &mut grounded,
            medium,
        );
        // `ItemEntity.tick` runs the fluid update a second time after moving, which is what
        // carries the current's push into the next tick's move.
        update_fluid_state(&state, dimension, &position, &mut velocity);
        updated.push((entity, identity.entity_id));

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

    for (entity, entity_id) in updated {
        // An entity removed this tick (picked up, merged into another stack, expired) is already
        // gone from the clients' view thanks to RemoveEntities; a position update for it would
        // only warn about a dead entity when the update system reads the message next tick.
        if despawned.contains(&entity_id) {
            continue;
        }
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
/// `surface_of` is the height the item would land on between two positions, read from the world.
/// It is passed in rather than read here so the resting rule can be tested without a world: the
/// surface is the only part of the fall that needs one. `fluid` is the medium the latched fluid
/// scan found the box in, the way vanilla `ItemEntity.tick` branches on `isInWater()`/`isInLava()`.
fn fall_step(
    position: &mut Position,
    velocity: &mut Velocity,
    surface_of: impl Fn(DVec3, DVec3) -> f64,
    fluid: Option<FluidKind>,
) -> bool {
    // Vanilla's order (`ItemEntity.tick`): the fluid branch or gravity, then the move, then the
    // drag. The client runs the very same tick on the item, so moving before dragging keeps both
    // simulations on the same trajectory and the server's position updates only confirm what the
    // client integrated, instead of fighting it with a different integration order.
    match fluid {
        Some(FluidKind::Water) => fluid_movement(velocity, WATER_DRAG),
        Some(FluidKind::Lava) => fluid_movement(velocity, LAVA_DRAG),
        None => velocity.vec.y -= GRAVITY,
    }

    let next = position.coords + velocity.vec.as_dvec3();
    // Swept check: the highest solid surface between where the item is and where it is heading.
    // At terminal velocity a step is more than one block tall, so checking only the block under
    // the landing point would let the item tunnel through thin floors and then pop back up.
    let surface = surface_of(position.coords, next);

    if velocity.vec.y <= 0.0 && next.y <= surface {
        // Land on the surface but keep the horizontal motion: vanilla items slide to a stop.
        // This also applies under water, where the scan passes through the fluid and finds the
        // solid floor, exactly like vanilla's `move` colliding with it.
        position.coords = DVec3::new(next.x, surface, next.z);
        velocity.vec.y = 0.0;
        return true;
    }

    position.coords = next;
    // Vanilla drags by 0.98 on every axis after the move, in air and in fluid alike (on the
    // ground x/z would use the block's friction, but a landed item is handled by the rest rule).
    velocity.vec *= AIR_DRAG;
    false
}

/// Vanilla `ItemEntity.setFluidMovement`: damp the horizontal axes and add buoyancy while below
/// the rise limit - and, unlike `applyGravity`, apply no gravity at all.
fn fluid_movement(velocity: &mut Velocity, multiplier: f32) {
    velocity.vec.x *= multiplier;
    velocity.vec.z *= multiplier;
    if velocity.vec.y < FLUID_RISE_LIMIT {
        velocity.vec.y += FLUID_BUOYANCY;
    }
}

/// Swept horizontal movement (X, then Z) with collision against solid blocks, the way vanilla
/// `Entity.move` collides every axis: an axis whose box would cross a block is clamped to that
/// block's face and its velocity is zeroed, while the other axis keeps sliding along the wall.
///
/// The sweep probes every block column the box passes through, not just the destination cell, so
/// a fast or diagonal throw cannot skip over a one-block-thick wall.
fn slide_step(position: &mut Position, velocity: &mut Velocity, solid_at: &impl Fn(DVec3) -> bool) {
    const HALF: f64 = ITEM_WIDTH / 2.0;
    // A box edge exactly on a block boundary only touches the neighbour, it does not overlap it.
    const EPS: f64 = 1e-9;

    let from = position.coords;
    // Block rows the box overlaps on Y, the axis that does not move in this pass.
    let y0 = from.y.floor() as i32;
    let y1 = (from.y + ITEM_WIDTH - EPS).floor() as i32;

    // --- X axis: the cross ranges are Y and Z at the current position ---
    let z0 = (from.z - HALF).floor() as i32;
    let z1 = (from.z + HALF - EPS).floor() as i32;

    let step_x = f64::from(velocity.vec.x);
    if step_x != 0.0 {
        let target = from.x + step_x;
        let lo = from.x.min(target) - HALF;
        let hi = from.x.max(target) + HALF;
        let mut hit_face: Option<f64> = None;
        let mut bx = lo.floor() as i32;
        let bx_end = (hi - EPS).floor() as i32;
        while bx <= bx_end {
            for by in y0..=y1 {
                for bz in z0..=z1 {
                    if solid_at(DVec3::new(bx as f64 + 0.5, by as f64 + 0.5, bz as f64 + 0.5)) {
                        // Clamp to the nearest face in the direction of motion.
                        let face = if step_x > 0.0 {
                            bx as f64 - HALF
                        } else {
                            bx as f64 + 1.0 + HALF
                        };
                        hit_face = Some(match hit_face {
                            Some(f) => {
                                if step_x > 0.0 { f.min(face) } else { f.max(face) }
                            }
                            None => face,
                        });
                    }
                }
            }
            bx += 1;
        }
        if let Some(face) = hit_face {
            position.coords.x = face;
            velocity.vec.x = 0.0;
        } else {
            position.coords.x = target;
        }
    }

    // --- Z axis: the cross ranges are Y and the new X ---
    let x0 = (position.coords.x - HALF).floor() as i32;
    let x1 = (position.coords.x + HALF - EPS).floor() as i32;

    let step_z = f64::from(velocity.vec.z);
    if step_z != 0.0 {
        let target = from.z + step_z;
        let lo = from.z.min(target) - HALF;
        let hi = from.z.max(target) + HALF;
        let mut hit_face: Option<f64> = None;
        let mut bz = lo.floor() as i32;
        let bz_end = (hi - EPS).floor() as i32;
        while bz <= bz_end {
            for by in y0..=y1 {
                for bx in x0..=x1 {
                    if solid_at(DVec3::new(bx as f64 + 0.5, by as f64 + 0.5, bz as f64 + 0.5)) {
                        let face = if step_z > 0.0 {
                            bz as f64 - HALF
                        } else {
                            bz as f64 + 1.0 + HALF
                        };
                        hit_face = Some(match hit_face {
                            Some(f) => {
                                if step_z > 0.0 { f.min(face) } else { f.max(face) }
                            }
                            None => face,
                        });
                    }
                }
            }
            bz += 1;
        }
        if let Some(face) = hit_face {
            position.coords.z = face;
            velocity.vec.z = 0.0;
        } else {
            position.coords.z = target;
        }
    }
}

/// Applies one tick of gravity (or buoyancy), horizontal collision, drag and resting to a single
/// item entity. `fluid` is the medium the latched fluid scan found the item in.
fn apply_movement(
    state: &GlobalStateResource,
    dimension: Dimension,
    position: &mut Position,
    velocity: &mut Velocity,
    grounded: &mut OnGround,
    fluid: Option<FluidKind>,
) {
    let dimension_str = dimension.as_str();
    let solid_at = |point: DVec3| {
        block_at(state, (point.x, point.y, point.z), dimension_str)
            .is_some_and(|block| is_solid(&block))
    };

    // Vanilla `Entity.move` collides on every axis; the horizontal ones are swept here so a
    // diagonal throw is stopped by a wall instead of tunneling through it.
    slide_step(position, velocity, &solid_at);

    let surface_of = |from: DVec3, to: DVec3| surface_height(state, from, to, dimension_str);

    if fall_step(position, velocity, surface_of, fluid) {
        // Resting sheds most of the sideways drift, the way hitting the ground slows an item down.
        velocity.vec.x *= 0.5;
        velocity.vec.z *= 0.5;
        grounded.0 = true;
    } else {
        // Vanilla `onGround` only comes from colliding with a solid block, never from a fluid.
        grounded.0 = false;
    }
}

/// The height an item comes to rest on: the top of the highest solid block in the column between
/// `from` and `to`, so a fast fall is stopped by the first surface it crosses instead of sinking
/// past it. Fluids are not solid, so the scan passes through water to the floor underneath, the
/// way vanilla's `move` collides with the floor but not with the fluid.
///
/// The top of the block, not the item's own floored height. It has to be the block's top because
/// the caller compares it against where the item is about to be: returning the item's floored
/// height would only ever stop it on an exact block boundary, which a falling item almost never
/// lands on, so it would sink through the world instead of resting on it.
fn surface_height(
    state: &GlobalStateResource,
    from: DVec3,
    to: DVec3,
    dimension: &str,
) -> f64 {
    // Scan from just under the item's feet (far enough down that an item already resting on a
    // block still sees that block, rather than the air it stands in) down to the landing cell.
    let top_y = (from.y - REST_OFFSET).floor();
    let bottom_y = (to.y - REST_OFFSET).floor();
    let mut y = top_y;
    while y >= bottom_y {
        if let Some(block) = block_at(state, (to.x, y, to.z), dimension) {
            if is_solid(&block) {
                return y + 1.0;
            }
        }
        y -= 1.0;
    }
    f64::from(MIN_WORLD_Y)
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

/// One cell of the fluid scan: a fluid block inside the item's box, with the height and the flow
/// vanilla would compute for it.
struct FluidCell {
    kind: FluidKind,
    /// Vanilla `FluidState.getHeight`: 1.0 when the same fluid sits directly above, else the
    /// fluid's own height (`amount / 9`).
    height: f64,
    /// Vanilla `FluidState.getFlow`: the current's direction at this cell (unit vector, or zero
    /// in still fluid).
    flow: DVec3,
}

/// The medium vanilla `ItemEntity.tick` branches on: water when the box is more than 0.1 deep in
/// it, else lava, else air (gravity). The 0.1 threshold is what makes an item bobbing at the
/// surface switch smoothly between the two branches instead of flip-flopping every tick.
fn branch_medium(touching_water: bool, water_height: f64, lava_height: f64) -> Option<FluidKind> {
    if touching_water && water_height > FLUID_BRANCH_MIN {
        Some(FluidKind::Water)
    } else if lava_height > FLUID_BRANCH_MIN {
        Some(FluidKind::Lava)
    } else {
        None
    }
}

/// Vanilla `Entity.updateInWaterStateAndDoFluidPushing` for one item: scans the box (deflated by
/// 0.001) for water and lava, records how deep the box is in each, and adds the currents' push to
/// the velocity. Returns the medium the movement branch has to use.
///
/// Vanilla runs this at the start of the tick (`Entity.baseTick`) and again at the end
/// (`ItemEntity.tick`), so the caller runs it twice as well.
fn update_fluid_state(
    state: &GlobalStateResource,
    dimension: Dimension,
    position: &Position,
    velocity: &mut Velocity,
) -> Option<FluidKind> {
    let dimension_str = dimension.as_str();
    let half = ITEM_WIDTH / 2.0;
    let deflate = FLUID_SCAN_DEFLATE;
    let min = DVec3::new(
        position.coords.x - half + deflate,
        position.coords.y + deflate,
        position.coords.z - half + deflate,
    );
    let max = DVec3::new(
        position.coords.x + half - deflate,
        position.coords.y + ITEM_WIDTH - deflate,
        position.coords.z + half - deflate,
    );
    let scan_box = ScanBox {
        min_y: min.y,
        x0: min.x.floor() as i32,
        x1: max.x.ceil() as i32,
        y0: min.y.floor() as i32,
        y1: max.y.ceil() as i32,
        z0: min.z.floor() as i32,
        z1: max.z.ceil() as i32,
    };

    let cell_at = |pos: (i32, i32, i32)| fluid_cell_at(state, dimension_str, pos);

    let (touching_water, water_height) =
        scan_fluid_kind(&scan_box, FluidKind::Water, WATER_PUSH, velocity, &cell_at);
    // Vanilla picks the lava motion scale from the dimension's `ultraWarm` flag.
    let lava_push = if dimension == Dimension::Nether {
        LAVA_PUSH_NETHER
    } else {
        LAVA_PUSH
    };
    let (_, lava_height) = scan_fluid_kind(&scan_box, FluidKind::Lava, lava_push, velocity, &cell_at);

    branch_medium(touching_water, water_height, lava_height)
}

/// The deflated item box as block-cell ranges, the way vanilla iterates an `AABB` in
/// `updateFluidHeightAndDoFluidPushing` (`floor(min)` .. `ceil(max)` on each axis).
struct ScanBox {
    min_y: f64,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    z0: i32,
    z1: i32,
}

/// Vanilla `Entity.updateFluidHeightAndDoFluidPushing(tag, motionScale)`: one pass over the box
/// for a single fluid kind. Returns `(any cell touched, max depth of the box in that fluid)`.
///
/// The cell lookup is a closure so the scan can be tested without a world.
fn scan_fluid_kind(
    scan_box: &ScanBox,
    kind: FluidKind,
    motion_scale: f64,
    velocity: &mut Velocity,
    cell_at: &impl Fn((i32, i32, i32)) -> Option<FluidCell>,
) -> (bool, f64) {
    let mut depth = 0.0;
    let mut touching = false;
    let mut flow_sum = DVec3::ZERO;
    let mut cells = 0;

    for x in scan_box.x0..scan_box.x1 {
        for y in scan_box.y0..scan_box.y1 {
            for z in scan_box.z0..scan_box.z1 {
                let Some(cell) = cell_at((x, y, z)) else {
                    continue;
                };
                if cell.kind != kind {
                    continue;
                }
                let surface = y as f64 + cell.height;
                if surface >= scan_box.min_y {
                    touching = true;
                    depth = (surface - scan_box.min_y).max(depth);
                    // `isPushedByFluid()` is true for items, so the current pushes; shallower
                    // than 0.4, the push scales down with the depth.
                    let mut flow = cell.flow;
                    if depth < 0.4 {
                        flow *= depth;
                    }
                    flow_sum += flow;
                    cells += 1;
                }
            }
        }
    }

    if flow_sum.length() > 0.0 {
        // `cells` is at least one here: a non-zero sum needs at least one contribution.
        let mut push = flow_sum / cells as f64;
        // Items are not players, so vanilla always normalizes the summed flow.
        push = normalize(push);
        push *= motion_scale;
        let vel = velocity.vec.as_dvec3();
        if vel.x.abs() < FLOW_STILL_EPS
            && vel.z.abs() < FLOW_STILL_EPS
            && push.length() < FLOW_MIN_PUSH
        {
            push = normalize(push) * FLOW_MIN_PUSH;
        }
        velocity.vec += push.as_vec3a();
    }

    (touching, depth)
}

/// Reads one cell of the world as a fluid: its kind, its height (vanilla `FluidState.getHeight`)
/// and its flow vector (vanilla `FluidState.getFlow`).
fn fluid_cell_at(
    state: &GlobalStateResource,
    dimension: &str,
    (x, y, z): (i32, i32, i32),
) -> Option<FluidCell> {
    let block = block_at(state, (x as f64, y as f64, z as f64), dimension)?;
    let fluid = fluid_state(block)?;
    let own = own_fluid_height(fluid.level);
    let same_above = block_at(state, (x as f64, (y + 1) as f64, z as f64), dimension)
        .and_then(fluid_state)
        .is_some_and(|above| above.kind == fluid.kind);
    let height = if same_above { 1.0 } else { own };
    let flow = fluid_flow(state, dimension, (x, y, z), fluid.kind, fluid.level, own);
    Some(FluidCell {
        kind: fluid.kind,
        height,
        flow,
    })
}

/// Vanilla `FlowingFluid.getOwnHeight`: `amount / 9`, where a source (block level 0) counts as 8
/// and falling block levels 8..=15 map back to fluid levels 1..=8.
fn own_fluid_height(block_level: u8) -> f64 {
    let amount = match block_level {
        0 => 8,
        level if level >= 8 => level - 7,
        level => level,
    };
    f64::from(amount) / 9.0
}

/// Vanilla `FlowingFluid.getFlow`: the direction the fluid at `pos` flows, from the height
/// difference with its four horizontal neighbours. A unit vector, or zero in still fluid.
fn fluid_flow(
    state: &GlobalStateResource,
    dimension: &str,
    (x, y, z): (i32, i32, i32),
    kind: FluidKind,
    block_level: u8,
    own_height: f64,
) -> DVec3 {
    // `Direction.Plane.HORIZONTAL`: north, east, south, west.
    const HORIZONTAL: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    let fluid_at = |(x, y, z): (i32, i32, i32)| {
        block_at(state, (x as f64, y as f64, z as f64), dimension).and_then(fluid_state)
    };

    let mut flow_x = 0.0;
    let mut flow_z = 0.0;
    for (dx, dz) in HORIZONTAL {
        let neighbour = fluid_at((x + dx, y, z + dz));
        // `affectsFlow`: the neighbour cell is empty or holds the same fluid.
        if !neighbour.is_none_or(|n| n.kind == kind) {
            continue;
        }
        let mut n_height = neighbour.map_or(0.0, |n| own_fluid_height(n.level));
        let mut diff = 0.0;
        if n_height == 0.0 {
            // The neighbour cell is empty: the flow still spreads if the fluid continues below
            // it and the block in the neighbour cell does not block motion.
            let blocks_motion = block_at(state, ((x + dx) as f64, y as f64, (z + dz) as f64), dimension)
                .is_some_and(|block| is_solid(&block));
            if !blocks_motion {
                if let Some(below) = fluid_at((x + dx, y - 1, z + dz)).filter(|b| b.kind == kind) {
                    let below_height = own_fluid_height(below.level);
                    if below_height > 0.0 {
                        n_height = below_height;
                        diff = own_height - (n_height - FLOW_SLOPE_CONST);
                    }
                }
            }
        } else if n_height > 0.0 {
            diff = own_height - n_height;
        }
        if diff != 0.0 {
            flow_x += f64::from(dx) * diff;
            flow_z += f64::from(dz) * diff;
        }
    }

    let mut flow = DVec3::new(flow_x, 0.0, flow_z);
    if block_level >= 8 {
        // FALLING: next to a solid face the flow dives straight down.
        for (dx, dz) in HORIZONTAL {
            if is_solid_face(state, dimension, (x + dx, y, z + dz), kind)
                || is_solid_face(state, dimension, (x + dx, y + 1, z + dz), kind)
            {
                flow = normalize(flow) + DVec3::new(0.0, -6.0, 0.0);
                break;
            }
        }
    }
    normalize(flow)
}

/// Vanilla `FlowingFluid.isSolidFace` for a horizontal side, approximated with the block's
/// collision shape: the neighbour is not the same fluid and blocks motion.
fn is_solid_face(
    state: &GlobalStateResource,
    dimension: &str,
    (x, y, z): (i32, i32, i32),
    kind: FluidKind,
) -> bool {
    match block_at(state, (x as f64, y as f64, z as f64), dimension) {
        Some(block) => !fluid_state(block).is_some_and(|f| f.kind == kind) && is_solid(&block),
        None => false,
    }
}

/// Vanilla `Vec3.normalize`: vectors shorter than 1e-4 normalize to zero instead of NaN.
fn normalize(v: DVec3) -> DVec3 {
    let len = v.length();
    if len < NORMALIZE_EPS { DVec3::ZERO } else { v / len }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_math::Vec3A;

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
            if fall_step(&mut position, &mut velocity, |_from, _to| surface, None) {
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
            let still_resting = fall_step(&mut resting, &mut velocity, |_from, _to| 64.0, None);
            assert!(
                still_resting,
                "the item must keep resting on the surface, not drift down (tick {tick})"
            );
            assert_eq!(resting.coords.y, 64.0, "the item sank on tick {tick}");
        }
    }

    #[test]
    fn a_player_thrown_stack_spawns_below_eye_height_without_look_offset() {
        // Vanilla `LivingEntity.createItemStackToDrop` spawns a player-thrown stack at
        // `(getX(), getEyeY() - 0.3, getZ())` - no offset along the view vector - with a velocity
        // of `0.3` along it plus a `0.1` upward bias and a small random kick.
        let position = Position::new(10.0, 64.0, -3.0);
        let rotation = Rotation::new(45.0, 0.0);

        let (drop_position, velocity) = thrown_stack_motion(&position, &rotation);

        assert_eq!(drop_position.coords.x, 10.0);
        assert_eq!(drop_position.coords.z, -3.0);
        assert!((drop_position.coords.y - (64.0 + PLAYER_EYE_HEIGHT - DROP_BELOW_EYE)).abs() < 1e-9);

        let (dir_x, dir_y, dir_z) = rotation.to_direction_vector();
        assert!((f64::from(velocity.vec.x) - f64::from(dir_x) * THROW_SPEED).abs() <= 0.02 + 1e-6);
        assert!((f64::from(velocity.vec.z) - f64::from(dir_z) * THROW_SPEED).abs() <= 0.02 + 1e-6);
        let vy = f64::from(velocity.vec.y);
        let base_vy = f64::from(dir_y) * THROW_SPEED + THROW_UPWARD_BIAS;
        assert!(
            (base_vy - 0.1 - 1e-6..=base_vy + 0.1 + 1e-6).contains(&vy),
            "thrown vy {vy} must be the view-vector throw plus the 0.1 bias plus at most 0.1 of noise"
        );
    }

    #[test]
    fn gravity_moves_the_item_before_the_drag_is_applied() {
        // Vanilla `ItemEntity.tick` order: `applyGravity` -> `move` -> drag. A tick from rest must
        // move the item by exactly the post-gravity velocity, and only then scale the velocity.
        let mut position = Position::new(0.5, 70.0, 0.5);
        let mut velocity = Velocity::zero();
        let no_surface = |_from: DVec3, _to: DVec3| f64::from(MIN_WORLD_Y);

        let landed = fall_step(&mut position, &mut velocity, no_surface, None);

        assert!(!landed);
        assert!((position.coords.y - (70.0 - f64::from(GRAVITY))).abs() < 1e-6);
        assert!(
            (f64::from(velocity.vec.y) + f64::from(GRAVITY) * f64::from(AIR_DRAG)).abs() < 1e-6
        );
    }

    #[test]
    fn a_throw_is_stopped_by_a_wall_even_when_the_step_lands_past_it() {
        // A one-block-thick wall at column x = 5. The step of 2.0 from x = 4.0 would end at
        // x = 6.0, whose box no longer overlaps the wall: only a swept check catches it.
        let wall = |p: DVec3| p.x.floor() == 5.0;
        let mut position = Position::new(4.0, 70.0, 0.5);
        let mut velocity = Velocity::new(2.0, 0.0, 0.0);

        slide_step(&mut position, &mut velocity, &wall);

        assert_eq!(position.coords.x, 5.0 - ITEM_WIDTH / 2.0);
        assert_eq!(velocity.vec.x, 0.0);
        assert_eq!(position.coords.z, 0.5);
    }

    #[test]
    fn a_throw_into_a_wall_slides_along_it_on_the_other_axis() {
        // Same wall at column x = 5, thrown diagonally: X is clamped to the face, Z still moves.
        let wall = |p: DVec3| p.x.floor() == 5.0;
        let mut position = Position::new(4.0, 70.0, 0.5);
        let mut velocity = Velocity::new(2.0, 0.0, 0.3);

        slide_step(&mut position, &mut velocity, &wall);

        assert_eq!(position.coords.x, 5.0 - ITEM_WIDTH / 2.0);
        assert_eq!(velocity.vec.x, 0.0);
        assert!((position.coords.z - 0.8).abs() < 1e-6);
        assert_eq!(velocity.vec.z, 0.3);
    }

    #[test]
    fn a_throw_against_a_wall_clamps_to_its_near_face_when_moving_negative() {
        // Wall at columns x <= 2, thrown in -X: the box stops at the wall's +X face.
        let wall = |p: DVec3| p.x.floor() <= 2.0;
        let mut position = Position::new(3.0, 70.0, 0.5);
        let mut velocity = Velocity::new(-2.0, 0.0, 0.0);

        slide_step(&mut position, &mut velocity, &wall);

        assert_eq!(position.coords.x, 3.0 + ITEM_WIDTH / 2.0);
        assert_eq!(velocity.vec.x, 0.0);
    }

    #[test]
    fn a_throw_with_no_wall_moves_the_full_step() {
        let open = |_p: DVec3| false;
        let mut position = Position::new(4.0, 70.0, 0.5);
        let mut velocity = Velocity::new(0.3, 0.0, -0.2);

        slide_step(&mut position, &mut velocity, &open);

        assert!((position.coords.x - 4.3).abs() < 1e-6);
        assert!((position.coords.z - 0.3).abs() < 1e-6);
        assert_eq!(velocity.vec.x, 0.3);
        assert_eq!(velocity.vec.z, -0.2);
    }

    #[test]
    fn a_fast_fall_is_stopped_by_a_surface_it_crosses_mid_step() {
        // Terminal-velocity step: from y = 70 with velocity -10 the landing point y ~ 60 is
        // already below the surface at 64. The swept scan must still report that surface
        // instead of only looking at the landing cell, or the item tunnels through the floor.
        let mut position = Position::new(0.5, 70.0, 0.5);
        let mut velocity = Velocity::new(0.0, -10.0, 0.0);
        let surface_of = |from: DVec3, to: DVec3| {
            if to.y <= 64.0 && from.y >= 64.0 {
                64.0
            } else {
                f64::from(MIN_WORLD_Y)
            }
        };

        let resting = fall_step(&mut position, &mut velocity, surface_of, None);

        assert!(resting, "the item must land on the surface it crossed");
        assert_eq!(position.coords.y, 64.0);
        assert_eq!(velocity.vec.y, 0.0);
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

    #[test]
    fn water_applies_buoyancy_and_drag_but_no_gravity() {
        // Vanilla `setFluidMovement(0.99)`: x/z *= 0.99, y += 0.0005 while y < 0.06, no gravity;
        // after the move the drag is 0.98 on every axis.
        let mut position = Position::new(0.5, 70.0, 0.5);
        let mut velocity = Velocity::new(0.3, -0.5, 0.0);
        let no_surface = |_from: DVec3, _to: DVec3| f64::from(MIN_WORLD_Y);

        let landed = fall_step(&mut position, &mut velocity, no_surface, Some(FluidKind::Water));

        assert!(!landed);
        let expected_vy = (-0.5 + f64::from(FLUID_BUOYANCY)) * f64::from(AIR_DRAG);
        assert!(
            (f64::from(velocity.vec.y) - expected_vy).abs() < 1e-6,
            "vy must be (vy + buoyancy) * 0.98, not vy - gravity: {} vs {expected_vy}",
            f64::from(velocity.vec.y)
        );
        let expected_vx = 0.3 * f64::from(WATER_DRAG) * f64::from(AIR_DRAG);
        assert!((f64::from(velocity.vec.x) - expected_vx).abs() < 1e-6);
        assert!((position.coords.y - (70.0 + (-0.5 + f64::from(FLUID_BUOYANCY)))).abs() < 1e-6);
    }

    #[test]
    fn an_item_bobbing_at_the_surface_keeps_the_water_branch() {
        // The old bug: the feet-block check flipped at the surface and gravity came back for a
        // tick. Vanilla latches the box scan, so an item less than 0.1 deep still counts as in
        // water for the branch as long as `wasTouchingWater` is set - here it rises at 0.05.
        let mut position = Position::new(0.5, 64.8, 0.5);
        let mut velocity = Velocity::new(0.1, 0.05, 0.0);
        let no_surface = |_from: DVec3, _to: DVec3| f64::from(MIN_WORLD_Y);

        let landed = fall_step(&mut position, &mut velocity, no_surface, Some(FluidKind::Water));

        assert!(!landed, "a rising item must not land on the water surface");
        // 0.05 < 0.06, so buoyancy applies: (0.05 + 0.0005) * 0.98, and never 0.05 - 0.04.
        let expected_vy = (0.05 + f64::from(FLUID_BUOYANCY)) * f64::from(AIR_DRAG);
        assert!((f64::from(velocity.vec.y) - expected_vy).abs() < 1e-6);
        assert!(f64::from(velocity.vec.y) > 0.0);
    }

    #[test]
    fn buoyancy_stops_above_the_rise_limit() {
        // Vanilla adds 5.0E-4 only while y < 0.06; above that the water branch adds nothing.
        let mut position = Position::new(0.5, 70.0, 0.5);
        let mut velocity = Velocity::new(0.0, 0.1, 0.0);
        let no_surface = |_from: DVec3, _to: DVec3| f64::from(MIN_WORLD_Y);

        fall_step(&mut position, &mut velocity, no_surface, Some(FluidKind::Water));

        let expected_vy = 0.1 * f64::from(AIR_DRAG);
        assert!((f64::from(velocity.vec.y) - expected_vy).abs() < 1e-6);
    }

    #[test]
    fn lava_uses_the_0_95_multiplier() {
        let mut position = Position::new(0.5, 70.0, 0.5);
        let mut velocity = Velocity::new(0.2, -0.1, 0.0);
        let no_surface = |_from: DVec3, _to: DVec3| f64::from(MIN_WORLD_Y);

        fall_step(&mut position, &mut velocity, no_surface, Some(FluidKind::Lava));

        let expected_vy = (-0.1 + f64::from(FLUID_BUOYANCY)) * f64::from(AIR_DRAG);
        let expected_vx = 0.2 * f64::from(LAVA_DRAG) * f64::from(AIR_DRAG);
        assert!((f64::from(velocity.vec.y) - expected_vy).abs() < 1e-6);
        assert!((f64::from(velocity.vec.x) - expected_vx).abs() < 1e-6);
    }

    #[test]
    fn an_item_lands_on_the_floor_under_water() {
        // The scan passes through the fluid and finds the solid floor, so a sinking item rests
        // on it instead of falling through the lake bed (vanilla `move` collides the same way).
        let mut position = Position::new(0.5, 64.3, 0.5);
        let mut velocity = Velocity::new(0.0, -0.5, 0.0);
        let floor = |_from: DVec3, _to: DVec3| 64.0;

        let landed = fall_step(&mut position, &mut velocity, floor, Some(FluidKind::Water));

        assert!(landed);
        assert_eq!(position.coords.y, 64.0);
        assert_eq!(velocity.vec.y, 0.0);
    }

    #[test]
    fn the_branch_needs_more_than_a_tenth_of_fluid_depth() {
        // Vanilla: `isInWater() && getFluidHeight(WATER) > 0.1`. A box barely touching the
        // surface (<= 0.1) falls back to gravity, which is what settles the surface bob.
        assert_eq!(branch_medium(true, 0.5, 0.0), Some(FluidKind::Water));
        assert_eq!(branch_medium(true, 0.1, 0.0), None);
        assert_eq!(branch_medium(true, 0.05, 0.0), None);
        assert_eq!(branch_medium(false, 0.0, 0.5), Some(FluidKind::Lava));
        assert_eq!(branch_medium(false, 0.0, 0.0), None);
    }

    #[test]
    fn the_fluid_scan_measures_the_box_depth() {
        // A box whose feet are at 64.2 overlapping a full water cell at y = 64 (surface 65.0)
        // is 0.8 deep in water.
        let water_cell = |(x, y, z): (i32, i32, i32)| {
            if (x, y, z) == (0, 64, 0) {
                Some(FluidCell {
                    kind: FluidKind::Water,
                    height: 1.0,
                    flow: DVec3::ZERO,
                })
            } else {
                None
            }
        };
        let mut velocity = Velocity::zero();
        let scan_box = ScanBox {
            min_y: 64.2,
            x0: 0,
            x1: 1,
            y0: 64,
            y1: 65,
            z0: 0,
            z1: 1,
        };

        let (touching, depth) = scan_fluid_kind(
            &scan_box,
            FluidKind::Water,
            WATER_PUSH,
            &mut velocity,
            &water_cell,
        );

        assert!(touching);
        assert!((depth - 0.8).abs() < 1e-9);
        // Still water (zero flow) pushes nothing.
        assert_eq!(velocity.vec, Vec3A::ZERO);
    }

    #[test]
    fn a_current_pushes_a_still_item() {
        // One cell with a unit flow in +X: the sum normalizes to +X and scales by the motion
        // scale (0.014 for water), which is above the 0.0045 stillness fallback.
        let cell = |_: (i32, i32, i32)| {
            Some(FluidCell {
                kind: FluidKind::Water,
                height: 1.0,
                flow: DVec3::new(1.0, 0.0, 0.0),
            })
        };
        let mut velocity = Velocity::zero();
        let scan_box = ScanBox {
            min_y: 64.0,
            x0: 0,
            x1: 1,
            y0: 64,
            y1: 65,
            z0: 0,
            z1: 1,
        };

        scan_fluid_kind(&scan_box, FluidKind::Water, WATER_PUSH, &mut velocity, &cell);

        assert!((f64::from(velocity.vec.x) - WATER_PUSH).abs() < 1e-6);
        assert_eq!(velocity.vec.y, 0.0);
        assert_eq!(velocity.vec.z, 0.0);
    }

    #[test]
    fn own_fluid_height_matches_vanilla_amounts() {
        // Source = 8/9; flowing levels are level/9; falling block levels 8..=15 map to 1..=8.
        assert!((own_fluid_height(0) - 8.0 / 9.0).abs() < 1e-9);
        assert!((own_fluid_height(1) - 1.0 / 9.0).abs() < 1e-9);
        assert!((own_fluid_height(7) - 7.0 / 9.0).abs() < 1e-9);
        assert!((own_fluid_height(8) - 1.0 / 9.0).abs() < 1e-9);
        assert!((own_fluid_height(15) - 8.0 / 9.0).abs() < 1e-9);
    }
}
