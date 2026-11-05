use crate::errors::NetAuthenticationError;
use ferrumc_core::identity::player_identity::PlayerProperty;
use ferrumc_net_encryption::minecraft_hex_digest;
use serde_derive::Deserialize;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
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

/// Looks up a premium account by username and returns its signed profile properties.
///
/// The client renders another player's cape (and skin) only when the `textures` property
/// forwarded in the player info update carries a valid Mojang signature, and only Mojang
/// can produce that signature. Players authenticated through the session server handshake
/// already carry it; for everyone else (offline-mode logins) the profile is fetched here by
/// username so a premium account still gets its own skin and cape on the server.
///
/// Returns `Ok(None)` when no premium account exists for the username.
pub(crate) async fn fetch_profile_properties(
    username: &str,
) -> Result<Option<Vec<PlayerProperty>>, NetAuthenticationError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| NetAuthenticationError::CouldNotReachMojang)?;

    // username -> undashed account uuid
    let lookup_url = format!("https://api.mojang.com/users/profiles/minecraft/{username}");
    let response = client
        .get(&lookup_url)
        .send()
        .await
        .map_err(|_| NetAuthenticationError::CouldNotReachMojang)?;

    match response.status().as_u16() {
        200 => {}
        204 | 404 => return Ok(None),
        429 => return Err(NetAuthenticationError::RateLimitReached),
        code => return Err(NetAuthenticationError::UnknownStatusError(code)),
    }

    let lookup: MojangNameLookup = response
        .json()
        .await
        .map_err(|err| NetAuthenticationError::ParseError(Arc::new(err)))?;

    // uuid -> full profile including the signed properties
    let profile_url = format!(
        "https://sessionserver.mojang.com/session/minecraft/profile/{}?unsigned=false",
        lookup.id
    );
    let response = client
        .get(&profile_url)
        .send()
        .await
        .map_err(|_| NetAuthenticationError::CouldNotReachMojang)?;

    match response.status().as_u16() {
        200 => {}
        204 | 404 => return Ok(None),
        429 => return Err(NetAuthenticationError::RateLimitReached),
        code => return Err(NetAuthenticationError::UnknownStatusError(code)),
    }

    let profile: MojangAuthResponse = response
        .json()
        .await
        .map_err(|err| NetAuthenticationError::ParseError(Arc::new(err)))?;

    Ok(Some(properties_of(profile.properties)))
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

    Ok((username, uuid, properties_of(response.properties)))
}

/// Maps Mojang's raw property list to protocol properties, keeping the base64 `textures`
/// value and its signature exactly as Mojang returned them.
fn properties_of(properties: Vec<MojangAuthProperty>) -> Vec<PlayerProperty> {
    properties
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
        .collect()
}

// Helper structs to decode Mojang's json response
#[derive(Deserialize)]
struct MojangAuthResponse {
    name: String,
    id: String,
    properties: Vec<MojangAuthProperty>,
}

/// The `api.mojang.com/users/profiles/minecraft/{name}` response: just the undashed uuid.
#[derive(Deserialize)]
struct MojangNameLookup {
    id: String,
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

    /// A `sessionserver.mojang.com/session/minecraft/profile/{uuid}?unsigned=false` payload
    /// whose `textures` document carries both a skin and a cape, signed by Mojang.
    const PROFILE_RESPONSE: &str = r#"{
        "id": "069a79f444e94726a5befca90e38aaf5",
        "name": "Notch",
        "properties": [
            {
                "name": "textures",
                "value": "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvMTIzIn0sIkNBUEUiOnsidXJsIjoiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS9jYXBlIn19",
                "signature": "c2lnbmF0dXJl"
            }
        ]
    }"#;

    /// An `api.mojang.com/users/profiles/minecraft/{name}` payload.
    const NAME_LOOKUP_RESPONSE: &str = r#"{"id":"069a79f444e94726a5befca90e38aaf5","name":"Notch"}"#;

    #[test]
    fn a_name_lookup_yields_the_undashed_uuid() {
        let lookup: MojangNameLookup =
            serde_json::from_str(NAME_LOOKUP_RESPONSE).expect("the payload is valid json");

        assert_eq!(lookup.id, "069a79f444e94726a5befca90e38aaf5");
    }

    #[test]
    fn the_signed_cape_texture_property_survives_the_profile_response() {
        let profile: MojangAuthResponse =
            serde_json::from_str(PROFILE_RESPONSE).expect("the payload is valid json");

        let properties = properties_of(profile.properties);

        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].name, "textures");
        // The base64 document must travel verbatim; the client unpacks the cape URL from it.
        assert_eq!(
            properties[0].value,
            "eyJ0ZXh0dXJlcyI6eyJTS0lOIjp7InVybCI6Imh0dHA6Ly90ZXh0dXJlcy5taW5lY3JhZnQubmV0L3RleHR1cmUvMTIzIn0sIkNBUEUiOnsidXJsIjoiaHR0cDovL3RleHR1cmVzLm1pbmVjcmFmdC5uZXQvdGV4dHVyZS9jYXBlIn19"
        );
        assert_eq!(properties[0].signature, Some("c2lnbmF0dXJl".to_string()));
    }
}
