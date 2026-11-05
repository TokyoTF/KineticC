//! Components for item entities: the dropped item that floats in the world until a player picks it
//! up.
//!
//! The entity itself is an ordinary [`EntityIdentity`] entity, so it reuses the existing position,
//! rotation and entity-update plumbing. The extra data needed to describe a dropped item lives in
//! [`ItemEntity`].

use bevy_ecs::prelude::Component;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_world::dimension::Dimension;

#[cfg(test)]
mod tests;

/// A dropped item.
#[derive(Component, Debug, Clone)]
pub struct ItemEntity {
    /// The item stack this entity carries.
    pub item: InventorySlot,
    /// Ticks left before the item can be picked up.
    ///
    /// The delay stops an item from being re-collected by the player who just dropped it.
    pub pickup_delay: u8,
    /// Ticks the item has existed, used for the merge and despawn rules.
    pub age: u32,
    /// The dimension the item was dropped in, so the pickup code reads the right world.
    pub dimension: Dimension,
}

impl ItemEntity {
    /// Ticks an item has to age before it can be picked up.
    ///
    /// Vanilla `ItemEntity.setDefaultPickUpDelay()` = 10: the delay block loot gets from
    /// `Block.popResource`.
    pub const PICKUP_DELAY: u8 = 10;
    /// Ticks a stack thrown by a player waits before it can be picked up.
    ///
    /// Vanilla `LivingEntity.drop` (the drop key and clicks that throw a stack out of a window)
    /// calls `setPickUpDelay(40)`, so a dropped item is not snapped back up instantly.
    pub const THROW_PICKUP_DELAY: u8 = 40;
    /// Ticks after which two nearby stacks of the same item merge.
    pub const MERGE_DELAY: u32 = 20;
    /// Ticks an item survives before despawning.
    pub const MAX_AGE: u32 = 5 * 60 * 20;

    /// Creates a dropped item with the default pickup delay.
    pub fn new(item: InventorySlot, dimension: Dimension) -> Self {
        Self::new_with_pickup_delay(item, dimension, Self::PICKUP_DELAY)
    }

    /// Creates a dropped item with an explicit pickup delay.
    pub fn new_with_pickup_delay(item: InventorySlot, dimension: Dimension, pickup_delay: u8) -> Self {
        Self {
            item,
            pickup_delay,
            age: 0,
            dimension,
        }
    }

    /// Whether the item is still protected from pickup.
    pub fn is_protected(&self) -> bool {
        self.pickup_delay > 0
    }

    /// Whether the item has lived long enough to merge with a neighbouring stack.
    pub fn can_merge(&self) -> bool {
        self.age >= Self::MERGE_DELAY
    }

    /// Whether the item has outlived its despawn timer.
    pub fn is_expired(&self) -> bool {
        self.age >= Self::MAX_AGE
    }

    /// Whether two item entities carry the same item and may merge.
    ///
    /// The component patches have to match too: two stacks of the same item with different
    /// components are different stacks in vanilla, and folding them together would lose the data
    /// or hand one of them the wrong one.
    pub fn can_merge_with(&self, other: &Self) -> bool {
        self.can_merge()
            && other.can_merge()
            && self.item.item_id == other.item.item_id
            && self.item.components_to_add == other.item.components_to_add
            && self.item.components_to_remove == other.item.components_to_remove
            && self.dimension == other.dimension
    }
}
