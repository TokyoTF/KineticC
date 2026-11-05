use ferrumc_core::transform::velocity::Velocity;
use ferrumc_macros::{NetEncode, packet};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// `ClientboundSetEntityMotionPacket`: `VarInt entity id` followed by three `i16` velocities.
///
/// Vanilla quantizes the velocity as `clamp(v, -3.9, 3.9) * 8000`
/// (`ClientboundSetEntityMotionPacket` constructor, `MAGICAL_QUANTIZATION = 8000.0`, `LIMIT = 3.9`).
/// The server sends it when an entity starts being tracked and whenever its velocity changes
/// (`ServerEntity.sendChanges` sends when `distanceToSqr(lastSentMovement) > 1.0E-7`).
#[derive(NetEncode)]
#[packet(packet_id = "set_entity_motion", state = "play")]
pub struct SetEntityMotionPacket {
    pub entity_id: VarInt,
    pub velocity_x: i16,
    pub velocity_y: i16,
    pub velocity_z: i16,
}

impl SetEntityMotionPacket {
    pub fn new(entity_id: i32, velocity: &Velocity) -> Self {
        Self {
            entity_id: VarInt::new(entity_id),
            velocity_x: encode_velocity(velocity.x),
            velocity_y: encode_velocity(velocity.y),
            velocity_z: encode_velocity(velocity.z),
        }
    }
}

/// The protocol's fixed-point velocity encoding shared with `SpawnEntityPacket`:
/// `clamp(v, -3.9, 3.9) * 8000`, truncated to an `i16` exactly like vanilla's `(int)(...)`.
pub fn encode_velocity(value: f32) -> i16 {
    (f64::from(value).clamp(-3.9, 3.9) * 8000.0) as i16
}
