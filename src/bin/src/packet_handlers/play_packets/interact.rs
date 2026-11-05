//! Handles the Interact Entity packet.
//!
//! In 1.21.x this single packet covers both attacking (left click) and interacting (right click).
//! Only attacks are modelled here: the target entity is resolved from the client's entity id, the
//! range is verified, and a `PlayerDamaged` event is emitted. The health system applies the damage,
//! so the invulnerability rules stay in a single place.

use bevy_ecs::prelude::{Entity, MessageWriter, Query, Res};
use bevy_math::DVec3;
use ferrumc_components::player::gameplay_state::dead::Dead;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::position::Position;
use ferrumc_data::generated::items::{AttributeModifiersImpl, DataComponent, Item, Operation};
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::item::ItemID;
use ferrumc_messages::player_damage::PlayerDamaged;
use ferrumc_net::InteractEntityReceiver;
use ferrumc_state::GlobalStateResource;
use tracing::{debug, trace};

/// Maximum squared distance at which a melee hit connects, in blocks squared.
const MAX_MELEE_RANGE_SQUARED: f64 = 6.0 * 6.0;

/// Damage dealt by an empty hand.
const BARE_HAND_DAMAGE: f32 = 1.0;

/// The components that decide what a player is holding: the hotbar slot and its inventory.
type HeldItemQuery<'a> = (&'a Hotbar, &'a Inventory);

pub fn handle(
    receiver: Res<InteractEntityReceiver>,
    attackers: Query<(Entity, &PlayerIdentity, &Position)>,
    victims: Query<(Entity, &PlayerIdentity, &Position, Option<&Dead>)>,
    held_items: Query<HeldItemQuery>,
    mut damage_writer: MessageWriter<PlayerDamaged>,
    state: Res<GlobalStateResource>,
) {
    for (event, eid) in receiver.0.try_iter() {
        if !event.is_attack() {
            trace!(
                "Ignoring non-attack interaction packet (type {:?})",
                event.interaction_type
            );
            continue;
        }

        let Ok((_, attacker_identity, attacker_pos)) = attackers.get(eid) else {
            debug!("Interact packet from an entity that is not a player: {eid:?}");
            continue;
        };
        if !state.0.players.is_connected(eid) {
            continue;
        }

        let target_entity_id = event.entity_id.0;
        let Some((victim, victim_identity, victim_pos, victim_is_dead)) = victims
            .iter()
            .find(|(_, identity, _, _)| identity.short_uuid == target_entity_id)
        else {
            trace!("No entity with id {target_entity_id} is tracked by the server");
            continue;
        };

        if victim == eid {
            continue;
        }

        if victim_is_dead.is_some() {
            trace!(
                "Ignoring attack on {}: the target is already dead",
                victim_identity.username
            );
            continue;
        }

        let delta = victim_pos.coords - attacker_pos.coords;
        if delta.length_squared() > MAX_MELEE_RANGE_SQUARED {
            trace!(
                "Ignoring attack by {} on {}: targets are {:.2} blocks apart",
                attacker_identity.username,
                victim_identity.username,
                delta.length()
            );
            continue;
        }

        // The hurt animation pushes the damage flash away from the attacker, so the yaw points
        // from the attacker towards the victim, rotated into the client's frame.
        let yaw = direction_yaw(delta);

        // Damage comes from the weapon: an empty hand does one, and every held item adds its base
        // attack damage on top of that.
        let damage = held_items
            .get(eid)
            .ok()
            .and_then(|(hotbar, inventory)| {
                hotbar
                    .get_selected_item(inventory)
                    .ok()
                    .flatten()
                    .and_then(|slot| slot.item_id)
            })
            .map(|item| BARE_HAND_DAMAGE + weapon_bonus_damage(item))
            .unwrap_or(BARE_HAND_DAMAGE);

        debug!(
            "{} attacked {} for {} damage",
            attacker_identity.username, victim_identity.username, damage
        );
        damage_writer.write(PlayerDamaged::melee(victim, eid, damage, yaw));
    }
}

/// The extra damage a held item adds, on top of the damage of an empty hand.
///
/// Vanilla stores it as an attribute modifier (`minecraft:base_attack_damage`) on the item, so the
/// value that applies to a swing is the item's modifier for the main hand. Items without a modifier
/// - a pickaxe, a block, anything not meant as a weapon - add nothing.
fn weapon_bonus_damage(item_id: ItemID) -> f32 {
    let Some(id) = u16::try_from(item_id.as_u32()).ok() else {
        return 0.0;
    };
    let Some(item) = Item::from_id(id) else {
        return 0.0;
    };

    let Some(modifiers) = item
        .components
        .iter()
        .find(|(component, _)| *component == DataComponent::AttributeModifiers)
        .and_then(|(_, component)| component.as_any().downcast_ref::<AttributeModifiersImpl>())
    else {
        return 0.0;
    };

    modifiers
        .attribute_modifiers
        .iter()
        .filter(|modifier| modifier.id == "minecraft:base_attack_damage")
        .filter(|modifier| matches!(modifier.operation, Operation::AddValue))
        .map(|modifier| modifier.amount as f32)
        .sum()
}

/// Converts a horizontal delta into the direction field of the hurt animation, in degrees.
fn direction_yaw(delta: DVec3) -> f32 {
    delta.x.atan2(-delta.z).to_degrees() as f32
}
