//! Applies damage to players, keeps the client's health bar in sync and fires death events.
//!
//! This is the only place where the server mutates [`Health`] for a player, so the value held by
//! the entity, the value shown on the client and the death event cannot drift apart. Damage coming
//! from invulnerable (creative or spectator) players is dropped here rather than at every call site.

use bevy_ecs::prelude::{Entity, MessageReader, MessageWriter, Query};
use ferrumc_components::health::Health;
use ferrumc_components::player::abilities::PlayerAbilities;
use ferrumc_components::player::gameplay_state::dead::Dead;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_messages::player_damage::{DamageSource, PlayerDamaged, PlayerDied};
use ferrumc_messages::teleport_player::TeleportPlayer;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::hurt_animation::HurtAnimationPacket;
use ferrumc_net::packets::outgoing::set_health::SetHealth;
use tracing::{debug, trace, warn};

/// Food level sent alongside health in the `SetHealth` packet.
///
/// Hunger is not driven by the survival systems yet, so the client's hunger bar is kept full until
/// the hunger system owns those values.
const FOOD_LEVEL: i32 = 20;
/// Saturation sent alongside health in the `SetHealth` packet.
const SATURATION: f32 = 5.0;

pub fn apply_damage(
    mut reader: MessageReader<PlayerDamaged>,
    mut healths: Query<(
        Entity,
        &PlayerIdentity,
        &PlayerAbilities,
        &mut Health,
        Option<&Dead>,
    )>,
    mut death_writer: MessageWriter<PlayerDied>,
    conn_query: Query<(Entity, &PlayerIdentity, &StreamWriter)>,
    transforms: Query<(&Position, &Rotation)>,
    mut teleport_writer: MessageWriter<TeleportPlayer>,
) {
    for event in reader.read() {
        if event.amount <= 0.0 {
            continue;
        }

        let Ok((entity, identity, abilities, mut health, is_dead)) = healths.get_mut(event.player)
        else {
            trace!(
                "Ignoring damage for {:?}: the entity has no health component",
                event.player
            );
            continue;
        };

        if is_dead.is_some() {
            debug!(
                "Ignoring damage for {}: the player is already dead",
                identity.username
            );
            continue;
        }

        if abilities.invulnerable {
            trace!(
                "Ignoring damage for {}: the player is invulnerable",
                identity.username
            );
            continue;
        }

        let damage = event.amount.min(health.current);
        health.current -= damage;
        debug!(
            "{} took {} damage ({:?}), health is now {}/{}",
            identity.username, damage, event.source, health.current, health.max
        );

        if let Ok((_, _, conn)) = conn_query.get(entity) {
            // Hunger and saturation are not driven by the survival systems yet, so they are sent
            // at their defaults; the client only uses them for the hunger bar overlay.
            if let Err(err) =
                conn.send_packet(SetHealth::new(health.current, FOOD_LEVEL, SATURATION))
            {
                warn!(
                    "Failed to send health update to {}: {err:?}",
                    identity.username
                );
            }
        }

        broadcast_hurt_animation(entity, identity.short_uuid, event.yaw, &conn_query);

        // A melee hit pushes the victim away from the attacker. The push is applied as a small
        // teleport because the client owns its own position, so a velocity it ignores would not
        // move the player at all.
        if let DamageSource::Melee { attacker } = event.source {
            apply_knockback(entity, attacker, &transforms, &mut teleport_writer);
        }

        if health.current <= 0.0 {
            death_writer.write(PlayerDied {
                player: entity,
                killer: event.attacker(),
            });
        }
    }
}

/// Horizontal distance the victim is pushed by a melee hit, in blocks.
const KNOCKBACK_STRENGTH: f64 = 0.4;
/// Upward lift applied with a melee hit, in blocks.
const KNOCKBACK_LIFT: f64 = 0.36;

/// Pushes `victim` away from `attacker`.
///
/// The push is sent as a teleport because the client is authoritative over its own position: a
/// velocity it is free to ignore would not move the player. The victim's facing is preserved so
/// their view does not snap around.
fn apply_knockback(
    victim: Entity,
    attacker: Entity,
    transforms: &Query<(&Position, &Rotation)>,
    teleport_writer: &mut MessageWriter<TeleportPlayer>,
) {
    let Ok((victim_position, victim_rotation)) = transforms.get(victim) else {
        return;
    };
    let Ok((attacker_position, _)) = transforms.get(attacker) else {
        return;
    };

    let mut delta = victim_position.coords - attacker_position.coords;
    delta.y = 0.0;
    if delta.length_squared() < f64::EPSILON {
        // Standing exactly on top of each other: any horizontal direction is as good as another.
        delta = bevy_math::DVec3::new(0.0, 0.0, 1.0);
    }
    let direction = delta.normalize();

    teleport_writer.write(TeleportPlayer {
        entity: victim,
        x: victim_position.coords.x + direction.x * KNOCKBACK_STRENGTH,
        y: victim_position.coords.y + KNOCKBACK_LIFT,
        z: victim_position.coords.z + direction.z * KNOCKBACK_STRENGTH,
        vel_x: direction.x * KNOCKBACK_STRENGTH,
        vel_y: KNOCKBACK_LIFT,
        vel_z: direction.z * KNOCKBACK_STRENGTH,
        yaw: victim_rotation.yaw,
        pitch: victim_rotation.pitch,
    });
}

/// Broadcasts the red flash and damage sound to every player except the victim.
fn broadcast_hurt_animation(
    victim: Entity,
    victim_entity_id: i32,
    yaw: f32,
    conn_query: &Query<(Entity, &PlayerIdentity, &StreamWriter)>,
) {
    let packet = HurtAnimationPacket::new(victim_entity_id, yaw);
    for (entity, _, conn) in conn_query.iter() {
        if entity == victim {
            continue;
        }
        if !conn.is_running() {
            continue;
        }
        if let Err(err) = conn.send_packet_ref(&packet) {
            warn!("Failed to broadcast hurt animation to {entity:?}: {err:?}");
        }
    }
}
