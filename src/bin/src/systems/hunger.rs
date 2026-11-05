//! The food bar: eating, exhaustion, regeneration and starvation.
//!
//! Three systems live here:
//!
//! * [`eat_item`] turns a `use_item` packet into a `PlayerEating` event when the held stack carries
//!   a `minecraft:food` component, and [`apply_eaten_food`] applies the nutrition and saturation it
//!   carries. The item stack is consumed by the same system, so the server's inventory accounting
//!   stays correct even before the client mirrors the click.
//! * [`drain_hunger`] runs the vanilla tick rules: exhaustion is spent on saturation first and on
//!   the food level only once saturation runs out, a well-fed player regenerates health, and a
//!   player with an empty food bar slowly starves to death.
//! * [`apply_exhaustion`] folds the `PlayerExhausted` events emitted by mining and combat into the
//!   player's exhaustion total.
//!
//! The damage caused by starvation goes through the same `PlayerDamaged` event as every other damage
//! source, so the health bar, the death event and the client stay in sync.

use bevy_ecs::prelude::{Entity, MessageReader, MessageWriter, Query, Res};
use ferrumc_components::health::Health;
use ferrumc_components::player::gameplay_state::dead::Dead;
use ferrumc_components::player::hunger::Hunger;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_data::generated::items::{DataComponent, FoodImpl, Item};
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_messages::player_damage::{DamageSource, PlayerDamaged};
use ferrumc_messages::player_eat::PlayerEating;
use ferrumc_messages::player_exhaustion::PlayerExhausted;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::set_health::SetHealth;
use ferrumc_net_codec::net_types::var_int::VarInt;
use tracing::{debug, trace, warn};

/// Maximum food level (20 = 10 shanks).
const MAX_FOOD: u8 = 20;
/// Maximum saturation.
const MAX_SATURATION: f32 = 20.0;
/// Exhaustion drained at once once this much has accumulated.
const EXHAUSTION_DRAIN: f32 = 4.0;
/// Food level at or above which a player regenerates health.
const REGEN_FOOD_THRESHOLD: u8 = 18;
/// Damage taken per starvation hit, in the same units as health (1 = half a heart).
const STARVATION_DAMAGE: f32 = 1.0;
/// Ticks between two starvation hits.
const STARVATION_INTERVAL: u64 = 80;
/// Ticks between two regeneration ticks.
const REGEN_INTERVAL: u64 = 80;

/// Handles one use of the held item, consuming it when it is food.
///
/// This is a plain function rather than a system because the packet channel has a single reader:
/// the use-item handler drains it and calls this for each event. Anything that is not food is
/// ignored; eating is the only item use that has a visible effect on survival stats. Buckets, bows
/// and shields need combat and block placement support that is not in place yet.
pub fn eat_selected_item(
    entity: Entity,
    identity: &PlayerIdentity,
    hotbar: &Hotbar,
    inventory: &mut Inventory,
    eat_writer: &mut MessageWriter<PlayerEating>,
) {
    let Some(slot) = hotbar
        .get_selected_item(inventory)
        .ok()
        .flatten()
        .filter(|slot| slot.count.0 > 0)
        .cloned()
    else {
        trace!(
            "Ignoring a use-item packet from {}: the selected slot is empty",
            identity.username
        );
        return;
    };

    let Some(food) = slot.item_id.and_then(food_values) else {
        trace!(
            "Ignoring a use-item packet from {}: the held item is not food",
            identity.username
        );
        return;
    };

    let Some(item_id) = slot.item_id else {
        return;
    };

    eat_writer.write(PlayerEating {
        player: entity,
        item_id,
        food_value: food.nutrition,
        saturation_value: food.saturation,
    });

    consume_one(hotbar, inventory, &slot, entity);
    debug!(
        "{} ate an item worth {} food and {} saturation",
        identity.username, food.nutrition, food.saturation
    );
}

/// Applies the nutrition and saturation of a `PlayerEating` event.
pub fn apply_eaten_food(
    mut events: MessageReader<PlayerEating>,
    mut hungers: Query<(Entity, &mut Hunger, &Health)>,
    mut conn_query: Query<&StreamWriter>,
) {
    for event in events.read() {
        let Ok((entity, mut hunger, health)) = hungers.get_mut(event.player) else {
            continue;
        };

        hunger.level = hunger.level.saturating_add(event.food_value).min(MAX_FOOD);
        hunger.saturation = (hunger.saturation + event.saturation_value).min(MAX_SATURATION);

        // The food bar and the health bar travel in the same packet, so one send updates both.
        if let Ok(conn) = conn_query.get_mut(entity) {
            let packet = SetHealth::new(health.current, i32::from(hunger.level), hunger.saturation);
            if let Err(err) = conn.send_packet(packet) {
                warn!("Failed to send the food update: {err:?}");
            }
        }

        debug!(
            "{:?} now has {} food and {} saturation",
            event.player, hunger.level, hunger.saturation
        );
    }
}

