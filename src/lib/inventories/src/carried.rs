//! The stack a player is carrying on the cursor of an open container screen.

use bevy_ecs::prelude::Component;
use ferrumc_net_codec::net_types::var_int::VarInt;

use crate::slot::InventorySlot;

/// The stack held on the cursor while a container screen is open.
///
/// A click packet reports the slots that changed and the stack left on the cursor, but dropping
/// outside the window touches no slot at all: the client only clears its own cursor. Without
/// remembering what the cursor held, the server would never learn which item to spawn and the stack
/// would silently disappear.
#[derive(Component, Clone, Debug, Default)]
pub struct CarriedItem(pub Option<InventorySlot>);

impl CarriedItem {
    /// The stack on the cursor, if it holds one.
    pub fn stack(&self) -> Option<&InventorySlot> {
        self.0.as_ref()
    }

    /// Replaces the cursor with `slot`, treating an empty stack as an empty cursor.
    pub fn set(&mut self, slot: Option<InventorySlot>) {
        self.0 = slot.filter(|slot| slot.count.0 > 0 && slot.item_id.is_some());
    }

    /// Takes `count` items off the cursor, leaving the rest where it was.
    ///
    /// Returns the removed stack, or `None` when the cursor is empty or `count` is not positive.
    pub fn take(&mut self, count: i32) -> Option<InventorySlot> {
        let stack = self.0.as_mut()?;
        let available = stack.count.0;
        let taken = count.clamp(0, available);

        if taken == 0 {
            return None;
        }

        let mut removed = stack.clone();
        removed.count = VarInt::new(taken);

        let remaining = available - taken;
        if remaining > 0 {
            stack.count = VarInt::new(remaining);
        } else {
            self.0 = None;
        }

        Some(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemID;
    use ferrumc_net_codec::net_types::var_int::VarInt;

    fn stack(count: i32) -> InventorySlot {
        InventorySlot {
            count: VarInt::new(count),
            item_id: Some(ItemID::new(1)),
            components_to_add: None,
            components_to_remove: None,
            components_to_add_count: None,
            components_to_remove_count: None,
        }
    }

    #[test]
    fn an_empty_cursor_carries_nothing() {
        assert!(CarriedItem::default().stack().is_none());
    }

    #[test]
    fn dropping_the_whole_cursor_empties_it() {
        let mut cursor = CarriedItem(Some(stack(5)));
        assert!(cursor.take(5).is_some());
        assert!(cursor.stack().is_none());
    }

    #[test]
    fn dropping_one_leaves_the_rest() {
        let mut cursor = CarriedItem(Some(stack(5)));
        let dropped = cursor.take(1).unwrap();
        assert_eq!(dropped.count.0, 1);
        assert_eq!(cursor.stack().unwrap().count.0, 4);
    }

    #[test]
    fn dropping_more_than_available_takes_all() {
        let mut cursor = CarriedItem(Some(stack(3)));
        let dropped = cursor.take(10).unwrap();
        assert_eq!(dropped.count.0, 3);
        assert!(cursor.stack().is_none());
    }
}
