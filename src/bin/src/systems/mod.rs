use bevy_ecs::schedule::IntoScheduleConfigs;
pub mod block_updates;
pub mod chests;
mod chunk_calculator;
mod chunk_sending;
pub mod chunk_unloader;
pub mod connection_killer;
pub mod day_cycle;
pub mod drops;
pub mod emit_player_joined;
pub mod fall_damage;
pub mod fall_flying;
pub mod fluids;
pub mod health_system;
pub mod hunger;
pub mod item_entities;
pub mod keep_alive_system;
pub mod lan_pinger;
pub mod listeners;
mod mining;
pub mod mobs;
mod mq;
pub mod new_connections;
mod particles;
pub mod physics;
mod player_swimming;
pub mod projectiles;
mod send_entity_updates;
pub mod shutdown_systems;
pub mod tick_counter;
pub mod tps_broadcast;
pub(crate) mod update_player_ping;
pub mod world_sync;

pub fn register_game_systems(schedule: &mut bevy_ecs::schedule::Schedule) {
    // Tick-bound systems only (run every game tick)
    // NOTE: accept_new_connections is registered separately in game_loop.rs
    // with apply_deferred and emit_player_joined chained after it.
    schedule.add_systems(
        (
            chunk_calculator::handle,
            chunk_sending::handle,
            // chunk_unloader::handle,
        )
            .chain(),
    );
    schedule.add_systems(mq::process);
    schedule.add_systems(player_swimming::detect_player_swimming);

    // Elytra glides end when the client reports being on the ground again. Run after the movement
    // packets have updated OnGround, otherwise the marker is cleared a tick late (or never).
    schedule.add_systems(
        fall_flying::clear_when_grounded
            .after(crate::packet_handlers::play_packets::set_player_position::handle)
            .after(crate::packet_handlers::play_packets::set_player_position_and_rotation::handle),
    );

    // Process scheduled fluid ticks: evaluate spreading, apply, broadcast, re-schedule.
    schedule.add_systems(fluids::seed_on_block_break);
    // Settle generated "hanging" fluids in newly loaded chunks (cave-breached oceans, perched
    // springs) the first time a player is near, mirroring vanilla's settle-on-load.
    schedule.add_systems(fluids::settle_loaded_fluids);
    schedule.add_systems(fluids::process_fluid_ticks);

    schedule.add_systems(send_entity_updates::handle);

    // Item entities are cheap when nothing was dropped, and fall/move/pick-up once per tick.
    schedule.add_systems(item_entities::tick_item_entities);

    // Projectiles are cheap when nothing was thrown, and fly/hit once per tick.
    schedule.add_systems(projectiles::tick_projectiles);

    // The client reports landing, so the fall is charged from the height it reached.
    schedule.add_systems(fall_damage::apply_fall_damage);

    // A chest that breaks drops what it held, so this runs after both mining paths have written
    // their block-broken event: survival digging and the creative instant break.
    schedule.add_systems(
        chests::drop_chest_contents_on_break
            .after(crate::systems::listeners::digging_system::handle_finish_digging)
            .after(crate::packet_handlers::play_packets::player_action::handle),
    );

    schedule.add_systems(day_cycle::tick_daylight_cycle);

    // Stream the measured server TPS to clients (shown on F3) once per second.
    schedule.add_systems(tps_broadcast::broadcast_tps);

    // Should always be last
    schedule.add_systems(connection_killer::connection_killer);
    schedule.add_systems(particles::handle);
}
