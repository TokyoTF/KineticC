use crate::defined_slots;
use crate::errors::InventoryError;
use crate::item::ItemID;
use crate::slot::InventorySlot;
use crate::{INVENTORY_UPDATES_QUEUE, InventoryUpdate};
use bevy_ecs::prelude::{Component, Entity};
use bitcode_derive::{Decode, Encode};
use ferrumc_net_codec::net_types::var_int::VarInt;

/// The most items a single stack can hold.
pub const MAX_STACK_SIZE: u32 = 64;

#[derive(Component, Clone, Debug, Decode, Encode)]
pub struct Inventory {
    pub slots: Box<[Option<InventorySlot>]>,
}

impl Default for Inventory {
    /// Make default inventory, sized for a PLAYER.
    /// 46 = (5 * 9) + 1 =
    /// NOT divisible by 9.
    fn default() -> Self {
        Self::new(Self::DEFAULT_PLAYER_SIZE)
    }
}

impl Inventory {
    pub const DEFAULT_PLAYER_SIZE: usize = 46;

    pub fn new(size: usize) -> Self {
        Self {
            slots: vec![None; size].into_boxed_slice(),
        }
    }

    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
    }

    pub fn contains_item(&self, item_id: i32) -> bool {
        self.slots.iter().any(|slot| {
            if let Some(slot) = slot {
                if let Some(item) = &slot.item_id {
                    item.0.0 == item_id
                } else {
                    false
                }
            } else {
                false
            }
        })
    }

    pub fn add_item(&mut self, item: InventorySlot) -> Result<(), InventoryError> {
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(item);
                return Ok(());
            }
        }
        Err(InventoryError::InventoryFull)
    }

    pub fn add_item_with_update(
        &mut self,
        item: InventorySlot,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(item.clone());
                INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
                    slot_index: index as u8,
                    slot: item,
                    entity,
                });
                return Ok(());
            }
        }
        Err(InventoryError::InventoryFull)
    }

    /// Adds a picked up item to the inventory the way vanilla does.
    ///
    /// The stack is merged into a stack of the same item when one has room for it, and whatever
    /// is left goes into the hotbar before the rest of the inventory. The crafting grid, the
    /// armour and the offhand slot are never used for this.
    ///
    /// `selected_slot` is the inventory index of the hotbar slot the player has selected, which
    /// vanilla tries first.
    pub fn pick_up_with_update(
        &mut self,
        item: InventorySlot,
        entity: Entity,
        selected_slot: Option<usize>,
    ) -> Result<(), InventoryError> {
        let Some(item_id) = item.item_id else {
            return Err(InventoryError::ItemNotFound);
        };

        let mut remaining = item.count.0.max(0) as u32;
        if remaining == 0 {
            return Ok(());
        }

        // The order vanilla looks for room in: the slot the player has selected, the rest of the
        // hotbar and then the rest of the inventory.
        let mut order: Vec<usize> = Vec::with_capacity(36);
        if let Some(slot) = selected_slot {
            order.push(slot);
        }
        order.extend(
            defined_slots::player::HOTBAR_SLOT_1 as usize
                ..=defined_slots::player::HOTBAR_SLOT_9 as usize,
        );
        order.extend(9..defined_slots::player::HOTBAR_SLOT_1 as usize);

        // Merge into a stack of the same item that still has room.
        for index in order.iter().copied() {
            if remaining == 0 {
                break;
            }

            let Some(slot) = self.slots.get_mut(index).and_then(Option::as_mut) else {
                continue;
            };

            if slot.item_id != Some(item_id) {
                continue;
            }

            let room = MAX_STACK_SIZE.saturating_sub(slot.count.0.max(0) as u32);
            if room == 0 {
                continue;
            }

            let added = room.min(remaining);
            slot.count = VarInt::new(slot.count.0 + added as i32);
            remaining -= added;

            INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
                slot_index: index as u8,
                slot: slot.clone(),
                entity,
            });
        }

        // Whatever is left goes into the first free slot.
        for index in order.iter().copied() {
            if remaining == 0 {
                break;
            }

            if self.slots.get(index).is_some_and(Option::is_some) {
                continue;
            }

            let mut stack = item.clone();
            stack.count = VarInt::new(remaining as i32);
            self.slots[index] = Some(stack.clone());
            remaining = 0;

            INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
                slot_index: index as u8,
                slot: stack,
                entity,
            });
        }

        if remaining > 0 {
            return Err(InventoryError::InventoryFull);
        }

        Ok(())
    }

    pub fn set_item(&mut self, index: usize, item: InventorySlot) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        self.slots[index] = Some(item);
        Ok(())
    }

    pub fn set_item_with_update(
        &mut self,
        index: usize,
        item: InventorySlot,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        self.slots[index] = Some(item.clone());
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index as u8,
            slot: item,
            entity,
        });
        Ok(())
    }

    /// Replaces a slot with the given content, or clears it when `item` is `None`, and sends an
    /// update. Clearing through this method keeps the server state and the client in step for slots
    /// a click emptied.
    pub fn set_slot_with_update(
        &mut self,
        index: usize,
        item: Option<InventorySlot>,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        self.slots[index] = item.clone();
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index as u8,
            slot: item.unwrap_or_default(),
            entity,
        });
        Ok(())
    }

    pub fn get_item(&self, index: usize) -> Result<Option<&InventorySlot>, InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        Ok(self.slots[index].as_ref())
    }

    pub fn remove_item(&mut self, index: usize) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        if self.slots[index].is_none() {
            return Err(InventoryError::ItemNotFound);
        }
        self.slots[index] = None;
        Ok(())
    }

    pub fn remove_item_with_update(
        &mut self,
        index: usize,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }
        if self.slots[index].is_none() {
            return Err(InventoryError::ItemNotFound);
        }
        self.slots[index] = None;
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index as u8,
            slot: InventorySlot::default(),
            entity,
        });
        Ok(())
    }

    /// Clears an inventory slot, regardless of its current state, and sends an update.
    /// This is idempotent and will not error if the slot is already empty.
    pub fn clear_slot_with_update(
        &mut self,
        index: usize,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        if index >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index));
        }

        // If the slot is already empty, we don't need to do anything
        // except send the update (which is good practice).
        if self.slots[index].is_none() {
            // Fall through to send the update
        }

        // Set the server's state to empty
        self.slots[index] = None;

        // Queue the update to tell the client the slot is now empty
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index as u8,
            slot: InventorySlot::default(), // An empty slot (count: 0)
            entity,
        });
        Ok(())
    }

    /// Searches the inventory for the first slot containing the given ItemID.
    ///
    /// Returns `Some(index)` if found, `None` otherwise.
    pub fn find_item(&self, item_id: ItemID) -> Option<usize> {
        self.slots.iter().position(|slot| match slot {
            Some(inventory_slot) => inventory_slot.item_id == Some(item_id),
            None => false,
        })
    }

    /// Swaps the contents of two slots and sends updates to the client.
    pub fn swap_slots_with_update(
        &mut self,
        index_a: usize,
        index_b: usize,
        entity: Entity,
    ) -> Result<(), InventoryError> {
        if index_a >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index_a));
        }
        if index_b >= self.slots.len() {
            return Err(InventoryError::InvalidSlotIndex(index_b));
        }
        if index_a == index_b {
            return Ok(()); // Nothing to do
        }

        // Swap the slots in the server's memory
        self.slots.swap(index_a, index_b);

        // Send an update for the first slot
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index_a as u8,
            // Clone the data that is now in slot A
            slot: self.slots[index_a].clone().unwrap_or_default(),
            entity,
        });

        // Send an update for the second slot
        INVENTORY_UPDATES_QUEUE.push(InventoryUpdate {
            slot_index: index_b as u8,
            // Clone the data that is now in slot B
            slot: self.slots[index_b].clone().unwrap_or_default(),
            entity,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemID;
    use ferrumc_net_codec::net_types::var_int::VarInt;

    fn make_slot_with_id(id: i32) -> InventorySlot {
        InventorySlot {
            item_id: Some(ItemID(VarInt::new(id))),
            ..Default::default()
        }
    }

    #[test]
    fn test_new_inventory() {
        let inv = Inventory::new(5);
        assert_eq!(inv.slots.len(), 5);
        assert!(inv.slots.iter().all(|s| s.is_none()));
    }

    #[test]
    fn a_picked_up_item_goes_into_the_hotbar() {
        let mut inv = Inventory::default();

        let mut slot = make_slot_with_id(1);
        slot.count = VarInt::new(3);

        inv.pick_up_with_update(slot, Entity::from_raw_u32(0).unwrap(), None)
            .expect("the item fits in the inventory");

        // The hotbar comes before the rest of the inventory, and the crafting grid, the armour
        // and the offhand slot are never used.
        assert!(
            inv.get_item(defined_slots::player::HOTBAR_SLOT_1 as usize)
                .unwrap()
                .is_some()
        );
        assert!(inv.get_item(0).unwrap().is_none());
        assert!(inv.get_item(9).unwrap().is_none());
    }

    #[test]
    fn a_picked_up_item_goes_into_the_selected_slot_first() {
        let mut inv = Inventory::default();
        let selected = defined_slots::player::HOTBAR_SLOT_3 as usize;

        let mut stack = make_slot_with_id(1);
        stack.count = VarInt::new(1);

        inv.pick_up_with_update(stack, Entity::from_raw_u32(0).unwrap(), Some(selected))
            .expect("the item fits in the inventory");

        assert!(inv.get_item(selected).unwrap().is_some());
        assert!(
            inv.get_item(defined_slots::player::HOTBAR_SLOT_1 as usize)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_picked_up_item_is_merged_into_a_stack_of_the_same_item() {
        let mut inv = Inventory::default();

        let mut stack = make_slot_with_id(1);
        stack.count = VarInt::new(60);
        inv.set_item(defined_slots::player::HOTBAR_SLOT_1 as usize, stack)
            .unwrap();

        let mut picked_up = make_slot_with_id(1);
        picked_up.count = VarInt::new(10);
        inv.pick_up_with_update(picked_up, Entity::from_raw_u32(0).unwrap(), None)
            .expect("the item fits in the inventory");

        let merged = inv
            .get_item(defined_slots::player::HOTBAR_SLOT_1 as usize)
            .unwrap()
            .unwrap();
        assert_eq!(merged.count.0, 64);

        // The rest does not fit in the full stack and goes into the next free slot.
        let overflow = inv
            .get_item(defined_slots::player::HOTBAR_SLOT_2 as usize)
            .unwrap()
            .unwrap();
        assert_eq!(overflow.count.0, 6);
    }

    #[test]
    fn test_add_and_get_item() {
        let mut inv = Inventory::new(2);
        let slot = make_slot_with_id(1);
        assert!(inv.add_item(slot.clone()).is_ok());
        assert!(inv.get_item(0).unwrap().is_some());
        assert!(inv.get_item(1).unwrap().is_none());
    }

    #[test]
    fn test_add_item_full() {
        let mut inv = Inventory::new(1);
        let slot = make_slot_with_id(1);
        inv.add_item(slot).unwrap();
        let slot2 = make_slot_with_id(2);
        assert!(matches!(
            inv.add_item(slot2),
            Err(InventoryError::InventoryFull)
        ));
    }

    #[test]
    fn test_set_and_remove_item() {
        let mut inv = Inventory::new(1);
        let slot = make_slot_with_id(1);
        inv.set_item(0, slot).unwrap();
        assert!(inv.get_item(0).unwrap().is_some());
        inv.remove_item(0).unwrap();
        assert!(inv.get_item(0).unwrap().is_none());
    }

    #[test]
    fn test_contains_item() {
        let mut inv = Inventory::new(2);
        let slot = make_slot_with_id(42);
        inv.add_item(slot).unwrap();
        assert!(inv.contains_item(42));
        assert!(!inv.contains_item(99));
    }

    #[test]
    fn test_clear() {
        let mut inv = Inventory::new(2);
        inv.set_item(0, make_slot_with_id(1)).unwrap();
        inv.set_item(1, make_slot_with_id(2)).unwrap();
        inv.clear();
        assert!(inv.slots.iter().all(|s| s.is_none()));
    }

    #[test]
    fn test_invalid_index() {
        let mut inv = Inventory::new(1);
        assert!(matches!(
            inv.get_item(2),
            Err(InventoryError::InvalidSlotIndex(2))
        ));
        assert!(matches!(
            inv.set_item(2, make_slot_with_id(1)),
            Err(InventoryError::InvalidSlotIndex(2))
        ));
        assert!(matches!(
            inv.remove_item(2),
            Err(InventoryError::InvalidSlotIndex(2))
        ));
    }
}
