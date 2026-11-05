use crate::errors::NetAuthenticationError;
use ferrumc_core::identity::player_identity::PlayerProperty;
use ferrumc_net_encryption::minecraft_hex_digest;
use serde_derive::Deserialize;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

/// Authenticates the given player with Mojang's session server.
///
/// # Parameters
/// - `username`: The username of the player to be authenticated. Should be from the login start packet.
/// - `server_id`: The server id sent in the encryption request packet. In Minecraft 1.7+, this value should be an empty string.
/// - `shared_secret`: The decrypted shared secret returned by the client in an encryption response packet.
///
/// # Returns
/// `(String, Uuid, Vec<PlayerProperty>)`: The data returned by Mojang upon successful authentication.
/// - `String`: The official username of the player from Mojang.
/// - `Uuid`: The official uuid of the player from Mojang.
/// - `Vec<PlayerProperty>`: The profile properties returned by Mojang, including the base64 texture
///   blob that carries the player's skin. The values are passed through unchanged.
///
/// # Error
/// A `NetAuthenticationError` upon an unsuccessful authentication. See the `NetAuthenticationError` enum for more information.
pub(crate) async fn authenticate_user(
    username: &str,
    server_id: &str,
    shared_secret: &[u8],
) -> Result<(String, Uuid, Vec<PlayerProperty>), NetAuthenticationError> {
    let url = format!(
        "https://sessionserver.mojang.com/session/minecraft/hasJoined?username={}&serverId={}",
        username,
        minecraft_hex_digest(server_id, shared_secret),
    );

    let response = reqwest::get(&url)
        .await
        .map_err(|_| NetAuthenticationError::CouldNotReachMojang)?;

    match response.status().as_u16() {
        200 => Ok(()),
        204 => Err(NetAuthenticationError::FailedToAuthenticate),
        404 => Err(NetAuthenticationError::BadURL),
        429 => Err(NetAuthenticationError::RateLimitReached),
        code => Err(NetAuthenticationError::UnknownStatusError(code)),
    }?;

    let response = response
        .json::<MojangAuthResponse>()
        .await
        .map_err(|err| NetAuthenticationError::ParseError(Arc::new(err)))?;

    parse_auth_response(response)
}

/// Turns the payload returned by Mojang's session server into a username, a UUID and the
/// profile properties.
///
/// The property values are kept exactly as Mojang returned them. A `textures` property is a
/// base64 document holding the URLs of the player's skin and cape, and that is the encoding the
/// protocol carries on the wire. Decoding the base64 here and re-encoding it later would make the
/// two packet paths disagree with the session server, and the client would fall back to a default
/// skin.
fn parse_auth_response(
    response: MojangAuthResponse,
) -> Result<(String, Uuid, Vec<PlayerProperty>), NetAuthenticationError> {
    let username = response.name;
    let uuid = Uuid::from_str(&response.id).map_err(|_| NetAuthenticationError::CorruptUuid)?;
    let properties: Vec<PlayerProperty> = response
        .properties
        .into_iter()
        .map(|property| PlayerProperty {
            name: property.name,
            signature: if property.signature.is_empty() {
                None
            } else {
                Some(property.signature)
            },
            value: property.value,
        })
        .collect();

    Ok((username, uuid, properties))
}

// Helper structs to decode Mojang's json response
#[derive(Deserialize)]
struct MojangAuthResponse {
    name: String,
    id: String,
    properties: Vec<MojangAuthProperty>,
}

#[derive(Deserialize, Clone)]
struct MojangAuthProperty {
    name: String,
    value: String,
    #[serde(default)]
    signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `hasJoined` payload as returned by Mojang's session server, with the base64 texture
    /// document and the signature of a premium account.
    const SIGNED_RESPONSE: &str = r#"{
        "id": "069a79f444e94726a5befca90e38aaf5",
        "name": "Notch",
        "properties": [
            {
                "name": "textures",
                "value": "eyJ0aW1lc3RhbXAiOjE3OTM2MTEwNTg2NzgsInByb2ZpbGVJZCI6IjA2OWE3OWY0NDRlOTQ3MjZhNWJlZmNhOTBlMzhhYWY1In0=",
                "signature": "c2lnbmF0dXJl"
            }
        ]
    }"#;

    /// A payload for an account without a signed texture property.
    const UNSIGNED_RESPONSE: &str = r#"{
        "id": "069a79f444e94726a5befca90e38aaf5",
        "name": "Notch",
        "properties": [
            {
                "name": "textures",
                "value": "eyJ0ZXh0dXJlcyI6e30="
            }
        ]
    }"#;

    fn parse(json: &str) -> (String, Uuid, Vec<PlayerProperty>) {
        let response: MojangAuthResponse =
            serde_json::from_str(json).expect("the payload is valid json");

        parse_auth_response(response).expect("the payload holds a valid uuid")
    }

    #[test]
    fn the_texture_property_is_forwarded_verbatim() {
        let (username, uuid, properties) = parse(SIGNED_RESPONSE);

        assert_eq!(username, "Notch");
        assert_eq!(
            uuid,
            Uuid::from_str("069a79f444e94726a5befca90e38aaf5").expect("a valid uuid")
        );
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].name, "textures");
        assert_eq!(
            properties[0].value,
            "eyJ0aW1lc3RhbXAiOjE3OTM2MTEwNTg2NzgsInByb2ZpbGVJZCI6IjA2OWE3OWY0NDRlOTQ3MjZhNWJlZmNhOTBlMzhhYWY1In0="
        );
        assert_eq!(properties[0].signature, Some("c2lnbmF0dXJl".to_string()));
    }

    #[test]
    fn a_missing_signature_becomes_no_signature() {
        let (_, _, properties) = parse(UNSIGNED_RESPONSE);

        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].value, "eyJ0ZXh0dXJlcyI6e30=");
        assert_eq!(properties[0].signature, None);
    }
}
