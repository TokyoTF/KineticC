use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::length_prefixed_vec::LengthPrefixedVec;
use ferrumc_net_codec::net_types::var_int::VarInt;

#[derive(NetEncode)]
#[packet(packet_id = "remove_entities", state = "play")]
pub struct RemoveEntitiesPacket {
    pub entity_ids: LengthPrefixedVec<VarInt>,
}

impl RemoveEntitiesPacket {
    pub fn from_entities<T>(entity_ids: T) -> Self
    where
        T: IntoIterator<Item = PlayerIdentity>,
    {
        let entity_ids: Vec<VarInt> = entity_ids
            .into_iter()
            .map(|entity| VarInt::new(entity.short_uuid))
            .collect();
        Self {
            entity_ids: LengthPrefixedVec::new(entity_ids),
        }
    }

    /// Builds the packet from raw network entity ids, which is what non-player entities carry.
    pub fn from_entity_ids<T>(entity_ids: T) -> Self
    where
        T: IntoIterator<Item = i32>,
    {
        let entity_ids: Vec<VarInt> = entity_ids.into_iter().map(VarInt::new).collect();
        Self {
            entity_ids: LengthPrefixedVec::new(entity_ids),
        }
    }
}
