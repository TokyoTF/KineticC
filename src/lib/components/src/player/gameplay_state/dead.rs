use bevy_ecs::prelude::Component;

/// Marks a player whose health has reached zero and who is waiting to respawn.
///
/// While the marker is present the player is excluded from combat and interactions, and the client
/// is showing the death screen. The marker is removed by the respawn handler once the client
/// acknowledges the respawn.
#[derive(Component, Default, Debug, Clone, Copy)]
pub struct Dead;
