//! Round-trip checks for the player data that is written to and read from the world storage.
//!
//! The inventory of a player is what tells the server which tool the player is holding, which in
//! turn decides how long a block takes to break and whether it drops anything. If saving or
//! loading that data silently produces something else, the server believes the player is holding
//! the wrong item and every dig is then judged with the wrong tool.

use ferrumc_components::player::offline_player_data::OfflinePlayerData;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_net_codec::net_types::var_int::VarInt;

/// A stack of a given item.
fn stack(item_id: i32, count: i32) -> InventorySlot {
    InventorySlot {
        count: VarInt::new(count),
        item_id: Some(ItemID(VarInt::new(item_id))),
        components_to_add_count: None,
        components_to_remove_count: None,
        components_to_add: None,
        components_to_remove: None,
    }
}

/// The slot the selected hotbar slot maps to: hotbar slot 4 of 0-8.
const SELECTED_SLOT: usize = ferrumc_inventories::defined_slots::player::HOTBAR_SLOT_1 as usize + 3;

#[test]
fn player_data_survives_an_encode_decode_round_trip() {
    let mut inventory = Inventory::default();
    inventory
        .set_item(SELECTED_SLOT, stack(882, 1))
        .expect("a stone pickaxe fits in the inventory");

    let data = OfflinePlayerData {
        inventory,
        ..Default::default()
    };
    let bytes = bitcode::encode(&data);

    let decoded: OfflinePlayerData =
        bitcode::decode(&bytes).expect("the saved player data decodes back");

    assert_eq!(
        decoded.inventory.slots.len(),
        data.inventory.slots.len(),
        "the inventory keeps its size"
    );

    let restored = decoded.inventory.get_item(SELECTED_SLOT).unwrap().unwrap();
    assert_eq!(restored.item_id, Some(ItemID(VarInt::new(882))));
    assert_eq!(restored.count.0, 1);
}

#[test]
fn an_empty_inventory_stays_empty() {
    // A fresh player has nothing, and that must encode as nothing rather than failing.
    let data = OfflinePlayerData::default();
    let bytes = bitcode::encode(&data);

    let decoded: OfflinePlayerData =
        bitcode::decode(&bytes).expect("the empty player data decodes back");

    assert!(decoded.inventory.slots.iter().all(|slot| slot.is_none()));
}

#[test]
fn spending_an_item_leaves_the_rest_of_the_stack_alone() {
    // Placing a block takes one unit off the stack. What is left must keep the same item.
    let mut inventory = Inventory::default();
    inventory
        .set_item(SELECTED_SLOT, stack(882, 8))
        .expect("a stack of eight fits in the inventory");

    let slot = inventory.get_item(SELECTED_SLOT).unwrap().unwrap();
    let mut spent = slot.clone();
    spent.count = VarInt::new((slot.count.0 - 1).max(0));
    inventory
        .set_item(SELECTED_SLOT, spent)
        .expect("the spent stack fits in the inventory");

    let left = inventory.get_item(SELECTED_SLOT).unwrap().unwrap();
    assert_eq!(left.item_id, Some(ItemID(VarInt::new(882))));
    assert_eq!(left.count.0, 7);
}
