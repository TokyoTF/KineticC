//! Play Sound packet (`ClientboundSoundPacket`).
//!
//! This is the server-side half of vanilla's `Level.playSound(null, x, y, z, ...)`: it is what
//! makes world actions audible, from throwing a snowball (`entity.snowball.throw` at the
//! thrower) to landing an ender pearl (`entity.player.teleport` where the pearl lands). The
//! sound goes out as a registry reference — its protocol id plus one, because
//! `ByteBufCodecs.holder` writes `id + 1` for a registry reference and `0` for an inline sound
//! definition — and the position in eighths of a block (`LOCATION_ACCURACY = 8.0`).

use ferrumc_macros::{NetEncode, packet};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// Sound categories, in `SoundSource` ordinal order; sent as a VarInt of the ordinal
/// (`buffer.writeEnum`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SoundSource {
    Master = 0,
    Music = 1,
    Records = 2,
    Weather = 3,
    Blocks = 4,
    Hostile = 5,
    Neutral = 6,
    Players = 7,
    Ambient = 8,
    Voice = 9,
    Ui = 10,
}

#[derive(NetEncode)]
#[packet(packet_id = "sound", state = "play")]
pub struct PlaySoundPacket {
    /// Registry id of the sound event, plus one.
    pub sound: VarInt,
    /// The category, as its `SoundSource` ordinal.
    pub source: VarInt,
    /// Position in eighths of a block.
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub volume: f32,
    pub pitch: f32,
    /// Seed of the played sound instance; vanilla takes it from the level's random.
    pub seed: i64,
}

impl PlaySoundPacket {
    /// Builds a packet for the registry sound event `sound_event_id` played at a world position.
    pub fn new(
        sound_event_id: u32,
        source: SoundSource,
        x: f64,
        y: f64,
        z: f64,
        volume: f32,
        pitch: f32,
    ) -> Self {
        Self {
            sound: VarInt::new(sound_event_id as i32 + 1),
            source: VarInt::new(source as i32),
            x: (x * 8.0) as i32,
            y: (y * 8.0) as i32,
            z: (z * 8.0) as i32,
            volume,
            pitch,
            seed: rand::random(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_sound_is_sent_as_id_plus_one() {
        // `ByteBufCodecs.holder` encodes a registry reference as `id + 1`; `0` would mean an
        // inline sound definition follows.
        let packet = PlaySoundPacket::new(520, SoundSource::Neutral, 0.0, 64.0, 0.0, 0.5, 1.0);
        assert_eq!(packet.sound, VarInt::new(521));
        assert_eq!(packet.source, VarInt::new(SoundSource::Neutral as i32));
    }

    #[test]
    fn the_position_is_in_eighths_of_a_block() {
        let packet = PlaySoundPacket::new(1, SoundSource::Players, 1.5, -64.0, 0.25, 1.0, 1.0);
        assert_eq!(packet.x, 12);
        assert_eq!(packet.y, -512);
        assert_eq!(packet.z, 2);
    }
}
