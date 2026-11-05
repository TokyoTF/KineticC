//! Applies container clicks to the inventories they belong to.
//!
//! A click reports the slots the client believes changed. In the player's own screen those slots are
//! the player's inventory, so the change is applied as reported. In a chest screen the container and
//! the player are on the same list, with the container first, so the slots have to be split between
//! the chest's inventory and the player's.
//!
//! A reported slot may be absent, which means the client emptied it; that case is applied as a clear
//! rather than skipped, otherwise a stack taken out of a slot would come back the next time the
//! screen is opened.
//!
//! Two kinds of click remove items without being covered by the player's own slots: the drop key on
//! a hovered slot and a click outside the window with a stack on the cursor. The first sends the
//! remaining count in its reported slots, the second touches no slot at all, so the stack the cursor
//! holds is tracked in [`CarriedItem`] and both spawn the item entity here.
//!
//! Slot numbers come straight from the client and are therefore attacker-controlled: every index is
//! validated before it reaches an inventory, and an out-of-range slot is skipped rather than
//! unwrapped, which used to crash the server thread.

use crate::packet_handlers::player::update_crafting::consume_crafting_ingredients;
use crate::packet_handlers::player::update_crafting::update_player_crafting_grid;
use crate::systems::chests::{
    chest_slots, container_contents, store_contents, ChestStore, OpenChest,
};
use crate::systems::item_entities::{drop_item, thrown_stack_motion};
use bevy_ecs::prelude::{Commands, Entity, Query, Res};
use ferrumc_components::item_entity::ItemEntity;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_inventories::carried::CarriedItem;
use ferrumc_inventories::defined_slots;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::incoming::click_container::{ClickContainer, HashedItem};
use ferrumc_net_codec::net_types::prefixed_optional::PrefixedOptional;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::dimension::Dimension;
use tracing::{debug, warn};

/// The inventory mode of a normal left or right click (`ClickType.PICKUP`).
const PICKUP_MODE: i32 = 0;
/// The inventory mode of the drop key (`ClickType.THROW`).
const THROW_MODE: i32 = 4;
/// The slot the client reports when a click lands outside every slot.
const OUTSIDE_SLOT: i16 = -999;

type ConnectionQuery<'a> = (Entity, &'a StreamWriter);

pub fn handle(
    receiver: Res<ferrumc_net::ClickContainerReceiver>,
    mut inventories: Query<&mut Inventory>,
    mut cursors: Query<&mut CarriedItem>,
    open_chests: Query<&OpenChest>,
    chest_store: Res<ChestStore>,
    world: Res<GlobalStateResource>,
    players: Query<(&Position, &Rotation)>,
    mut commands: Commands,
    connections: Query<ConnectionQuery<'_>>,
) {
    for (event, eid) in receiver.0.try_iter() {
        // The stack on the cursor before this click. A click outside the window drops it, and a
        // drop key click on a slot is only valid while the cursor is empty, so both need it up front.
        let carried = cursors.get(eid).ok().and_then(|cursor| cursor.0.clone());

        // Where and how fast a stack thrown by this click leaves the player's hand.
        let motion = players
            .get(eid)
            .ok()
            .map(|(position, rotation)| thrown_stack_motion(position, rotation));
        let mut throw = |stack: InventorySlot| {
            if let Some((position, velocity)) = &motion {
                drop_item(
                    &mut commands,
                    position,
                    velocity,
                    stack,
                    Dimension::default(),
                    // Thrown by the player out of a window: vanilla `setPickUpDelay(40)`.
                    ItemEntity::THROW_PICKUP_DELAY,
                    &connections,
                );
            }
        };

        let outside = event.slot == OUTSIDE_SLOT && event.mode.0 == PICKUP_MODE;
        let cursor_after = carried_after(&event);

        if outside {
            // Vanilla treats a click outside the window as a pickup click on slot -999: the primary
            // button drops the whole cursor and the secondary one a single item. The client only
            // clears its own cursor, so the server is the one that has to spawn the item entity.
            if let Ok(mut cursor) = cursors.get_mut(eid) {
                let count = outside_drop_count(event.button, cursor.stack().map_or(0, |stack| stack.count.0));
                if let Some(dropped) = cursor.take(count) {
                    throw(dropped);
                }
            }
        } else if let Some(thrown) = clicked_throw(
            &event,
            &inventories,
            &open_chests,
            &chest_store,
            &world,
            eid,
            carried.as_ref(),
        ) {
            throw(thrown);
        }

        match open_chests.get(eid) {
            Ok(open) => apply_to_chest(event, eid, *open, &chest_store, &world, &mut inventories),
            Err(_) => apply_to_player(event, eid, &mut inventories),
        }

        // A drop outside the window already updated the cursor with what is left after the throw;
        // every other click reports its resulting cursor in the packet.
        if !outside {
            if let Ok(mut cursor) = cursors.get_mut(eid) {
                cursor.set(cursor_after);
            }
        }
    }
}