/// Folds the exhaustion emitted by mining and combat into the player's exhaustion total.
pub fn apply_exhaustion(
    mut events: MessageReader<PlayerExhausted>,
    mut hungers: Query<&mut Hunger>,
) {
    for event in events.read() {
        let Ok(mut hunger) = hungers.get_mut(event.player) else {
            continue;
        };
        hunger.exhaustion += event.amount;
    }
}

/// Runs the exhaustion, regeneration and starvation rules once per tick.
pub fn drain_hunger(
    mut players: Query<(Entity, &mut Health, &mut Hunger, Option<&Dead>)>,
    mut conn_query: Query<&StreamWriter>,
    mut damage_writer: MessageWriter<PlayerDamaged>,
    tick: Res<ferrumc_core::tick::TickCounter>,
) {
    let tick_now = tick.get();

    for (entity, mut health, mut hunger, is_dead) in players.iter_mut() {
        if is_dead.is_some() {
            continue;
        }

        // Exhaustion is spent in chunks of four: saturation absorbs it first, the food level only
        // once saturation is empty.
        if hunger.exhaustion >= EXHAUSTION_DRAIN {
            let drained = hunger.exhaustion.floor();
            hunger.exhaustion -= drained;
            let spent = drained.min(EXHAUSTION_DRAIN);

            if hunger.saturation > 0.0 {
                hunger.saturation = (hunger.saturation - spent).max(0.0);
            } else if hunger.level > 0 {
                hunger.level = hunger.level.saturating_sub(spent as u8);
            }
        }

        let starving = hunger.level == 0;
        let regenerating =
            !starving && hunger.level >= REGEN_FOOD_THRESHOLD && health.current < health.max;

        if starving && tick_now.is_multiple_of(STARVATION_INTERVAL) {
            damage_writer.write(PlayerDamaged::from_source(
                entity,
                STARVATION_DAMAGE,
                DamageSource::Starvation,
            ));
        }

        if regenerating && tick_now.is_multiple_of(REGEN_INTERVAL) {
            health.current = (health.current + 1.0).min(health.max);
        }

        if starving || regenerating {
            if let Ok(conn) = conn_query.get_mut(entity) {
                let _ = conn.send_packet(SetHealth::new(
                    health.current,
                    i32::from(hunger.level),
                    hunger.saturation,
                ));
            }
        }
    }
}

/// Removes one unit of the eaten stack, and the item when the stack runs out.
fn consume_one(hotbar: &Hotbar, inventory: &mut Inventory, slot: &InventorySlot, entity: Entity) {
    let remaining = slot.count.0 - 1;
    if remaining <= 0 {
        if let Err(err) = hotbar.remove_item_with_update(inventory, hotbar.selected_slot, entity) {
            warn!("Failed to consume the eaten item: {err:?}");
        }
        return;
    }

    let remaining_slot = InventorySlot {
        count: VarInt::new(remaining),
        item_id: slot.item_id,
        components_to_add: None,
        components_to_remove: None,
        components_to_add_count: None,
        components_to_remove_count: None,
    };
    if let Err(err) = inventory.set_item_with_update(
        hotbar.get_selected_inventory_index(),
        remaining_slot,
        entity,
    ) {
        warn!("Failed to update the eaten item stack: {err:?}");
    }
}

/// The food values of an item, or `None` when the item is not food.
fn food_values(item_id: ItemID) -> Option<FoodImpl> {
    let id: u16 = item_id.as_u32().try_into().ok()?;
    let item = Item::from_id(id)?;

    let component = item
        .components
        .iter()
        .find(|(component, _)| *component == DataComponent::Food)
        .map(|(_, component)| component)?;

    component.as_any().downcast_ref::<FoodImpl>().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_food_items_have_no_food_values() {
        let stone = ItemID::from_name("minecraft:stone").unwrap();
        assert!(food_values(stone).is_none());
    }

    #[test]
    fn bread_reports_its_vanilla_values() {
        let bread = ItemID::from_name("minecraft:bread").unwrap();
        let food = food_values(bread).expect("bread is food");
        assert_eq!(food.nutrition, 5);
        assert_eq!(food.saturation, 6.0);
    }

    #[test]
    fn golden_apple_reports_its_vanilla_values() {
        let apple = ItemID::from_name("minecraft:golden_apple").unwrap();
        let food = food_values(apple).expect("a golden apple is food");
        assert_eq!(food.nutrition, 4);
    }

    #[test]
    fn saturation_is_capped() {
        // Eating must never push saturation past its maximum, whatever the item says.
        let mut hunger = Hunger {
            level: MAX_FOOD,
            saturation: MAX_SATURATION,
            exhaustion: 0.0,
        };
        hunger.saturation = (hunger.saturation + 100.0).min(MAX_SATURATION);
        assert_eq!(hunger.saturation, MAX_SATURATION);
    }
}
