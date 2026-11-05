use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule};

mod change_game_mode;
mod chat_message;
mod chunk_batch_ack;
mod client_command;
mod command;
mod command_suggestions;
mod confirm_player_teleport;
mod interact;
mod keep_alive;
mod pick_item_from_block;
pub mod place_block;
mod player_abilities;
pub(crate) mod player_action;
mod player_command;
pub mod player_input;
mod player_loaded;
pub(crate) mod set_player_position;
pub(crate) mod set_player_position_and_rotation;
mod set_player_rotation;
mod swing_arm;

pub fn register_packet_handlers(schedule: &mut Schedule) {
    // Added separately so if we mess up the signature of one of the systems we can know exactly
    // which one
    schedule.add_systems(chunk_batch_ack::handle);
    schedule.add_systems(confirm_player_teleport::handle);
    schedule.add_systems(keep_alive::handle);
    schedule.add_systems(place_block::handle);
    schedule.add_systems(player_action::handle);
    schedule.add_systems(player_command::handle);
    schedule.add_systems(player_input::handle);
    schedule.add_systems(set_player_position::handle);
    schedule.add_systems(set_player_position_and_rotation::handle);
    schedule.add_systems(set_player_rotation::handle);
    schedule.add_systems(swing_arm::handle);
    schedule.add_systems(update_survival_mode_slot::handle);
    schedule.add_systems(close_container::handle);
    schedule.add_systems(player_loaded::handle);
    schedule.add_systems(command::handle);
    schedule.add_systems(command_suggestions::handle);
    schedule.add_systems(chat_message::handle);
    schedule.add_systems(set_creative_mode_slot::handle);
    schedule.add_systems(set_held_item::handle);
    schedule.add_systems(player_abilities::handle);
    schedule.add_systems(change_game_mode::handle);
    schedule.add_systems(pick_item_from_block::handle);
    schedule.add_systems(interact::handle);
    schedule.add_systems(client_command::handle);
    schedule.add_systems(use_item::handle);

    // Combat and eating are chains of events, not single systems: the damage has to be applied
    // before the death is announced, and the eaten food has to be applied before the tick ends.
    // Ordering matters because messages only reach their readers once the writer has run.
    schedule.add_systems(crate::systems::health_system::apply_damage.after(interact::handle));
    schedule.add_systems(
        crate::systems::listeners::player_death::handle
            .after(crate::systems::health_system::apply_damage),
    );
    schedule.add_systems(crate::systems::hunger::apply_eaten_food.after(use_item::handle));
    // Exhaustion is folded in before the drain rules run, so a block mined this tick can already
    // push the player past a saturation threshold.
    schedule.add_systems(
        crate::systems::hunger::apply_exhaustion
            .after(crate::systems::listeners::digging_system::handle_finish_digging),
    );
    schedule.add_systems(
        crate::systems::hunger::drain_hunger.after(crate::systems::hunger::apply_exhaustion),
    );
}

pub mod set_creative_mode_slot;

mod close_container;
pub mod set_held_item;
mod update_survival_mode_slot;
mod use_item;
