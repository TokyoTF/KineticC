use bevy_ecs::prelude::{Entity, Message};

/// What caused a damage event.
///
/// The source determines knockback direction, death messages and which protections apply, so it is
/// carried next to the damage amount rather than being inferred later by the health system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DamageSource {
    /// Another entity hit the player in melee.
    Melee { attacker: Entity },
    /// Fall damage. The amount is expected to be pre-computed from the fall distance.
    Fall,
    /// Starvation from an empty food bar.
    Starvation,
    /// Falling out of the world.
    Void,
    /// Drowning.
    Drowning,
    /// Landing a thrown ender pearl (`ThrownEnderpearl` deals this to its owner).
    EnderPearl,
    /// Any cause that is not modelled yet, including environmental hazards.
    Generic,
}

/// Fired when a player should take damage.
///
///  * Fired by: Physics (fall damage), Hunger System (starvation), Combat.
///  * Listened for by: A `health_system` that will decrease the `Health` component.
#[derive(Message)]
pub struct PlayerDamaged {
    pub player: Entity,
    pub amount: f32,
    pub source: DamageSource,
    /// Direction the hit came from, in degrees, as used by the hurt animation packet.
    pub yaw: f32,
}

impl PlayerDamaged {
    /// Creates a melee damage event caused by `attacker`, with the direction of the blow.
    pub fn melee(player: Entity, attacker: Entity, amount: f32, yaw: f32) -> Self {
        Self {
            player,
            amount,
            source: DamageSource::Melee { attacker },
            yaw,
        }
    }

    /// Creates a damage event with no attacker and no incoming direction.
    pub fn from_source(player: Entity, amount: f32, source: DamageSource) -> Self {
        Self {
            player,
            amount,
            source,
            yaw: 0.0,
        }
    }

    /// The entity that caused the damage, when the damage came from another entity.
    pub fn attacker(&self) -> Option<Entity> {
        match self.source {
            DamageSource::Melee { attacker } => Some(attacker),
            _ => None,
        }
    }
}

/// Fired when the `health_system` sees a player's health reach zero.
///
///  * Fired by: `health_system`.
///  * Listened for by: `respawn_system`, `player_leave_system` (to broadcast death msg).
#[derive(Message)]
pub struct PlayerDied {
    pub player: Entity,
    /// The entity that caused the death, when the killer is known.
    pub killer: Option<Entity>,
}
