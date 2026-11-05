//! Handles spawning players for each other when they join the server.
//!
//! When a player joins:
//! 1. Send existing players' info + spawn packets to the new player
//! 2. Broadcast the new player's info + spawn packets to existing players

use bevy_ecs::prelude::{Entity, MessageReader, Query, Res};
use ferrumc_components::player::client_information::ClientInformationComponent;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::{position::Position, rotation::Rotation};
use ferrumc_macros::get_registry_entry;
use ferrumc_messages::player_join::PlayerJoined;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net::packets::outgoing::player_info_update::PlayerInfoUpdatePacket;
use ferrumc_net::packets::outgoing::spawn_entity::SpawnEntityPacket;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use tracing::{error, trace};

const PLAYER_TYPE_ID: i32 =
    get_registry_entry!("minecraft:entity_type.entries.minecraft:player") as i32;

/// Listens for `PlayerJoined` events and handles spawning players for each other.
pub fn handle(
    mut events: MessageReader<PlayerJoined>,
    player_query: Query<(
        Entity,
        &PlayerIdentity,
        &Position,
        &Rotation,
        &StreamWriter,
        &ClientInformationComponent,
    )>,
    state: Res<GlobalStateResource>,
) {
    for event in events.read() {
        let new_player_entity = event.entity;
        let new_player_identity = &event.identity;

        // Get the new player's connection and components
        let Ok((_, _, new_pos, new_rot, new_conn, new_client_info)) =
            player_query.get(new_player_entity)
        else {
            error!(
                "Failed to get new player components for spawn broadcast: {:?}",
                new_player_entity
            );
            continue;
        };

        // Create packets for the new player once (to broadcast to existing players)
        let new_player_info_packet =
            PlayerInfoUpdatePacket::new_player_join_packet(new_player_identity);
        let new_player_spawn_packet = SpawnEntityPacket::new(
            new_player_identity.short_uuid,
            new_player_identity.uuid.as_u128(),
            PLAYER_TYPE_ID,
            new_pos,
            new_rot,
        );
        // The player model customisation byte (metadata index 17) defaults to 0 on the
        // client, which hides the cape and the second skin layer; send the joining player
        // its own byte (from its client information) so its cape renders for itself too.
        let new_player_metadata_packet = EntityMetadataPacket::new(
            VarInt::new(new_player_identity.short_uuid),
            [EntityMetadata::player_model_customisation(
                new_client_info.displayed_skin_parts,
            )],
        );
        if let Err(e) = new_conn.send_packet_ref(&new_player_metadata_packet) {
            error!(
                "Failed to send own model customisation to new player: {:?}",
                e
            );
        }

        let mut spawned_for_new_player = 0;
        let mut spawned_for_existing = 0;

        for (entity, identity, pos, rot, conn, client_info) in player_query.iter() {
            // Skip self
            if entity == new_player_entity {
                continue;
            }

            // Skip disconnected players
            if !state.0.players.is_connected(entity) {
                continue;
            }

            // 1. Send existing player's info to the new player
            // PlayerInfoUpdate MUST come before SpawnEntity (protocol requirement)
            let existing_player_info = PlayerInfoUpdatePacket::new_player_join_packet(identity);
            if let Err(e) = new_conn.send_packet_ref(&existing_player_info) {
                error!("Failed to send existing player info to new player: {:?}", e);
                continue;
            }

            // 2. Send existing player's spawn packet to the new player
            let existing_player_spawn = SpawnEntityPacket::new(
                identity.short_uuid,
                identity.uuid.as_u128(),
                PLAYER_TYPE_ID,
                pos,
                rot,
            );
            if let Err(e) = new_conn.send_packet_ref(&existing_player_spawn) {
                error!(
                    "Failed to send existing player spawn to new player: {:?}",
                    e
                );
                continue;
            }

            // 2b. Send existing player's model customisation (cape bit) to the new player,
            // after the spawn packet so the entity exists client-side
            let existing_player_metadata = EntityMetadataPacket::new(
                VarInt::new(identity.short_uuid),
                [EntityMetadata::player_model_customisation(
                    client_info.displayed_skin_parts,
                )],
            );
            if let Err(e) = new_conn.send_packet_ref(&existing_player_metadata) {
                error!(
                    "Failed to send existing player metadata to new player: {:?}",
                    e
                );
            }
            spawned_for_new_player += 1;

            // 3. Send new player's info to existing player
            if let Err(e) = conn.send_packet_ref(&new_player_info_packet) {
                error!("Failed to send new player info to existing player: {:?}", e);
                continue;
            }

            // 4. Send new player's spawn packet to existing player
            if let Err(e) = conn.send_packet_ref(&new_player_spawn_packet) {
                error!(
                    "Failed to send new player spawn to existing player: {:?}",
                    e
                );
                continue;
            }

            // 5. Send new player's model customisation (cape bit) to the existing player
            if let Err(e) = conn.send_packet_ref(&new_player_metadata_packet) {
                error!(
                    "Failed to send new player metadata to existing player: {:?}",
                    e
                );
            }
            spawned_for_existing += 1;
        }

        trace!(
            "Player {} joined: sent {} existing players, spawned for {} existing players",
            new_player_identity.username, spawned_for_new_player, spawned_for_existing
        );
    }
}