/// Applies the clicked slots to the player's own inventory.
///
/// This is also where the 2x2 crafting grid lives, so a change to it recomputes the result slot.
fn apply_to_player(
    event: ferrumc_net::packets::incoming::click_container::ClickContainer,
    entity: Entity,
    inventories: &mut Query<&mut Inventory>,
) {
    let Ok(mut inventory) = inventories.get_mut(entity) else {
        warn!("Failed to get inventory for entity {entity}");
        return;
    };

    let slot_count = inventory.slots.len();
    let mut took_craft_result = false;
    for slot in event.changed_slots.data {
        let Ok(slot_number) = usize::try_from(slot.number) else {
            warn!(
                "Ignoring container click for entity {entity}: negative slot {}",
                slot.number
            );
            continue;
        };
        let updated = clicked_slot(slot);

        if slot_number >= slot_count {
            warn!(
                "Ignoring container click for entity {entity}: slot {slot_number} out of bounds \
                 (inventory has {slot_count} slots)"
            );
            continue;
        }

        let emptied_output = slot_number == defined_slots::player::CRAFT_SLOT_OUTPUT as usize
            && updated.as_ref().is_none_or(|updated| updated.count.0 == 0);

        if let Err(err) = inventory.set_slot_with_update(slot_number, updated, entity) {
            warn!("Failed to update slot {slot_number} for {entity}: {err:?}");
            continue;
        }

        if (defined_slots::player::CRAFT_SLOT_1..=defined_slots::player::CRAFT_SLOT_4)
            .contains(&(slot_number as u8))
        {
            update_player_crafting_grid(&mut inventory, entity);
        }

        // A result the client has picked out of the crafting grid leaves the output slot empty, so
        // that is the moment the ingredients are spent.
        if emptied_output {
            took_craft_result = true;
        }
    }

    if took_craft_result {
        consume_crafting_ingredients(&mut inventory, entity);
    }
}

/// Applies the clicked slots to a chest and the player's inventory.
fn apply_to_chest(
    event: ferrumc_net::packets::incoming::click_container::ClickContainer,
    entity: Entity,
    open: OpenChest,
    chest_store: &ChestStore,
    world: &Res<GlobalStateResource>,
    inventories: &mut Query<&mut Inventory>,
) {
    let Ok(mut inventory) = inventories.get_mut(entity) else {
        warn!("Failed to get inventory for entity {entity}");
        return;
    };

    // The compound view of both halves: 27 slots for a single chest, 54 for a double one.
    let mut chest = container_contents(chest_store, world, &open.pair);
    let container_slots = chest.slots.len() as i32;

    for slot in event.changed_slots.data {
        let reported_slot = i32::from(slot.number);
        let Ok(slot_index) = usize::try_from(reported_slot) else {
            warn!("Ignoring container click for entity {entity}: negative slot {reported_slot}");
            continue;
        };
        let updated = clicked_slot(slot);

        // The chest's own slots come first, and everything after them belongs to the player.
        if slot_index < chest.slots.len() {
            chest.slots[slot_index] = updated;
            continue;
        }

        let Some(player_slot) = chest_slots::to_player_slot(reported_slot, container_slots) else {
            warn!(
                "Ignoring container click for entity {entity}: slot {reported_slot} is outside the \
                 chest window"
            );
            continue;
        };

        if let Err(err) = inventory.set_slot_with_update(player_slot as usize, updated, entity) {
            warn!("Failed to update slot {player_slot} for {entity}: {err:?}");
        }
    }

    store_contents(chest_store, world, &open.pair, &chest);

    debug!("Updated chest at {:?}", open.pair.first.pos);
}

