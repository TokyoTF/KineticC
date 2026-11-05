use bevy_ecs::prelude::Entity;
use ferrumc_data::items::Item;
use ferrumc_inventories::crafting::get_recipes_from_2x2;
use ferrumc_inventories::defined_slots;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_net_codec::net_types::var_int::VarInt;
use tracing::error;

/// Takes in a player inventory and entity id and will update the survival crafting grid output based on its inputs
pub fn update_player_crafting_grid(inventory: &mut Inventory, eid: Entity) {
    let recipes = get_recipes_from_2x2([
        [
            get_inventory_slot(inventory, defined_slots::player::CRAFT_SLOT_1),
            get_inventory_slot(inventory, defined_slots::player::CRAFT_SLOT_2),
        ],
        [
            get_inventory_slot(inventory, defined_slots::player::CRAFT_SLOT_3),
            get_inventory_slot(inventory, defined_slots::player::CRAFT_SLOT_4),
        ],
    ]);

    if let Some(first) = recipes.first().and_then(|recipe| recipe.result.as_ref()) {
        let item = Item::from_registry_key(first.id)
            .unwrap_or_else(|| panic!("Failed to get item: {:?}", first.id));

        let slot = InventorySlot {
            item_id: Some(ItemID(VarInt(item.id as _))),
            count: VarInt(first.count as _),
            ..Default::default()
        };

        inventory
            .set_item_with_update(defined_slots::player::CRAFT_SLOT_OUTPUT as _, slot, eid)
            .unwrap_or_else(|err| error!("Failed to set player crafting output slot: {}", err))
    } else {
        inventory
            .clear_slot_with_update(defined_slots::player::CRAFT_SLOT_OUTPUT as _, eid)
            .unwrap_or_else(|err| error!("Failed to clear player crafting output slot: {}", err))
    }
}

/// Spends one unit of every item in the 2x2 crafting grid.
///
/// Called after the player has taken the crafted result: the client reports the output slot as empty
/// but leaves the ingredients in place, because in vanilla the server consumes them.
pub fn consume_crafting_ingredients(inventory: &mut Inventory, eid: Entity) {
    for slot in [
        defined_slots::player::CRAFT_SLOT_1,
        defined_slots::player::CRAFT_SLOT_2,
        defined_slots::player::CRAFT_SLOT_3,
        defined_slots::player::CRAFT_SLOT_4,
    ] {
        let Some(current) = inventory.get_item(slot as usize).ok().flatten().cloned() else {
            continue;
        };

        let remaining = current.count.0 - 1;
        if remaining <= 0 {
            if let Err(err) = inventory.clear_slot_with_update(slot as usize, eid) {
                error!("Failed to clear crafting slot {slot}: {}", err);
            }
        } else if let Err(err) = inventory.set_item_with_update(
            slot as usize,
            InventorySlot {
                count: VarInt(remaining),
                ..current
            },
            eid,
        ) {
            error!("Failed to spend crafting slot {slot}: {}", err);
        }
    }

    // The grid changed, so the result slot has to be recomputed.
    update_player_crafting_grid(inventory, eid);
}

fn get_inventory_slot(inventory: &Inventory, slot: u8) -> Option<&Item> {
    inventory
        .get_item(slot as usize)
        .ok()
        .and_then(|slot| slot.and_then(|id| id.item_id))
        .and_then(|item_id| Item::from_id(item_id.0 .0 as u16))
}
