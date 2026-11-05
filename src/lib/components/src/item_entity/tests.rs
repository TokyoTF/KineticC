//! Unit tests for the item entity rules.

use super::*;
use ferrumc_inventories::item::ItemID;
use ferrumc_net_codec::net_types::var_int::VarInt;

/// A stack of one unit of item `id`, usable as a fixture.
fn stack_of(id: i32) -> InventorySlot {
    InventorySlot {
        count: VarInt::new(1),
        item_id: Some(ItemID::new(id)),
        components_to_add: None,
        components_to_remove: None,
        components_to_add_count: None,
        components_to_remove_count: None,
    }
}

/// A stack of one unit of the first item, usable as a fixture.
fn stack() -> InventorySlot {
    stack_of(1)
}

/// An item entity aged to `age`.
fn aged(age: u32) -> ItemEntity {
    let mut item = ItemEntity::new(stack(), Dimension::default());
    item.age = age;
    item
}

#[test]
fn new_items_are_protected_from_pickup() {
    let item = ItemEntity::new(stack(), Dimension::default());
    assert!(
        item.is_protected(),
        "an item must not be picked up the tick it spawned"
    );
    assert!(!item.can_merge());
    assert!(!item.is_expired());
}

#[test]
fn the_pickup_protection_expires() {
    let mut item = ItemEntity::new(stack(), Dimension::default());
    for _ in 0..ItemEntity::PICKUP_DELAY {
        item.pickup_delay = item.pickup_delay.saturating_sub(1);
    }
    assert!(!item.is_protected());
}

#[test]
fn the_pickup_delay_never_wraps_below_zero() {
    let mut item = ItemEntity::new(stack(), Dimension::default());
    for _ in 0..ItemEntity::PICKUP_DELAY {
        item.pickup_delay = item.pickup_delay.saturating_sub(1);
    }
    assert_eq!(item.pickup_delay, 0);
}

#[test]
fn items_merge_only_once_old_enough() {
    let young = ItemEntity::new(stack(), Dimension::default());
    let young_other = ItemEntity::new(stack(), Dimension::default());
    assert!(
        !young.can_merge_with(&young_other),
        "young stacks do not merge"
    );

    let old = aged(ItemEntity::MERGE_DELAY);
    let other = aged(ItemEntity::MERGE_DELAY);
    assert!(
        old.can_merge_with(&other),
        "old stacks of the same item merge"
    );
}

#[test]
fn both_stacks_must_be_old_enough_to_merge() {
    let first = aged(ItemEntity::MERGE_DELAY);
    let second = aged(ItemEntity::MERGE_DELAY - 1);
    assert!(!first.can_merge_with(&second));
}

#[test]
fn different_items_do_not_merge() {
    let mut first = aged(ItemEntity::MERGE_DELAY);
    let second = ItemEntity {
        item: stack_of(2),
        ..first.clone()
    };
    first.age = ItemEntity::MERGE_DELAY;
    assert!(
        !first.can_merge_with(&second),
        "stacks of different items do not merge"
    );
}

#[test]
fn items_with_different_components_do_not_merge() {
    let mut first = aged(ItemEntity::MERGE_DELAY);
    let mut other = aged(ItemEntity::MERGE_DELAY);

    other.item.components_to_add = Some(vec![VarInt::new(42)]);
    assert!(
        !first.can_merge_with(&other),
        "a stack with a component patch is not the same stack"
    );

    first.item.components_to_add = Some(vec![VarInt::new(42)]);
    assert!(first.can_merge_with(&other));
}

#[test]
fn items_in_different_dimensions_do_not_merge() {
    let first = aged(ItemEntity::MERGE_DELAY);
    let other = ItemEntity {
        dimension: Dimension::Nether,
        ..first.clone()
    };
    assert!(!first.can_merge_with(&other));
}

#[test]
fn items_despawn_after_their_lifetime() {
    let mut item = ItemEntity::new(stack(), Dimension::default());
    item.age = ItemEntity::MAX_AGE - 1;
    assert!(!item.is_expired());
    item.age = ItemEntity::MAX_AGE;
    assert!(item.is_expired());
}
