use ferrumc_macros::{packet, NetDecode};
use ferrumc_net_codec::net_types::length_prefixed_vec::LengthPrefixedVec;
use ferrumc_net_codec::net_types::prefixed_optional::PrefixedOptional;
use ferrumc_net_codec::net_types::var_int::VarInt;

#[derive(NetDecode)]
#[packet(packet_id = "container_click", state = "play")]
pub struct ClickContainer {
    pub window_id: VarInt,
    pub state_id: VarInt,
    pub slot: i16,
    pub button: i8,
    pub mode: VarInt,
    pub changed_slots: LengthPrefixedVec<ChangedSlot>,
    /// The stack left on the cursor after the click.
    ///
    /// This is a `HashedStack`, not a full slot: only the item, the count and hashes of the
    /// component patch travel. The server needs the item and the count to drop what a click outside
    /// the window is carrying.
    pub carried_item: PrefixedOptional<HashedItem>,
}

#[derive(NetDecode)]
pub struct ChangedSlot {
    pub number: i16,
    pub data: PrefixedOptional<HashedItem>,
}

#[derive(NetDecode)]
pub struct HashedItem {
    pub item_id: VarInt,
    pub item_count: VarInt,
    pub components_to_add: LengthPrefixedVec<HashedItemComponent>,
    pub components_to_remove: LengthPrefixedVec<VarInt>,
}

#[derive(NetDecode)]
pub struct HashedItemComponent {
    pub ty: VarInt,
    pub hash: i32,
}
