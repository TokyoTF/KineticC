use bevy_ecs::prelude::{Component, Entity, Query};
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::length_prefixed_vec::LengthPrefixedVec;
use ferrumc_net_codec::net_types::prefixed_optional::PrefixedOptional;
use ferrumc_net_codec::net_types::var_int::VarInt;
use tracing::debug;

#[derive(NetEncode)]
#[packet(packet_id = "player_info_update", state = "play")]
pub struct PlayerInfoUpdatePacket {
    pub actions: u8,
    pub players: LengthPrefixedVec<PlayerWithActions>,
}

impl PlayerInfoUpdatePacket {
    pub fn with_players(players: Vec<PlayerWithActions>) -> Self {
        let players: Vec<PlayerWithActions> = players.into_iter().collect();
        Self {
            actions: players
                .iter()
                .map(|player| player.get_actions_mask())
                .fold(0, |acc, x| acc | x),
            players: LengthPrefixedVec::new(players),
        }
    }

    /// The packet to be sent to all already connected players when a new player joins the server
    pub fn new_player_join_packet(identity: &PlayerIdentity) -> Self {
        Self::with_players(vec![PlayerWithActions::add_player(identity)])
    }

    /// The packet to be sent to a new player when they join the server,
    /// To let them know about all the players that are already connected
    pub fn existing_player_info_packet(
        new_player_id: Entity,
        query: Query<(Entity, &PlayerIdentity)>,
    ) -> Self {
        let players: Vec<&PlayerIdentity> = {
            let players = query.iter().collect::<Vec<(_, _)>>();
            players
                .iter()
                // The new player already knows about itself, the packet carries everybody else.
                .filter(|&player| player.0 != new_player_id)
                .map(|player| player.1)
                .collect()
        };

        let players = players
            .into_iter()
            .map(PlayerWithActions::add_player)
            .collect::<Vec<_>>();

        debug!("Sending PlayerInfoUpdatePacket with {:?} players", players);

        Self::with_players(players)
    }

    pub fn update_players_ping(players: Vec<(u128, VarInt)>) -> Self {
        let players_with_actions = players
            .into_iter()
            .map(|(uuid, ping)| PlayerWithActions {
                uuid,
                actions: vec![PlayerAction::UpdatePing(ping)],
            })
            .collect::<Vec<_>>();

        Self::with_players(players_with_actions)
    }
}

#[derive(NetEncode, Debug, Component)]
pub struct PlayerWithActions {
    pub uuid: u128,
    pub actions: Vec<PlayerAction>,
}

impl PlayerWithActions {
    pub fn get_actions_mask(&self) -> u8 {
        let mut mask = 0;
        for action in &self.actions {
            mask |= match action {
                PlayerAction::AddPlayer { .. } => 0x01,
                PlayerAction::UpdateListed { .. } => 0x08,
                PlayerAction::UpdatePing { .. } => 0x10,
            }
        }
        mask
    }

    pub fn add_player(identity: &PlayerIdentity) -> Self {
        Self {
            uuid: identity.uuid.as_u128(),
            actions: vec![
                PlayerAction::AddPlayer {
                    name: identity.username.clone(),
                    properties: LengthPrefixedVec::new(
                        identity
                            .properties
                            .iter()
                            .map(|property| PlayerProperty {
                                name: property.name.clone(),
                                // The profile property value is already the base64 document the
                                // protocol carries; re-encoding it would send different bytes.
                                value: property.value.clone(),
                                signature: PrefixedOptional::new(property.signature.clone()),
                            })
                            .collect(),
                    ),
                },
                PlayerAction::UpdateListed { is_listed: true },
            ],
        }
    }
}

#[derive(NetEncode, Debug)]
pub enum PlayerAction {
    AddPlayer {
        name: String,
        properties: LengthPrefixedVec<PlayerProperty>,
    },
    UpdateListed {
        is_listed: bool,
    },
    UpdatePing(VarInt),
}

#[derive(NetEncode, Debug)]
pub struct PlayerProperty {
    pub name: String,
    pub value: String,
    pub signature: PrefixedOptional<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};

    /// The UUID of a premium account, used as a fixed point in the byte level tests.
    const NOTCH_UUID: u128 = 0x069a79f444e94726a5befca90e38aaf5;

    /// A `textures` property as returned by Mojang: a base64 document plus its signature.
    fn textures_property() -> ferrumc_core::identity::player_identity::PlayerProperty {
        ferrumc_core::identity::player_identity::PlayerProperty {
            name: "textures".to_string(),
            value: "eyJ0ZXh0dXJlcyI6e30=".to_string(),
            signature: Some("sig".to_string()),
        }
    }

    fn notch_identity() -> PlayerIdentity {
        PlayerIdentity::new("Notch".to_string(), NOTCH_UUID, vec![textures_property()])
    }

    #[test]
    fn a_profile_property_is_sent_verbatim() {
        let identity = PlayerIdentity::new(
            "Notch".to_string(),
            0,
            vec![ferrumc_core::identity::player_identity::PlayerProperty {
                name: "textures".to_string(),
                value: "eyJ0ZXh0dXJlcyI6e30=".to_string(),
                signature: Some("signature".to_string()),
            }],
        );

        let packet = PlayerInfoUpdatePacket::new_player_join_packet(&identity);
        let PlayerAction::AddPlayer { properties, .. } = &packet.players.data[0].actions[0] else {
            panic!("a join packet starts with AddPlayer");
        };

        assert_eq!(properties.data.len(), 1);
        assert_eq!(properties.data[0].name, "textures");
        assert_eq!(properties.data[0].value, "eyJ0ZXh0dXJlcyI6e30=");
        assert_eq!(
            properties.data[0].signature,
            PrefixedOptional::Some("signature".to_string())
        );
    }

    #[test]
    fn the_packet_matches_the_protocol_layout() {
        let packet = PlayerInfoUpdatePacket::new_player_join_packet(&notch_identity());

        let mut buf = Vec::new();
        packet
            .encode(&mut buf, &NetEncodeOpts::None)
            .expect("the packet encodes");

        // The layout of `player_info_update` (protocol 772) is:
        //   actions: unsigned byte, players: prefixed array of UUID, and for every player the
        //   fields of the actions selected by the mask, in mask order.
        let mut expected: Vec<u8> = vec![
            0x3F, // packet id
            0x09, // add player (0x01) | update listed (0x08)
            0x01, // one player
            // player UUID
            0x06, 0x9A, 0x79, 0xF4, 0x44, 0xE9, 0x47, 0x26, 0xA5, 0xBE, 0xFC, 0xA9, 0x0E, 0x38,
            0xAA, 0xF5, //
            0x05, // the length of the name
        ];
        expected.extend_from_slice(b"Notch");
        expected.push(0x01); // one property
        expected.push(0x08); // the length of the property name
        expected.extend_from_slice(b"textures");
        expected.push(0x14); // the length of the property value
        expected.extend_from_slice(b"eyJ0ZXh0dXJlcyI6e30=");
        expected.push(0x01); // the property is signed
        expected.push(0x03); // the length of the signature
        expected.extend_from_slice(b"sig");
        expected.push(0x01); // listed

        assert_eq!(buf, expected);
    }
}
