//! Projectile entities: thrown ender pearls, snowballs, eggs and firework rockets.
//!
//! A projectile is spawned by [`use_held_item`] when the item in hand is a throwable (vanilla's
//! `EnderPearlItem`, `SnowballItem`, `EggItem`, `FireworkRocketItem`) rather than a block or
//! food. Vanilla tries `useItemOn` first and, when the item places nothing, falls through to
//! `useItem`; both paths end here.
//!
//! Movement runs once per tick in [`tick_projectiles`]: vanilla's `ThrowableProjectile` applies
//! gravity 0.03 and air drag 0.99 each tick and despawns on the first block it touches. The spawn
//! packet carries the owner's entity id in its `data` field (`Projectile.getAddEntityPacket`),
//! which is how the client links the projectile to its owner — and how a firework rocket knows
//! whose elytra flight to boost.

use bevy_ecs::prelude::{Commands, Entity, MessageWriter, Query, Res, With, Without};
use bevy_ecs::system::SystemParam;
use bevy_math::DVec3;
use ferrumc_components::projectile::{Projectile, ProjectileKind};
use ferrumc_core::identity::entity_identity::EntityIdentity;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_data::generated::items::Item;
use ferrumc_entities::LastSyncedPosition;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_macros::get_registry_entry;
use ferrumc_messages::entity_update::SendEntityUpdate;
use ferrumc_messages::player_damage::{DamageSource, PlayerDamaged};
use ferrumc_messages::teleport_player::TeleportPlayer;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_event::EntityStatus;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net::packets::outgoing::play_sound::{PlaySoundPacket, SoundSource};
use ferrumc_net::packets::outgoing::remove_entities::RemoveEntitiesPacket;
use ferrumc_net::packets::outgoing::spawn_entity::SpawnEntityPacket;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::dimension::Dimension;
use tracing::{debug, warn};

use crate::systems::item_entities::{block_at, is_fluid_at, is_solid};

/// Speed a thrown item leaves the hand at, in blocks per tick
/// (`EnderpearlItem.PROJECTILE_SHOOT_POWER`).
const THROW_SPEED: f64 = 1.5;
/// Gravity applied to a flying projectile per tick (`ThrowableProjectile.getDefaultGravity`).
const GRAVITY: f32 = 0.03;
/// Fraction of velocity a projectile keeps per tick in air (`ThrowableProjectile.applyInertia`).
const AIR_DRAG: f32 = 0.99;
/// Fraction of velocity a projectile keeps per tick inside a fluid.
const WATER_DRAG: f32 = 0.8;
/// A player's eye height above their feet.
const PLAYER_EYE_HEIGHT: f64 = 1.62;
/// How far below eye height a projectile spawns, so it does not clip the thrower's head.
const SPAWN_DROP: f64 = 0.1;
/// Damage landing an ender pearl deals its owner (`ThrownEnderpearl.onHit`).
const ENDER_PEARL_DAMAGE: f32 = 5.0;
/// Ticks a firework rocket stays attached to its owner. Vanilla derives this from the rocket's
/// flight duration; a plain rocket boosts for about this long.
const FIREWORK_BOOST_TICKS: u32 = 30;
/// Lowest Y a projectile can reach before it is removed.
const MIN_WORLD_Y: f64 = -64.0;

// Sound event protocol ids from `assets/data/registries.json` (`minecraft:sound_event.entries`).
// The `get_registry_entry!` macro cannot be used for these: it splits the path on `.`, and sound
// event keys contain dots. `projectile_sounds_match_the_sound_registry` checks the values against
// registries.json so they cannot drift.
/// `entity.ender_pearl.throw`, played at the thrower when a pearl is thrown.
const SOUND_ENDER_PEARL_THROW: u32 = 520;
/// `entity.snowball.throw`, played at the thrower when a snowball is thrown.
const SOUND_SNOWBALL_THROW: u32 = 1413;
/// `entity.egg.throw`, played at the thrower when an egg is thrown.
const SOUND_EGG_THROW: u32 = 488;
/// `entity.player.teleport`, played where a thrown pearl lands (`ThrownEnderpearl.playSound`).
const SOUND_PLAYER_TELEPORT: u32 = 1185;
/// `entity.firework_rocket.launch`, played once by the rocket entity at spawn
/// (`FireworkRocketEntity.tick`).
const SOUND_FIREWORK_ROCKET_LAUNCH: u32 = 548;

