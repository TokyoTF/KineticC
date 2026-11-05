//! Keeps the elytra fall-flying marker in step with the ground.

use bevy_ecs::prelude::{Commands, Entity, Query, With};
use ferrumc_components::player::fall_flying::FallFlying;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_metadata::{EntityMetadata, EntityMetadataPacket};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// Removes [`FallFlying`] once the player reports being on the ground again, and broadcasts the
/// standing pose plus a cleared elytra bit so the client (and everyone else) stops showing the
/// glide animation. Sending only the pose left the 0x80 "flying with elytra" shared flag set, which
/// is what actually drives the animation, so the player stayed stuck gliding on the ground.
pub fn clear_when_grounded(
    mut commands: Commands,
    players: Query<(Entity, &OnGround), With<FallFlying>>,
    identity_query: Query<&PlayerIdentity>,
    conn_query: Query<&StreamWriter>,
) {
    for (entity, ground) in players.iter() {
        if !ground.0 {
            continue;
        }
        if let Ok(identity) = identity_query.get(entity) {
            let entity_id = VarInt::new(identity.short_uuid);
            // Pose back to standing...
            let pose_packet = EntityMetadataPacket::new(
                entity_id,
                [EntityMetadata::entity_standing()],
            );
            // ...and clear the elytra flying bit (0x80) in the entity state byte. Clearing the
            // whole byte mirrors how StopSprinting already behaves and avoids leaving the bit set.
            let state_packet = EntityMetadataPacket::new(
                entity_id,
                [EntityMetadata::entity_clear_state()],
            );
            for conn in conn_query.iter() {
                let _ = conn.send_packet_ref(&pose_packet);
                let _ = conn.send_packet_ref(&state_packet);
            }
        }
        commands.entity(entity).remove::<FallFlying>();
    }
}
