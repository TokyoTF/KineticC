use bevy_ecs::prelude::{Entity, Message};

/// Fired whenever a player spends energy: mining, attacking, jumping.
///
/// The hunger system accumulates these values until they exceed the drain threshold, which is how
/// vanilla models the food bar. Keeping the sources separate from the accumulation means new
/// activities only need to emit this event.
///
/// * Fired by: digging, combat, movement.
/// * Listened for by: the hunger system.
#[derive(Message)]
pub struct PlayerExhausted {
    pub player: Entity,
    /// Amount of exhaustion gained.
    pub amount: f32,
}