/// The projectile an item turns into when used, or `None` when the item is not throwable.
fn projectile_kind_for_item(item_id: i32) -> Option<ProjectileKind> {
    let registry_key = Item::from_id(item_id as u16)?.registry_key;
    match registry_key {
        "minecraft:ender_pearl" => Some(ProjectileKind::EnderPearl),
        "minecraft:snowball" => Some(ProjectileKind::Snowball),
        "minecraft:egg" | "minecraft:blue_egg" | "minecraft:brown_egg" => Some(ProjectileKind::Egg),
        "minecraft:firework_rocket" => Some(ProjectileKind::FireworkRocket),
        _ => None,
    }
}

/// Protocol id of the entity type a projectile kind is rendered as.
fn entity_type_id(kind: ProjectileKind) -> i32 {
    match kind {
        ProjectileKind::EnderPearl => {
            get_registry_entry!("minecraft:entity_type.entries.minecraft:ender_pearl") as i32
        }
        ProjectileKind::Snowball => {
            get_registry_entry!("minecraft:entity_type.entries.minecraft:snowball") as i32
        }
        ProjectileKind::Egg => {
            get_registry_entry!("minecraft:entity_type.entries.minecraft:egg") as i32
        }
        ProjectileKind::FireworkRocket => {
            get_registry_entry!("minecraft:entity_type.entries.minecraft:firework_rocket") as i32
        }
    }
}

/// The velocity a thrown item leaves the hand with: throw speed along the view vector plus the
/// thrower's own movement, except vertical movement while they are on the ground
/// (`Projectile.shootFromRotation`).
fn throw_velocity(rotation: &Rotation, owner_velocity: Option<DVec3>, on_ground: bool) -> DVec3 {
    let (dx, dy, dz) = rotation.to_direction_vector();
    let mut velocity = DVec3::new(
        f64::from(dx) * THROW_SPEED,
        f64::from(dy) * THROW_SPEED,
        f64::from(dz) * THROW_SPEED,
    );
    if let Some(owner) = owner_velocity {
        velocity.x += owner.x;
        velocity.z += owner.z;
        if !on_ground {
            velocity.y += owner.y;
        }
    }
    velocity
}

/// The sound a thrown item makes and how loud it is.
///
/// Vanilla plays the throw sound in the item's `use` (`EnderPearlItem`, `SnowballItem`, `EggItem`)
/// at the player's position, category NEUTRAL, volume 0.5 and pitch `0.4 / (random * 0.4 + 0.8)`.
/// A firework rocket instead plays `entity.firework_rocket.launch` once from the rocket entity
/// itself (`FireworkRocketEntity.tick`), category AMBIENT at volume 3.0.
fn throw_sound(kind: ProjectileKind) -> (u32, SoundSource, f32, f32) {
    match kind {
        ProjectileKind::EnderPearl => (
            SOUND_ENDER_PEARL_THROW,
            SoundSource::Neutral,
            0.5,
            throw_pitch(),
        ),
        ProjectileKind::Snowball => (
            SOUND_SNOWBALL_THROW,
            SoundSource::Neutral,
            0.5,
            throw_pitch(),
        ),
        ProjectileKind::Egg => (SOUND_EGG_THROW, SoundSource::Neutral, 0.5, throw_pitch()),
        ProjectileKind::FireworkRocket => {
            (SOUND_FIREWORK_ROCKET_LAUNCH, SoundSource::Ambient, 3.0, 1.0)
        }
    }
}

/// `0.4 / (random * 0.4 + 0.8)`, the randomized throw pitch of vanilla's throw sounds.
fn throw_pitch() -> f32 {
    0.4 / (rand::random::<f32>() * 0.4 + 0.8)
}

/// Broadcasts a positional sound to every connected player — the server half of vanilla's
/// `Level.playSound(null, x, y, z, ...)`.
fn broadcast_sound(
    conn_query: &Query<(Entity, &StreamWriter)>,
    sound_id: u32,
    source: SoundSource,
    position: DVec3,
    volume: f32,
    pitch: f32,
) {
    let packet = PlaySoundPacket::new(
        sound_id, source, position.x, position.y, position.z, volume, pitch,
    );
    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&packet) {
            warn!("Failed to broadcast sound {sound_id}: {err:?}");
        }
    }
}

/// What a throw needs from the throwing player, gathered in one place so the two use paths
/// (use-item in air and use-item-on a block) share a single code path.
pub struct Thrower<'a> {
    pub entity: Entity,
    /// The player's protocol entity id, sent as the spawn packet's `data` field.
    pub protocol_id: i32,
    pub position: &'a Position,
    pub rotation: &'a Rotation,
    pub velocity: Option<&'a Velocity>,
    pub on_ground: Option<&'a OnGround>,
    pub fall_flying: bool,
    pub dimension: Dimension,
}

