//! Marker for a player currently gliding with an elytra.

use bevy_ecs::prelude::Component;

/// Present while the player is fall-flying (elytra glide).
///
/// The client reports the start of a glide with a `StartFlyingWithElytra` player command and the
/// server keeps this marker until the player reports being on the ground again. It lets the
/// server answer vanilla's `Player.isFallFlying()` check, which gates firework rocket boosts.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct FallFlying;
