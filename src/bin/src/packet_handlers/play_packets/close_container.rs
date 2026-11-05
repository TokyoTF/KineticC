use bevy_ecs::system::{Commands, Query, Res};
use ferrumc_core::transform::position::Position;
use ferrumc_inventories::defined_slots;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::CloseContainerReceiver;
use tracing::{debug, error};

use crate::systems::chests::{close_chest, OpenChest};

/// Called when a player closes a container.
///
/// Window `0` is the player's own screen, so closing it clears the 2x2 crafting grid. Any other
/// window is a chest: the screen is confirmed as closed and the lid lowers for everyone nearby. This
/// is the only reader of the close packet, so the chest bookkeeping has to live here.
pub fn handle(
    mut commands: Commands,
    receiver: Res<CloseContainerReceiver>,
    mut inventories: Query<&mut Inventory>,
    open_chests: Query<&OpenChest>,
    players: Query<(&StreamWriter, &Position)>,
) {
    for (event, eid) in receiver.0.try_iter() {
        if event.window_id.0 == 0 {
            debug!("Clearing crafting grid");

            if let Ok(mut inventory) = inventories.get_mut(eid) {
                // CRAFT_SLOT_OUTPUT is slot 0 and CRAFT_SLOT 1-4 are slots 1-4, this clears all the survival inventory crafting grid slots
                for slot_id in
                    defined_slots::player::CRAFT_SLOT_OUTPUT..=defined_slots::player::CRAFT_SLOT_4
                {
                    inventory
                        .clear_slot_with_update(slot_id as _, eid)
                        .unwrap_or_else(|err| error!("Failed to clear player inventory: {}", err))
                }
            }
            continue;
        }

        let Ok(open) = open_chests.get(eid) else {
            continue;
        };
        let Ok((conn, _)) = players.get(eid) else {
            continue;
        };
        let listeners: Vec<(&StreamWriter, &Position)> = players.iter().collect();
        close_chest(&mut commands, eid, conn, *open, &listeners);
    }
}