/// Uses the item in the player's selected hotbar slot, when that item is a throwable.
///
/// This is the vanilla `Item.use` path for projectiles: the item leaves the hand as a projectile
/// entity and one unit is consumed. A firework rocket only works while the player is gliding with
/// an elytra (`isFallFlying`); used on the ground vanilla passes and nothing happens. Anything
/// that is not a throwable returns `false` so the caller can try other uses (eating, ...).
///
/// Returns whether the item was used.
pub fn use_held_item(
    commands: &mut Commands,
    inventory: &mut Inventory,
    hotbar: &Hotbar,
    thrower: Thrower,
    conn_query: &Query<(Entity, &StreamWriter)>,
) -> bool {
    let slot_index = hotbar.get_selected_inventory_index();
    let Ok(Some(held)) = inventory.get_item(slot_index).map(|slot| slot.cloned()) else {
        return false;
    };
    let Some(item_id) = held.item_id else {
        return false;
    };
    let Some(kind) = projectile_kind_for_item(item_id.0.0) else {
        return false;
    };

    // A firework rocket only boosts an elytra flight; used on the ground vanilla passes.
    if kind == ProjectileKind::FireworkRocket && !thrower.fall_flying {
        return false;
    }

    let mut thrown = held.clone();
    thrown.count = VarInt::new(1);
    spawn_projectile(commands, kind, &thrower, thrown, conn_query);

    // Vanilla plays the throw sound at the thrower's feet in the item's `use`.
    let (sound_id, source, volume, pitch) = throw_sound(kind);
    broadcast_sound(
        conn_query,
        sound_id,
        source,
        thrower.position.coords,
        volume,
        pitch,
    );

    // One unit leaves the stack, the way `itemstack.consume(1, player)` does in vanilla.
    let remaining = held.count.0 - 1;
    let result = if remaining > 0 {
        let mut updated = held.clone();
        updated.count = VarInt::new(remaining);
        inventory.set_item_with_update(slot_index, updated, thrower.entity)
    } else {
        inventory.clear_slot_with_update(slot_index, thrower.entity)
    };
    if let Err(err) = result {
        warn!(
            "Failed to consume the thrown item for {:?}: {err:?}",
            thrower.entity
        );
    }

    true
}

/// Spawns a projectile entity and broadcasts the spawn packet to every connected player.
///
/// Vanilla spawns the projectile at eye height minus a little, with the throw speed along the
/// view vector plus the thrower's own movement. The spawn packet's `data` field carries the
/// owner's protocol id and its velocity fields the initial velocity; the item stack goes out as
/// metadata index 8 (`ThrowableItemProjectile.DATA_ITEM_STACK`), the same slot an item entity uses.
pub fn spawn_projectile(
    commands: &mut Commands,
    kind: ProjectileKind,
    thrower: &Thrower,
    item: InventorySlot,
    conn_query: &Query<(Entity, &StreamWriter)>,
) -> Entity {
    let spawn_position = Position::new(
        thrower.position.coords.x,
        thrower.position.coords.y + PLAYER_EYE_HEIGHT - SPAWN_DROP,
        thrower.position.coords.z,
    );
    let owner_velocity = thrower.velocity.map(|vel| vel.vec.as_dvec3());
    let on_ground = thrower.on_ground.map(|ground| ground.0).unwrap_or(true);
    let vec = throw_velocity(thrower.rotation, owner_velocity, on_ground);
    let velocity = Velocity::new(vec.x, vec.y, vec.z);

    let identity = EntityIdentity::new();
    let entity = commands
        .spawn((
            Projectile::new(kind, thrower.entity, item.clone(), thrower.dimension),
            identity.clone(),
            spawn_position,
            *thrower.rotation,
            velocity,
            OnGround(false),
            LastSyncedPosition(spawn_position.coords),
            // No `HasGravity`: `tick_projectiles` integrates the flight itself, and the generic
            // gravity system would add its acceleration on top of it.
        ))
        .id();

    let packet = SpawnEntityPacket::projectile(
        identity.entity_id,
        identity.uuid.as_u128(),
        entity_type_id(kind),
        &spawn_position,
        thrower.rotation,
        thrower.protocol_id,
        &velocity,
    );
    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&packet) {
            warn!("Failed to broadcast projectile spawn: {err:?}");
        }
    }

    let metadata = EntityMetadataPacket::new(
        identity.entity_id.into(),
        vec![EntityMetadata::entity_item_slot(item)],
    );
    for (_, conn) in conn_query.iter() {
        if let Err(err) = conn.send_packet_ref(&metadata) {
            warn!("Failed to broadcast projectile metadata: {err:?}");
        }
    }

    debug!(
        "Spawned {:?} projectile {} at {:?}",
        kind, identity.entity_id, spawn_position.coords
    );
    entity
}

