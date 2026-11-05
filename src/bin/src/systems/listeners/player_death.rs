//! Handles player deaths.
//!
//! When a player's health reaches zero:
//!
//! 1. The player is marked [`Dead`], which keeps them out of combat until they respawn.
//! 2. The death event is broadcast so every client plays the death sound and particles.
//! 3. A death message is broadcast to chat.
//!
//! The client shows the death screen on its own; the server only has to flag the entity and make
//! sure everyone else is told.

use bevy_ecs::prelude::{Commands, Entity, MessageReader, Query, Res};
use ferrumc_components::player::gameplay_state::dead::Dead;
use ferrumc_core::{identity::player_identity::PlayerIdentity, mq};
use ferrumc_messages::PlayerDied;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_event::EntityStatus;
use ferrumc_state::GlobalStateResource;
use ferrumc_text::TextComponent;
use tracing::{debug, warn};

/// Client-side entity event status code for "this entity died".
const ENTITY_EVENT_DEATH: u8 = 3;

pub fn handle(
    mut commands: Commands,
    mut events: MessageReader<PlayerDied>,
    player_query: Query<(Entity, &PlayerIdentity, &StreamWriter)>,
    state: Res<GlobalStateResource>,
) {
    for event in events.read() {
        let victim = event.player;
        let Ok((_, victim_identity, _)) = player_query.get(victim) else {
            debug!("Ignoring death for {victim:?}: the entity no longer exists");
            continue;
        };

        commands.entity(victim).insert(Dead);

        let death_packet = EntityStatus {
            entity_id: victim_identity.short_uuid,
            status: ENTITY_EVENT_DEATH,
        };

        for (entity, _, conn) in player_query.iter() {
            if entity == victim {
                continue;
            }
            if !state.0.players.is_connected(entity) {
                continue;
            }
            if let Err(err) = conn.send_packet_ref(&death_packet) {
                warn!(
                    "Failed to broadcast death of {}: {err:?}",
                    victim_identity.username
                );
            }
        }

        mq::broadcast(
            TextComponent::from(format!("{} died", victim_identity.username)),
            false,
        );

        debug!("Player {} died", victim_identity.username);
    }
}