/// The stack a drop key click takes out of a slot, ready to be spawned as an item entity.
///
/// Vanilla only performs a throw while the cursor is empty: the primary button throws one item and
/// the secondary one the whole stack. The slot itself is left to the reported slots, which already
/// carry the remaining count, so this only has to describe the item entity.
fn clicked_throw(
    event: &ClickContainer,
    inventories: &Query<&mut Inventory>,
    open_chests: &Query<&OpenChest>,
    chest_store: &ChestStore,
    world: &GlobalStateResource,
    player: Entity,
    carried: Option<&InventorySlot>,
) -> Option<InventorySlot> {
    if event.mode.0 != THROW_MODE || event.slot < 0 || carried.is_some() {
        return None;
    }

    let slot = event.slot as usize;
    let stack = match open_chests.get(player) {
        Ok(open) => chest_window_stack(open, chest_store, world, inventories, player, slot),
        Err(_) => inventories
            .get(player)
            .ok()
            .and_then(|inventory| inventory.get_item(slot).ok().flatten().cloned()),
    }?;

    let count = slot_throw_count(event.button, stack.count.0);
    if count == 0 {
        return None;
    }

    let mut thrown = stack;
    thrown.count = VarInt::new(count);
    Some(thrown)
}

/// The stack a chest window shows in `slot`: one of the chest's own, or one of the player's.
fn chest_window_stack(
    open: &OpenChest,
    chest_store: &ChestStore,
    world: &GlobalStateResource,
    inventories: &Query<&mut Inventory>,
    player: Entity,
    slot: usize,
) -> Option<InventorySlot> {
    let chest = container_contents(chest_store, world, &open.pair);
    if slot < chest.slots.len() {
        return chest.slots[slot].clone();
    }

    let player_slot = chest_slots::to_player_slot(slot as i32, chest.slots.len() as i32)?;
    inventories.get(player).ok().and_then(|inventory| {
        inventory
            .get_item(player_slot as usize)
            .ok()
            .flatten()
            .cloned()
    })
}

/// The stack the click leaves on the cursor.
///
/// The protocol sends it as a hashed stack, so only the item and the count can be recovered; the
/// component patch is a hash and is not needed to drop the stack.
fn carried_after(event: &ClickContainer) -> Option<InventorySlot> {
    match &event.carried_item {
        PrefixedOptional::Some(hashed) => Some(hashed_stack(hashed)),
        PrefixedOptional::None => None,
    }
}

/// The stack a hashed slot describes, without its component patch.
fn hashed_stack(hashed: &HashedItem) -> InventorySlot {
    InventorySlot {
        count: hashed.item_count,
        item_id: Some(ItemID(hashed.item_id)),
        components_to_add: None,
        components_to_remove: None,
        components_to_add_count: None,
        components_to_remove_count: None,
    }
}

/// The slot content a click reported, or `None` when the report cleared the slot.
fn clicked_slot(
    slot: ferrumc_net::packets::incoming::click_container::ChangedSlot,
) -> Option<InventorySlot> {
    slot.data
        .to_option()
        .map(|new_data| hashed_stack(&new_data))
}

/// How many items a click outside the window drops from the cursor.
fn outside_drop_count(button: i8, available: i32) -> i32 {
    match button {
        0 => available,
        _ => 1,
    }
    .clamp(0, available)
}

/// How many items the drop key takes from a slot.
fn slot_throw_count(button: i8, available: i32) -> i32 {
    match button {
        0 => 1,
        _ => available,
    }
    .clamp(0, available)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_primary_drops_the_whole_cursor() {
        assert_eq!(outside_drop_count(0, 12), 12);
    }

    #[test]
    fn outside_secondary_drops_one() {
        assert_eq!(outside_drop_count(1, 12), 1);
    }

    #[test]
    fn outside_counts_never_exceed_the_cursor() {
        assert_eq!(outside_drop_count(0, 0), 0);
        assert_eq!(outside_drop_count(1, 0), 0);
    }

    #[test]
    fn slot_throw_primary_drops_one() {
        assert_eq!(slot_throw_count(0, 12), 1);
    }

    #[test]
    fn slot_throw_secondary_drops_the_whole_stack() {
        assert_eq!(slot_throw_count(1, 12), 12);
    }

    #[test]
    fn slot_throw_does_nothing_on_empty_slot() {
        assert_eq!(slot_throw_count(0, 0), 0);
        assert_eq!(slot_throw_count(1, 0), 0);
    }
}
