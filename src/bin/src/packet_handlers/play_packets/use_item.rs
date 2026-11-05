//! Use Item packet handler.
//!
//! Sent when a player uses the item in their hand with nothing in front of them: throwing an
//! ender pearl, a snowball or an egg, boosting with a firework rocket, eating, drawing a bow, and
//! so on. Throwables leave the hand as projectile entities; only eating has any other survival
//! effect that is implemented, so the packet is also handed to the hunger systems, which ignore
//! anything that is not food.
//!
//! The packet carries a sequence id, which vanilla expects to be acknowledged so the client knows the
//! swing animation may finish. The acknowledgement is sent by the swing handler, which runs on the
//! same click.
//!
//! This system is the only reader of the use-item channel. A second reader would consume the events
//! before this one saw them, which is why eating is a plain function called from here rather than a
//! system of its own.

use bevy_ecs::prelude::{Commands, Entity, MessageWriter, Query, Res};
use ferrumc_components::player::fall_flying::FallFlying;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_core::transform::grounded::OnGround;
use ferrumc_core::transform::position::Position;
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_core::transform::velocity::Velocity;
use ferrumc_inventories::hotbar::Hotbar;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_messages::player_eat::PlayerEating;
use ferrumc_net::UseItemReceiver;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::incoming::use_item::Hand;
use ferrumc_net::packets::outgoing::entity_animation::EntityAnimationPacket;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::dimension::Dimension;
use tracing::trace;

use crate::systems::{hunger, projectiles};

/// Animation code for swinging the arm of the hand that was used.
const SWING_MAIN_HAND: u8 = 0;
const SWING_OFF_HAND: u8 = 3;

/// The player state a use-item click reads: the inventory to spend from and everything a
/// [`projectiles::Thrower`] needs when the held item is a throwable.
type UseItemPlayer<'a> = (
    Entity,
    &'a PlayerIdentity,
    &'a Hotbar,
    &'a mut Inventory,
    &'a Position,
    &'a Rotation,
    Option<&'a Velocity>,
    Option<&'a OnGround>,
    Option<&'a FallFlying>,
);

pub fn handle(
    receiver: Res<UseItemReceiver>,
    mut players: Query<UseItemPlayer<'_>>,
    conn_query: Query<(Entity, &StreamWriter)>,
    mut commands: Commands,
    state: Res<GlobalStateResource>,
    mut eat_writer: MessageWriter<PlayerEating>,
) {
    for (event, entity) in receiver.0.try_iter() {
        if !state.0.players.is_connected(entity) {
            continue;
        }

        let Ok((
            _,
            identity,
            hotbar,
            mut inventory,
            position,
            rotation,
            velocity,
            on_ground,
            fall_flying,
        )) = players.get_mut(entity)
        else {
            continue;
        };

        trace!(
            "{} used an item with the {} hand",
            identity.username,
            match event.hand {
                Hand::MainHand => "main",
                Hand::OffHand => "off",
            }
        );

        // Playing the swing locally keeps the client in sync: the item-in-air state and the eating
        // animation both stem from it.
        let animation = match event.hand {
            Hand::MainHand => SWING_MAIN_HAND,
            Hand::OffHand => SWING_OFF_HAND,
        };
        let packet = EntityAnimationPacket::new(VarInt::new(identity.short_uuid), animation);
        for (_, conn) in conn_query.iter() {
            if !conn.is_running() {
                continue;
            }
            if let Err(err) = conn.send_packet_ref(&packet) {
                trace!("Failed to broadcast the use-item swing: {err:?}");
            }
        }

        // Throwables leave the hand as projectile entities before anything else gets to use the
        // item: an ender pearl, snowball or egg is thrown, a firework rocket boosts an elytra
        // flight. Anything else falls through to eating.
        let used = projectiles::use_held_item(
            &mut commands,
            &mut inventory,
            hotbar,
            projectiles::Thrower {
                entity,
                protocol_id: identity.short_uuid,
                position,
                rotation,
                velocity,
                on_ground,
                fall_flying: fall_flying.is_some(),
                dimension: Dimension::default(),
            },
            &conn_query,
        );
        if !used {
            // Eating is the only other use with a survival effect: it consumes the stack and
            // emits the event the hunger systems apply later in the tick.
            hunger::eat_selected_item(entity, identity, hotbar, &mut inventory, &mut eat_writer);
        }
    }
}
