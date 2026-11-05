#![expect(clippy::type_complexity)]
use bevy_ecs::prelude::{MessageReader, Query};
use bevy_math::Vec3A;
use ferrumc_core::identity::entity_identity::EntityIdentity;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_entities::LastSyncedPosition;
use ferrumc_messages::entity_update::SendEntityUpdate;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::entity_position_sync::TeleportEntityPacket;
use ferrumc_net::packets::outgoing::set_entity_motion::SetEntityMotionPacket;
use ferrumc_net::packets::outgoing::update_entity_position_and_rotation::UpdateEntityPositionAndRotationPacket;
use ferrumc_net_codec::net_types::angle::NetAngle;
use tracing::{debug, warn};

pub fn handle(
    mut query: Query<(
        &Position,
        &Velocity,
        &Rotation,
        &mut LastSyncedPosition,
        Option<&EntityIdentity>,
        Option<&PlayerIdentity>,
        &OnGround,
    )>,
    mut conn_query: Query<&StreamWriter>,
    mut reader: MessageReader<SendEntityUpdate>,
) {
    let mut entities_to_update = vec![];
    for msg in reader.read() {
        entities_to_update.push(msg.0);
    }
    for entity in entities_to_update {
        if let Ok((pos, vel, rot, mut last_synced, entity_id_opt, player_id_opt, grounded)) =
            query.get_mut(entity)
        {
            let id = if let Some(entity_id) = entity_id_opt {
                entity_id.entity_id
            } else if let Some(player_id) = player_id_opt {
                player_id.short_uuid
            } else {
                warn!(
                    "Tried to send entity update for entity without identity: {:?}",
                    entity
                );
                continue;
            };
            if last_synced.0.distance(pos.coords) >= 8.0 {
                let packet = TeleportEntityPacket {
                    entity_id: id.into(),
                    x: pos.x,
                    y: pos.y,
                    z: pos.z,
                    vel_x: vel.x as f64,
                    vel_y: vel.y as f64,
                    vel_z: vel.z as f64,
                    yaw: rot.yaw,
                    pitch: rot.pitch,
                    on_ground: grounded.0,
                };
                for conn in conn_query.iter_mut() {
                    // TODO: Only send if the client is tracking this entity
                    if let Err(e) = conn.send_packet_ref(&packet) {
                        warn!(
                            "Failed to send teleport packet for entity {:?}: {:?}",
                            entity, e
                        );
                    }
                }
            } else {
                let (delta_x, delta_y, delta_z) = {
                    let delta = pos.coords - last_synced.0;
                    (
                        (delta.x * 4096.0).round() as i16,
                        (delta.y * 4096.0).round() as i16,
                        (delta.z * 4096.0).round() as i16,
                    )
                };
                let packet = UpdateEntityPositionAndRotationPacket {
                    entity_id: id.into(),
                    delta_x,
                    delta_y,
                    delta_z,
                    yaw: NetAngle::from_degrees(rot.yaw.into()),
                    pitch: NetAngle::from_degrees(rot.pitch.into()),
                    on_ground: grounded.0,
                };
                for conn in conn_query.iter_mut() {
                    // TODO: Only send if the client is tracking this entity
                    if let Err(e) = conn.send_packet_ref(&packet) {
                        warn!(
                            "Failed to send entity update packet for entity {:?}: {:?}",
                            entity, e
                        );
                    }
                }
                // Vanilla `ServerEntity.sendChanges` sends `ClientboundSetEntityMotionPacket`
                // whenever the velocity changed since the last send (for a falling item that is
                // every tick). A moving entity keeps its client-side velocity in sync this way;
                // a resting one (zero velocity) sends none.
                if **vel != Vec3A::ZERO {
                    let motion_packet = SetEntityMotionPacket::new(id, vel);
                    for conn in conn_query.iter_mut() {
                        if let Err(e) = conn.send_packet_ref(&motion_packet) {
                            warn!(
                                "Failed to send entity motion packet for entity {:?}: {:?}",
                                entity, e
                            );
                        }
                    }
                }
            };
            *last_synced = LastSyncedPosition(pos.coords);
        } else {
            // Legitimate when the entity was removed after the update was queued (picked up,
            // merged, expired): updates are read one tick after they are written, and by then the
            // entity may be gone. Vanilla's ServerEntity likewise sends nothing for removed
            // entities, so skip quietly instead of warning.
            debug!("Skipping entity update for removed entity: {entity:?}");
        }
    }
}
