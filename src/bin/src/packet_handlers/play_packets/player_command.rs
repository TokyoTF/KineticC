use bevy_ecs::prelude::{Commands, Entity, Query, Res};
use ferrumc_components::player::fall_flying::FallFlying;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_net::PlayerCommandPacketReceiver;
use ferrumc_net::broadcast::broadcast_packet_except;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::incoming::player_command::PlayerCommandAction;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net_codec::net_types::var_int::VarInt;
use tracing::log::trace;

/// Handles PlayerCommand packets (sprinting, leave bed, etc.)
/// Note: Sneaking is handled via PlayerInput packet, NOT here
pub fn handle(
    receiver: Res<PlayerCommandPacketReceiver>,
    conn_query: Query<(Entity, &StreamWriter)>,
    identity_query: Query<&PlayerIdentity>,
    mut commands: Commands,
) {
    for (event, eid) in receiver.0.try_iter() {
        // Get the sender's identity to use the correct entity ID
        let Ok(identity) = identity_query.get(eid) else {
            continue;
        };

        let entity_id = VarInt::new(identity.short_uuid);

        trace!(
            "PlayerCommand: {:?} from {} (entity_id={})",
            event.action, identity.username, identity.short_uuid
        );

        match event.action {
            PlayerCommandAction::StartSprinting => {
                let packet =
                    EntityMetadataPacket::new(entity_id, [EntityMetadata::entity_sprinting()]);
                broadcast_packet_except(eid, &packet, conn_query.iter());
            }
            PlayerCommandAction::StopSprinting => {
                let packet =
                    EntityMetadataPacket::new(entity_id, [EntityMetadata::entity_clear_state()]);
                broadcast_packet_except(eid, &packet, conn_query.iter());
            }
            PlayerCommandAction::StartFlyingWithElytra => {
                // The client owns its glide state; the marker lets the server answer vanilla's
                // `isFallFlying()` check (firework rockets only boost while gliding). It is
                // cleared by `fall_flying::clear_when_grounded` once the player lands.
                commands.entity(eid).insert(FallFlying);
                // Broadcast the fall-flying pose so other clients (and the player's own third-person
                // view) show the glide animation. Without this the pose is never set on the wire.
                let packet = EntityMetadataPacket::new(
                    entity_id,
                    [
                        EntityMetadata::entity_fall_flying(),
                        EntityMetadata::entity_flying_with_elytra(),
                    ],
                );
                broadcast_packet_except(eid, &packet, conn_query.iter());
            }
            _ => {}
        }
    }
}
