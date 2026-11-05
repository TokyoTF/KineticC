# Player skins

A skin is not a server-side asset. The client draws a player from the `textures` profile property,
which Mojang's session server returns for premium accounts and which the server forwards to clients.
The property value is a base64 document (with an optional signature) that points at a texture URL.

## Online mode

With `online_mode = true` the login handshake asks Mojang's session server for the player's profile.
[`authenticate_user`](../src/lib/net/src/auth.rs) returns the username, UUID and profile properties.
The property `value` is kept exactly as Mojang returned it, base64 encoded, because that is the wire
format the protocol expects.

The session server is the authority on the identity of a premium player: the username and the UUID
that the server puts in the [Login Success](../src/lib/net/src/packets/outgoing/login_success.rs)
packet and in the [`PlayerIdentity`](../src/lib/core/src/identity/player_identity.rs) are the ones
Mojang returned, not the ones the client sent. The name is compared without case, because the
session server resolves names case insensitively.

The properties are delivered through two packets:

| packet | path | consumer |
| --- | --- | --- |
| Login Success | [`send_login_success`](../src/lib/net/src/conn_init/login.rs) | the joining client, for its own skin |
| Player Info Update | [`PlayerInfoUpdatePacket::add_player`](../src/lib/net/src/packets/outgoing/player_info_update.rs) | every other client, for the tab list and the in-world player |

Both writers forward `value` and `signature` without decoding or re-encoding. A base64 round trip
changes the bytes, which leaves the client unable to resolve the texture and falls back to the default
skin.

`Textures` are signed for some accounts and unsigned for others. The signature is optional on the wire
and is forwarded when Mojang supplies it; the server never verifies it. The client does verify it,
against the public keys published at `https://api.minecraftservices.com/publickeys`, and only draws a
premium skin when the signature is valid. A property without a signature is forwarded as an unsigned
property.

## Packet layout

Both packets carry the properties as a *game profile* property list, which is the shape of Mojang's
API response with the protocol encoding instead of JSON:

| field | type |
| --- | --- |
| name | string |
| value | string (the base64 texture document) |
| signature | prefixed optional string |

`Player Info Update` (protocol 772) carries them in the *add player* action, whose mask is `0x01`. The
action mask of the packet is `0x09` in this implementation, because *add player* (`0x01`) and *update
listed* (`0x08`) are both sent. The per-player action array has no length prefix: the mask tells the
client which fields to read.

## Offline mode

`online_mode = false` performs no profile lookup. Players are identified by the name their client
sent and carry no properties, so every player uses the default skin. Claiming a premium name does not
grant its skin.

## Known gaps

- No fallback skin is chosen server-side; the client decides what to draw when the property is absent.
- A skin changed on Mojang's side is picked up on the next login, not while the player is online.
- Capes and elytra textures travel in the same property and are rendered by the client; the server
  does not interpret them.

## References

- [Java Edition protocol](https://minecraft.wiki/w/Java_Edition_protocol/Packets#Player_Info_Update)
  — the packet layouts of protocol 772.
- [Mojang API](https://minecraft.wiki/w/Mojang_API#Query_player's_skin_and_cape) — the shape of the
  profile properties and the meaning of the signature.
- [Skin](https://minecraft.wiki/w/Skin#Skin_signature) — how the client validates the signature.
