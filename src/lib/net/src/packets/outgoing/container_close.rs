//! Container Close packet.
//!
//! Sent when a container screen the client has open stops being usable: the block was broken, the
//! server is shutting down, or the player was moved away. The client then returns to its inventory.

use ferrumc_macros::{packet, NetEncode};
use ferrumc_net_codec::net_types::var_int::VarInt;

#[derive(NetEncode)]
#[packet(packet_id = "container_close", state = "play")]
pub struct ContainerClose {
    pub window_id: VarInt,
}

impl ContainerClose {
    /// Closes the given window.
    pub fn new(window_id: i32) -> Self {
        Self {
            window_id: VarInt::new(window_id),
        }
    }
}
