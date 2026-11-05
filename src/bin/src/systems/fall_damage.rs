//! Fall and void damage.
//!
//! The client owns the player's vertical movement, so the server cannot integrate the fall itself:
//! it watches the reported position instead. While a player is in the air the highest point they
//! reached is tracked, and the difference to the landing height is the fall distance that vanilla
//! charges for, ignoring the first three blocks.
//!
//! Falling out of the world is a separate rule: below the world floor a player takes damage every
//! tick until they die, instead of one hit based on the distance fallen.

use bevy_ecs::prelude::{Entity, MessageWriter, Query};
use ferrumc_components::player::fall_tracker::FallTracker;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_messages::player_damage::{DamageSource, PlayerDamaged};
use tracing::trace;

/// Fall distance that is free: the first three blocks never hurt.
const SAFE_FALL_DISTANCE: f64 = 3.0;
/// Damage per block fallen past the free distance. Vanilla uses one half heart per block.
const DAMAGE_PER_BLOCK: f32 = 1.0;
/// Y coordinate below which the world is void.
const VOID_Y: f64 = -64.0;
/// Damage taken while inside the void.
const VOID_DAMAGE: f32 = 4.0;

pub fn apply_fall_damage(
    mut players: Query<(Entity, &Position, &OnGround, &mut FallTracker)>,
    mut damage_writer: MessageWriter<PlayerDamaged>,
) {
    for (entity, position, grounded, mut tracker) in players.iter_mut() {
        let y = position.coords.y;

        if y < VOID_Y {
            trace!("Player {entity:?} is in the void");
            damage_writer.write(PlayerDamaged::from_source(
                entity,
                VOID_DAMAGE,
                DamageSource::Void,
            ));
            continue;
        }

        if grounded.0 {
            let distance = tracker.land(y);
            let chargeable = distance - SAFE_FALL_DISTANCE;
            if chargeable > 0.0 {
                let damage = chargeable.floor() as f32 * DAMAGE_PER_BLOCK;
                trace!("Player {entity:?} fell {distance} blocks, taking {damage} damage");
                damage_writer.write(PlayerDamaged::from_source(
                    entity,
                    damage,
                    DamageSource::Fall,
                ));
            }
        } else {
            tracker.track_airborne(y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fall of three blocks or less is free.
    #[test]
    fn a_short_fall_is_free() {
        let mut tracker = FallTracker::on_ground(70.0);
        tracker.track_airborne(70.0);
        let distance = tracker.land(67.5);
        assert!(distance - SAFE_FALL_DISTANCE <= 0.0);
    }

    /// A fall of ten blocks costs seven damage.
    #[test]
    fn a_long_fall_is_charged_per_block() {
        let mut tracker = FallTracker::on_ground(80.0);
        tracker.track_airborne(80.0);
        let distance = tracker.land(70.0);
        let chargeable = distance - SAFE_FALL_DISTANCE;
        let damage = chargeable.floor() as f32 * DAMAGE_PER_BLOCK;
        assert_eq!(distance, 10.0);
        assert_eq!(damage, 7.0);
    }
}
