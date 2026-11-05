use crate::item::ItemID;
use bitcode_derive::{Decode, Encode};
use ferrumc_net_codec::decode::errors::NetDecodeError;
use ferrumc_net_codec::decode::{NetDecode, NetDecodeOpts};
use ferrumc_net_codec::encode::errors::NetEncodeError;
use ferrumc_net_codec::encode::{NetEncode, NetEncodeOpts};
use ferrumc_net_codec::net_types::var_int::VarInt;
use std::fmt::Display;
use std::io::{Read, Write};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, Hash, Default, PartialEq, Decode, Encode)]
pub struct InventorySlot {
    pub count: VarInt,
    pub item_id: Option<ItemID>,
    pub components_to_add_count: Option<VarInt>,
    pub components_to_remove_count: Option<VarInt>,
    pub components_to_add: Option<Vec<VarInt>>,
    pub components_to_remove: Option<Vec<VarInt>>,
    // https://minecraft.wiki/w/Java_Edition_protocol/Slot_data
}

impl InventorySlot {
    pub fn empty() -> Self {
        Self {
            count: VarInt(0),
            item_id: None,
            components_to_add_count: None,
            components_to_add: None,
            components_to_remove: None,
            components_to_remove_count: None,
        }
    }
}

impl Display for InventorySlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "InventorySlot {{ count: {}, item_id: {:?}, components_to_add_count: {:?}, components_to_remove_count: {:?} }}",
            self.count.0,
            self.item_id,
            self.components_to_add_count,
            self.components_to_remove_count
        )
    }
}

impl NetDecode for InventorySlot {
    fn decode<R: Read>(reader: &mut R, opts: &NetDecodeOpts) -> Result<Self, NetDecodeError> {
        let count = VarInt::decode(reader, opts)?;
        // Vanilla treats any count <= 0 as an empty slot and reads nothing further
        // (ItemStack.OPTIONAL_STREAM_CODEC: `if (i <= 0) return ItemStack.EMPTY`).
        if count.0 <= 0 {
            Ok(Self {
                count,
                ..Default::default()
            })
        } else {
            let item_id = VarInt::decode(reader, opts)?;
            let components_to_add_count = VarInt::decode(reader, opts)?;
            let components_to_remove_count = VarInt::decode(reader, opts)?;

            let components_to_add = {
                let mut components = Vec::with_capacity(components_to_add_count.0 as usize);
                for _ in 0..components_to_add_count.0 {
                    components.push(VarInt::decode(reader, opts)?);
                }
                Some(components)
            };
            let components_to_remove = {
                let mut components = Vec::with_capacity(components_to_remove_count.0 as usize);
                for _ in 0..components_to_remove_count.0 {
                    components.push(VarInt::decode(reader, opts)?);
                }
                Some(components)
            };

            Ok(Self {
                count,
                item_id: Some(ItemID(item_id)),
                components_to_add_count: Some(components_to_add_count),
                components_to_remove_count: Some(components_to_remove_count),
                components_to_add,
                components_to_remove,
            })
        }
    }

    async fn decode_async<R: AsyncRead + Unpin>(
        _reader: &mut R,
        _opts: &NetDecodeOpts,
    ) -> Result<Self, NetDecodeError> {
        todo!()
    }
}

impl NetEncode for InventorySlot {
    fn encode<W: Write>(&self, writer: &mut W, opts: &NetEncodeOpts) -> Result<(), NetEncodeError> {
        // 1. Always encode the count
        self.count.encode(writer, opts)?;

        // Vanilla treats any count <= 0 as an empty slot and reads nothing further
        // (ItemStack.OPTIONAL_STREAM_CODEC: `if (i <= 0) return ItemStack.EMPTY`), so a
        // negative count must stop here too or the extra bytes would desynchronize the
        // rest of the packet on the client.
        if self.count.0 <= 0 {
            return Ok(());
        }

        // 2. Encode ItemID (0 = minecraft:air when the slot has a count but no item)
        match &self.item_id {
            Some(item_id) => item_id.0.encode(writer, opts)?,
            None => VarInt::new(0).encode(writer, opts)?,
        }

        // 3. Encode the component patch counts.
        //
        // This model only stores component *type* ids, never their values, but vanilla's
        // DataComponentPatch.STREAM_CODEC writes a type id **plus the value bytes** for every
        // added component. Emitting a non-zero add/remove count here would produce a
        // truncated patch: the client would read the following bytes as component values and
        // fail to decode the whole packet ("Failed to decode packet
        // 'clientbound/minecraft:container_set_content'"). So the patch is always sent
        // empty, which is decodable regardless of what is stored.
        VarInt::new(0).encode(writer, opts)?; // components_to_add_count
        VarInt::new(0).encode(writer, opts)?; // components_to_remove_count

        Ok(())
    }

