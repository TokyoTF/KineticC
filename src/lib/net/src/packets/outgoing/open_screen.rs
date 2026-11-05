//! Open Screen packet.
//!
//! Tells the client that a container screen was opened. The window type references the
//! `minecraft:menu` registry that was sent during configuration, so it is an id and not a name.
//! Chests are the only container the server opens today.

use ferrumc_macros::{packet, NetEncode};
use ferrumc_nbt::NBT;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_text::TextComponent;

/// Id of `minecraft:generic_9x3` in the `minecraft:menu` registry: a single chest, 27 slots.
pub const MENU_TYPE_SINGLE_CHEST: i32 = 2;

/// Id of `minecraft:generic_9x6` in the `minecraft:menu` registry: a double chest, 54 slots.
pub const MENU_TYPE_DOUBLE_CHEST: i32 = 5;

#[derive(NetEncode)]
#[packet(packet_id = "open_screen", state = "play")]
pub struct OpenScreen {
    /// The window id the client echoes back in its inventory packets.
    pub container_id: VarInt,
    /// The container type from the `minecraft:menu` registry.
    pub window_type: VarInt,
    /// The screen title.
    pub title: NBT<TextComponent>,
}

impl OpenScreen {
    /// Opens a container screen in the given window.
    pub fn new(container_id: i32, window_type: i32, title: TextComponent) -> Self {
        Self {
            container_id: VarInt::new(container_id),
            window_type: VarInt::new(window_type),
            title: NBT::new(title),
        }
    }

    /// Opens a chest screen in the given window.
    pub fn chest(container_id: i32, double: bool, title: TextComponent) -> Self {
        let window_type = if double {
            MENU_TYPE_DOUBLE_CHEST
        } else {
            MENU_TYPE_SINGLE_CHEST
        };
        Self::new(container_id, window_type, title)
    }
}
