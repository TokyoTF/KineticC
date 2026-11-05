use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::length_prefixed_vec::LengthPrefixedVec;
use ferrumc_net_codec::net_types::prefixed_optional::PrefixedOptional;

#[derive(NetEncode)]
#[packet(packet_id = "login_finished", state = "login")]
pub struct LoginSuccessPacket<'a> {
    pub uuid: u128,
    pub username: &'a str,
    pub properties: LengthPrefixedVec<LoginSuccessProperties<'a>>,
}

impl<'a> LoginSuccessPacket<'a> {
    pub fn new(uuid: u128, username: &'a str) -> Self {
        Self {
            uuid,
            username,
            properties: LengthPrefixedVec::new(vec![]),
        }
    }
}

#[derive(NetEncode, Clone)]
pub struct LoginSuccessProperties<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub signature: PrefixedOptional<&'a str>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrumc_core::identity::player_identity::PlayerProperty;
    use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};

    #[test]
    fn the_packet_matches_the_protocol_layout() {
        let properties = [PlayerProperty {
            name: "textures".to_string(),
            value: "eyJ0ZXh0dXJlcyI6e30=".to_string(),
            signature: Some("sig".to_string()),
        }];

        let packet = LoginSuccessPacket {
            uuid: 0x069a79f444e94726a5befca90e38aaf5,
            username: "Notch",
            properties: LengthPrefixedVec::new(
                properties
                    .iter()
                    .map(|property| LoginSuccessProperties {
                        name: &property.name,
                        value: &property.value,
                        signature: PrefixedOptional::new(property.signature.as_deref()),
                    })
                    .collect(),
            ),
        };

        let mut buf = Vec::new();
        packet
            .encode(&mut buf, &NetEncodeOpts::None)
            .expect("the packet encodes");

        // The layout of `login_success` (protocol 772) is:
        //   uuid: 16 bytes, username: string, properties: prefixed array of game profile
        //   properties, and every property is a name, a value and an optional signature.
        let mut expected: Vec<u8> = vec![
            0x02, // packet id
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

        assert_eq!(buf, expected);
    }
}