    async fn encode_async<W: AsyncWrite + Unpin>(
        &self,
        _writer: &mut W,
        _opts: &NetEncodeOpts,
    ) -> Result<(), NetEncodeError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferrumc_net_codec::encode::NetEncodeOpts;
    use ferrumc_net_codec::net_types::var_int::VarInt;
    use std::io::Cursor;

    // This helper function runs the encode/decode cycle
    fn run_roundtrip_test(slot_in: &InventorySlot) -> InventorySlot {
        let mut buffer = Vec::new();

        // Create both types of options explicitly.
        let encode_opts = NetEncodeOpts::default();
        let decode_opts = NetDecodeOpts::default();

        // 1. Encode
        slot_in
            .encode(&mut buffer, &encode_opts)
            .expect("Encode failed");

        // 2. Decode
        let mut reader = Cursor::new(&buffer);
        let slot_out = InventorySlot::decode(&mut reader, &decode_opts).expect("Decode failed");

        // 3. Check that all bytes were read
        assert_eq!(
            reader.position() as usize,
            buffer.len(),
            "Decoder did not read the entire buffer"
        );

        slot_out
    }

    #[test]
    fn test_slot_encode_decode_roundtrip() {
        // A slot without components round-trips unchanged.
        let simple_slot = InventorySlot {
            count: VarInt::new(10),
            item_id: Some(ItemID::new(1)),
            components_to_add_count: Some(VarInt::new(0)),
            components_to_remove_count: Some(VarInt::new(0)),
            components_to_add: Some(vec![]),
            components_to_remove: Some(vec![]),
        };

        let decoded_simple = run_roundtrip_test(&simple_slot);
        assert_eq!(simple_slot, decoded_simple, "Simple slot roundtrip failed");
    }

    /// Vanilla layout of a slot: count VarInt; if count <= 0 nothing more; otherwise item id
    /// VarInt, then the component patch counts (add, remove). See ItemStack.OPTIONAL_STREAM_CODEC
    /// and DataComponentPatch.STREAM_CODEC in 1.21.8.
    #[test]
    fn test_empty_slot_encodes_as_single_zero() {
        let mut buffer = Vec::new();
        InventorySlot::empty()
            .encode(&mut buffer, &NetEncodeOpts::default())
            .unwrap();
        assert_eq!(buffer, vec![0x00]);
    }

    /// A negative count is an empty slot for the client and must not be followed by anything,
    /// otherwise the rest of the packet desynchronizes (count <= 0, not just == 0).
    #[test]
    fn test_negative_count_encodes_as_count_only() {
        let slot = InventorySlot {
            count: VarInt::new(-999),
            item_id: Some(ItemID::new(1)),
            ..Default::default()
        };
        let mut buffer = Vec::new();
        slot.encode(&mut buffer, &NetEncodeOpts::default()).unwrap();
        // VarInt(-999) = 0x99 0xf8 0xff 0xff 0x0f, and nothing after it.
        assert_eq!(buffer, vec![0x99, 0xf8, 0xff, 0xff, 0x0f]);
    }

    /// The component fields only store type ids, never values, so they must never go on the
    /// wire: vanilla expects type id + value bytes per added component, and a count without
    /// values makes the client fail to decode the whole packet. This is the regression test
    /// for the join-time "Failed to decode packet 'container_set_content'" crash caused by a
    /// saved firework_rocket stack carrying a `minecraft:fireworks` (type 60) component.
    #[test]
    fn test_components_are_never_emitted_on_the_wire() {
        let slot = InventorySlot {
            count: VarInt::new(64),
            item_id: Some(ItemID::new(1192)), // minecraft:firework_rocket
            components_to_add_count: Some(VarInt::new(1)),
            components_to_remove_count: Some(VarInt::new(0)),
            components_to_add: Some(vec![VarInt::new(60)]), // minecraft:fireworks
            components_to_remove: Some(vec![]),
        };
        let mut buffer = Vec::new();
        slot.encode(&mut buffer, &NetEncodeOpts::default()).unwrap();
        // 64 = 0x40, item 1192 = VarInt 0xa8 0x09, then an empty patch (0, 0).
        assert_eq!(buffer, vec![0x40, 0xa8, 0x09, 0x00, 0x00]);
    }

    /// A slot with a count but no item id encodes the item as air (id 0), which the client
    /// accepts, instead of shifting the whole packet.
    #[test]
    fn test_count_without_item_encodes_air() {
        let slot = InventorySlot {
            count: VarInt::new(5),
            item_id: None,
            ..Default::default()
        };
        let mut buffer = Vec::new();
        slot.encode(&mut buffer, &NetEncodeOpts::default()).unwrap();
        assert_eq!(buffer, vec![0x05, 0x00, 0x00, 0x00]);
    }
}
