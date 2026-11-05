pub mod digging_system;
pub mod entity_spawn;
pub mod gamemode_change;
pub mod player_death;
pub mod player_despawn;
pub mod player_join_message;
pub mod player_leave_message;
pub mod player_spawn;
pub mod player_tp;

pub fn register_gameplay_listeners(schedule: &mut bevy_ecs::schedule::Schedule) {
    use bevy_ecs::schedule::{ApplyDeferred, IntoScheduleConfigs};

    schedule.add_systems(player_leave_message::handle);
    schedule.add_systems(player_join_message::handle);
    schedule.add_systems(player_spawn::handle);
    schedule.add_systems(player_despawn::handle);
    schedule.add_systems(gamemode_change::handle);
    schedule.add_systems(entity_spawn::spawn_command_processor);
    schedule.add_systems(entity_spawn::handle_spawn_entity);

    // The digging handlers have to run in this order, with the deferred component changes flushed
    // in between. The client can send several of them in one tick: a block that breaks in a single
    // tick sends START and FINISH together, and switching targets sends ABORT for the old block
    // right before START for the new one. Without the sync points the cancel would read the old dig
    // and the finish would not see the new one, dropping the break with "finished digging without
    // starting".
    schedule.add_systems(
        (
            digging_system::handle_start_digging,
            ApplyDeferred,
            digging_system::handle_cancel_digging,
            ApplyDeferred,
            digging_system::handle_finish_digging,
        )
            .chain(),
    );
    schedule.add_systems(player_tp::teleport_player);
}
