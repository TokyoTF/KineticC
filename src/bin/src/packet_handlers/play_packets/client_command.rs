//! Handles the Client Command packet.
//!
//! The only action the server acts on is `PerformRespawn`: the client asks to leave the death
//! screen. Everything the client needs to continue playing is re-sent from scratch - the respawn
//! packet switches the dimension state, a teleport puts the player back at the world spawn, and the
//! chunk calculation is reset so the surrounding chunks are streamed again.

use bevy_ecs::prelude::{Commands, Entity, MessageWriter, Query, Res, With};
use bevy_math::DVec3;
use ferrumc_components::health::Health;
use ferrumc_components::player::gamemode::GameModeComponent;
use ferrumc_components::player::gameplay_state::dead::Dead;
use ferrumc_core::identity::player_identity::PlayerIdentity;
use ferrumc_macros::block;
use ferrumc_messages::chunk_calc::ChunkCalc;
use ferrumc_messages::teleport_player::TeleportPlayer;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::respawn::RespawnPacket;
use ferrumc_state::GlobalStateResource;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::dimension::Dimension;
use ferrumc_world::pos::{ChunkBlockPos, ChunkPos};
use tracing::{debug, trace, warn};

/// Chunk a respawned player is placed in.
const SPAWN_CHUNK: (i32, i32) = (0, 0);
/// Column inside the spawn chunk that is scanned for the spawn height.
const SPAWN_COLUMN: (u8, u8) = (8, 8);
/// Height a respawned player is placed at when the spawn chunk cannot be scanned.
const FALLBACK_SPAWN_HEIGHT: f64 = 64.0;

/// The query used to reach a dead player's connection and gamemode.
type RespawnTargetQuery<'a> = (Entity, &'a GameModeComponent, &'a StreamWriter);

pub fn handle(
    receiver: Res<ferrumc_net::ClientCommandReceiver>,
    state: Res<GlobalStateResource>,
    dead_players: Query<RespawnTargetQuery, With<Dead>>,
    identity_query: Query<&PlayerIdentity>,
    mut commands: Commands,
    mut teleport_writer: MessageWriter<TeleportPlayer>,
    mut chunk_calc_writer: MessageWriter<ChunkCalc>,
) {
    for (event, entity) in receiver.0.try_iter() {
        if !event.is_respawn_request() {
            trace!("Ignoring client command action {:?}", event.action);
            continue;
        }

        let Ok((_, gamemode, conn)) = dead_players.get(entity) else {
            trace!("Ignoring respawn request from {entity:?}: the player is not dead");
            continue;
        };

        let Ok(identity) = identity_query.get(entity) else {
            warn!("Ignoring respawn request from {entity:?}: the player has no identity");
            continue;
        };

        if let Err(err) = conn.send_packet(RespawnPacket::same_dimension(gamemode.0)) {
            warn!(
                "Failed to send respawn packet to {}: {err:?}",
                identity.username
            );
            continue;
        }

        let spawn = world_spawn_position(&state);
        teleport_writer.write(TeleportPlayer {
            entity,
            x: spawn.x,
            y: spawn.y,
            z: spawn.z,
            vel_x: 0.0,
            vel_y: 0.0,
            vel_z: 0.0,
            yaw: 0.0,
            pitch: 0.0,
        });
        chunk_calc_writer.write(ChunkCalc(entity));

        commands.entity(entity).remove::<Dead>();
        commands.entity(entity).insert(Health::default());

        debug!("Respawned {} at {:?}", identity.username, spawn);
    }
}

/// Position of the block a respawned player stands on, centred in the block above the surface.
///
/// The spawn chunk is generated on demand when it does not exist yet, and its centre column is
/// scanned from the top down for the first solid block. Fluids are skipped so a respawn always ends
/// up on dry ground rather than inside an ocean.
fn world_spawn_position(state: &Res<GlobalStateResource>) -> DVec3 {
    let chunk_pos = ChunkPos::new(SPAWN_CHUNK.0, SPAWN_CHUNK.1);
    let chunk = match ferrumc_utils::world::load_or_generate_chunk(
        &state.0,
        chunk_pos,
        Dimension::default().as_str(),
    ) {
        Ok(chunk) => chunk,
        Err(err) => {
            warn!("Failed to load the spawn chunk for respawn: {err:?}");
            return column_centre(FALLBACK_SPAWN_HEIGHT);
        }
    };

    let height = chunk.dimensions();
    for y in height.iter().rev() {
        let block = chunk.get_block(ChunkBlockPos::new(SPAWN_COLUMN.0, y, SPAWN_COLUMN.1));
        if is_spawnable_surface(&block) {
            return column_centre(f64::from(y) + 1.0);
        }
    }

    column_centre(f64::from(height.max_y()) + 1.0)
}

/// Whether a block can carry a respawned player.
fn is_spawnable_surface(block: &BlockStateId) -> bool {
    let air = block!("air");
    let void_air = block!("void_air");
    *block != air && *block != void_air && !ferrumc_world::fluid::is_fluid(*block)
}

/// Centre of the spawn column at the given height, in world coordinates.
fn column_centre(height: f64) -> DVec3 {
    DVec3::new(
        f64::from(SPAWN_CHUNK.0 * 16 + i32::from(SPAWN_COLUMN.0)) + 0.5,
        height,
        f64::from(SPAWN_CHUNK.1 * 16 + i32::from(SPAWN_COLUMN.1)) + 0.5,
    )
}