/// Everything a projectile carries into one tick.
type ProjectileQueryItem<'a> = (
    Entity,
    &'a EntityIdentity,
    &'a mut Projectile,
    &'a mut Position,
    &'a mut Velocity,
    &'a mut OnGround,
);

/// The messages `tick_projectiles` emits, grouped into one parameter so the system stays under
/// clippy's argument limit.
#[derive(SystemParam)]
pub struct ProjectileWriters<'w> {
    update: MessageWriter<'w, SendEntityUpdate>,
    damage: MessageWriter<'w, PlayerDamaged>,
    teleport: MessageWriter<'w, TeleportPlayer>,
}

/// Advances every projectile: applies gravity and drag, moves it and resolves hits.
///
/// Like item entities, projectiles are excluded from the generic velocity system and integrate
/// their own movement here, because the hit test needs the position before and after the step.
pub fn tick_projectiles(
    mut commands: Commands,
    mut projectiles: Query<ProjectileQueryItem<'_>, With<Projectile>>,
    owners: Query<(&PlayerIdentity, &Position, &Rotation), Without<Projectile>>,
    conn_query: Query<(Entity, &StreamWriter)>,
    state: Res<GlobalStateResource>,
    mut writers: ProjectileWriters,
) {
    let mut despawned: Vec<i32> = Vec::new();

    for (entity, identity, mut projectile, mut position, mut velocity, mut grounded) in
        projectiles.iter_mut()
    {
        projectile.age = projectile.age.saturating_add(1);

        // A firework rocket does not fly on its own: it stays attached to its owner and the
        // client applies the boost while the owner is gliding. It is removed once the boost
        // would be over.
        if projectile.kind == ProjectileKind::FireworkRocket {
            if let Ok((_, owner_position, _)) = owners.get(projectile.owner) {
                position.coords = owner_position.coords;
            }
            if projectile.age >= FIREWORK_BOOST_TICKS {
                despawned.push(identity.entity_id);
            } else {
                writers.update.write(SendEntityUpdate(entity));
            }
            continue;
        }

        if projectile.is_expired() {
            despawned.push(identity.entity_id);
            continue;
        }

        let in_fluid = is_fluid_at(&state, position.coords, projectile.dimension);

        // Vanilla `ThrowableProjectile.tick`: gravity, then inertia drag, then move and test
        // the path against blocks.
        velocity.vec.y -= GRAVITY;
        velocity.vec *= if in_fluid { WATER_DRAG } else { AIR_DRAG };

        let next = position.coords + velocity.vec.as_dvec3();

        if next.y < MIN_WORLD_Y {
            despawned.push(identity.entity_id);
            continue;
        }

        if let Some(hit) = projectile_hit(&state, projectile.dimension, position.coords, next) {
            position.coords = hit;
            on_projectile_hit(
                &projectile,
                hit,
                &owners,
                &conn_query,
                &mut writers.damage,
                &mut writers.teleport,
            );
            // Vanilla's `Snowball.onHit`/`ThrownEgg.onHit` broadcast entity event 3, which makes
            // the client burst the item into particles; throwables have no impact *sound*.
            if matches!(
                projectile.kind,
                ProjectileKind::Snowball | ProjectileKind::Egg
            ) {
                let event = EntityStatus {
                    entity_id: identity.entity_id,
                    status: 3,
                };
                for (_, conn) in conn_query.iter() {
                    if let Err(err) = conn.send_packet_ref(&event) {
                        warn!("Failed to broadcast projectile impact event: {err:?}");
                    }
                }
            }
            despawned.push(identity.entity_id);
            continue;
        }

        position.coords = next;
        grounded.0 = false;
        writers.update.write(SendEntityUpdate(entity));
    }

    if !despawned.is_empty() {
        let packet = RemoveEntitiesPacket::from_entity_ids(despawned.iter().copied());
        for (_, conn) in conn_query.iter() {
            if let Err(err) = conn.send_packet_ref(&packet) {
                warn!("Failed to broadcast projectile despawn: {err:?}");
            }
        }

        // Entities are removed by a discard pass, because `Commands` cannot run while the query
        // above is still borrowed by the loop.
        let removed: Vec<Entity> = projectiles
            .iter()
            .filter(|(_, identity, ..)| despawned.contains(&identity.entity_id))
            .map(|(entity, ..)| entity)
            .collect();
        for entity in removed {
            commands.entity(entity).despawn();
        }
    }
}

/// Whether a projectile flying from `from` to `to` hits a solid block this tick, and where.
fn projectile_hit(
    state: &GlobalStateResource,
    dimension: Dimension,
    from: DVec3,
    to: DVec3,
) -> Option<DVec3> {
    let dimension = dimension.as_str();
    projectile_hit_at(from, to, |at| {
        block_at(state, (at.x, at.y, at.z), dimension).is_some_and(|block| is_solid(&block))
    })
}

/// The hit test itself, with the world read behind a closure so it can be tested without one.
///
/// Vanilla raycasts the whole movement vector; at throw speed a projectile moves up to 1.5 blocks
/// per tick, so testing the start, the midpoint and the endpoint keeps it from tunnelling
/// through a thin block between ticks.
fn projectile_hit_at(from: DVec3, to: DVec3, solid_at: impl Fn(DVec3) -> bool) -> Option<DVec3> {
    let midpoint = from + (to - from) * 0.5;
    if solid_at(from) {
        Some(from)
    } else if solid_at(to) {
        Some(to)
    } else if solid_at(midpoint) {
        Some(midpoint)
    } else {
        None
    }
}

/// Applies the effect of a projectile hitting something at `hit`.
///
/// An ender pearl teleports its owner to the hit position and deals 5 damage
/// (`ThrownEnderpearl.onHit`); the other throwables just despawn (their entity hits are part of
/// combat, which is not implemented yet).
fn on_projectile_hit(
    projectile: &Projectile,
    hit: DVec3,
    owners: &Query<(&PlayerIdentity, &Position, &Rotation), Without<Projectile>>,
    conn_query: &Query<(Entity, &StreamWriter)>,
    damage_writer: &mut MessageWriter<PlayerDamaged>,
    teleport_writer: &mut MessageWriter<TeleportPlayer>,
) {
    if projectile.kind != ProjectileKind::EnderPearl {
        return;
    }

    let Ok((_, _, owner_rotation)) = owners.get(projectile.owner) else {
        return;
    };

    // Vanilla plays `entity.player.teleport` at the pearl's landing spot (`ThrownEnderpearl`
    // .playSound), category PLAYERS at full volume.
    broadcast_sound(
        conn_query,
        SOUND_PLAYER_TELEPORT,
        SoundSource::Players,
        hit,
        1.0,
        1.0,
    );

    // The client owns its own position, so the teleport goes through the same `TeleportPlayer`
    // message knockback uses: it updates the server copy and sends the position-sync packet the
    // client confirms. Velocity is zeroed, the way vanilla's pearl teleport does.
    teleport_writer.write(TeleportPlayer {
        entity: projectile.owner,
        x: hit.x,
        y: hit.y,
        z: hit.z,
        vel_x: 0.0,
        vel_y: 0.0,
        vel_z: 0.0,
        yaw: owner_rotation.yaw,
        pitch: owner_rotation.pitch,
    });
    damage_writer.write(PlayerDamaged::from_source(
        projectile.owner,
        ENDER_PEARL_DAMAGE,
        DamageSource::EnderPearl,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearls_snowballs_eggs_and_firework_rockets_are_throwable() {
        assert_eq!(
            projectile_kind_for_item(1070),
            Some(ProjectileKind::EnderPearl)
        );
        assert_eq!(
            projectile_kind_for_item(971),
            Some(ProjectileKind::Snowball)
        );
        assert_eq!(projectile_kind_for_item(986), Some(ProjectileKind::Egg));
        assert_eq!(
            projectile_kind_for_item(1192),
            Some(ProjectileKind::FireworkRocket)
        );
    }

    #[test]
    fn blocks_and_tools_are_not_throwable() {
        assert_eq!(projectile_kind_for_item(1), None); // stone
        assert_eq!(projectile_kind_for_item(310), None); // torch
    }

    #[test]
    fn projectile_entity_types_match_the_registry() {
        assert_eq!(entity_type_id(ProjectileKind::EnderPearl), 42);
        assert_eq!(entity_type_id(ProjectileKind::Snowball), 115);
        assert_eq!(entity_type_id(ProjectileKind::Egg), 37);
        assert_eq!(entity_type_id(ProjectileKind::FireworkRocket), 51);
    }

    #[test]
    fn projectile_sounds_match_the_sound_registry() {
        let registries: serde_json::Value =
            serde_json::from_str(include_str!("../../../../assets/data/registries.json")).unwrap();
        let entries = &registries["minecraft:sound_event"]["entries"];
        let id = |name: &str| {
            entries[format!("minecraft:{name}").as_str()]["protocol_id"]
                .as_u64()
                .unwrap() as u32
        };
        assert_eq!(SOUND_ENDER_PEARL_THROW, id("entity.ender_pearl.throw"));
        assert_eq!(SOUND_SNOWBALL_THROW, id("entity.snowball.throw"));
        assert_eq!(SOUND_EGG_THROW, id("entity.egg.throw"));
        assert_eq!(SOUND_PLAYER_TELEPORT, id("entity.player.teleport"));
        assert_eq!(
            SOUND_FIREWORK_ROCKET_LAUNCH,
            id("entity.firework_rocket.launch")
        );
        assert_eq!(
            throw_sound(ProjectileKind::EnderPearl).0,
            SOUND_ENDER_PEARL_THROW
        );
        assert_eq!(
            throw_sound(ProjectileKind::Snowball).0,
            SOUND_SNOWBALL_THROW
        );
        assert_eq!(throw_sound(ProjectileKind::Egg).0, SOUND_EGG_THROW);
        assert_eq!(
            throw_sound(ProjectileKind::FireworkRocket).0,
            SOUND_FIREWORK_ROCKET_LAUNCH
        );
    }

    #[test]
    fn throw_sounds_are_neutral_and_half_volume_but_the_rocket_launch_is_loud() {
        let (_, source, volume, _) = throw_sound(ProjectileKind::Snowball);
        assert_eq!(source, SoundSource::Neutral);
        assert_eq!(volume, 0.5);

        let (_, source, volume, _) = throw_sound(ProjectileKind::FireworkRocket);
        assert_eq!(source, SoundSource::Ambient);
        assert_eq!(volume, 3.0);
    }

    #[test]
    fn a_throw_follows_the_view_vector_at_throw_speed() {
        // Yaw 0 looks towards +Z.
        let velocity = throw_velocity(&Rotation::new(0.0, 0.0), None, true);
        assert!(velocity.x.abs() < 1e-9);
        assert!(velocity.y.abs() < 1e-9);
        assert!((velocity.z - THROW_SPEED).abs() < 1e-9);
    }

    #[test]
    fn a_throw_adds_the_throwers_movement_but_not_vertical_while_grounded() {
        let owner = DVec3::new(0.1, 0.5, -0.2);

        let grounded = throw_velocity(&Rotation::new(0.0, 0.0), Some(owner), true);
        assert!(
            grounded.y.abs() < 1e-9,
            "no vertical boost while on the ground"
        );
        assert!((grounded.x - 0.1).abs() < 1e-9);
        assert!((grounded.z - (THROW_SPEED - 0.2)).abs() < 1e-9);

        let airborne = throw_velocity(&Rotation::new(0.0, 0.0), Some(owner), false);
        assert!((airborne.y - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_projectile_inside_a_wall_hits_immediately() {
        let from = DVec3::new(0.5, 64.5, 2.5);
        let to = from + DVec3::new(0.0, 0.0, 1.0);
        assert_eq!(projectile_hit_at(from, to, |at| at.z >= 2.0), Some(from));
    }

    #[test]
    fn a_fast_projectile_cannot_tunnel_through_a_thin_wall() {
        // Flying 3 blocks in one tick past a one-block wall at z in [2, 3): neither endpoint is
        // inside it, but the midpoint is.
        let from = DVec3::new(0.5, 64.5, 0.5);
        let to = from + DVec3::new(0.0, 0.0, 3.0);
        let hit = projectile_hit_at(from, to, |at| at.z >= 2.0 && at.z < 3.0);
        assert_eq!(hit, Some(from + DVec3::new(0.0, 0.0, 1.5)));
    }

    #[test]
    fn a_clear_flight_hits_nothing() {
        let from = DVec3::new(0.5, 64.5, 0.5);
        let to = from + DVec3::new(1.0, 0.0, 0.0);
        assert_eq!(projectile_hit_at(from, to, |_| false), None);
    }
}
