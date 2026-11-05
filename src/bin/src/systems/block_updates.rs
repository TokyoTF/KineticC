//! Neighbour-aware block states.
//!
//! Vanilla recomputes a block's state through `updateShape` whenever a neighbour changes. The
//! server does not run that pipeline for every world edit yet, so placement (and block breaking)
//! computes the affected states here instead, following the vanilla rules in
//! `FenceBlock`, `RedStoneWireBlock`, `WallTorchBlock` and `StandingAndWallBlockItem` (1.21.8).

use std::collections::BTreeMap;

use bevy_ecs::prelude::{Commands, Entity, Query};
use ferrumc_core::transform::rotation::Rotation;
use ferrumc_net::connection::StreamWriter;
use ferrumc_state::GlobalState;
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::dimension::Dimension;
use ferrumc_world::pos::BlockPos;
use ferrumc_world::vanilla_chunk_format::BlockData;

/// Every player connection, with the entity it belongs to. Same shape as the connection query
/// the item-drop code uses, so `refresh_around` can hand it to [`crate::systems::item_entities`].
type ConnQuery<'a> = (Entity, &'a StreamWriter);

/// Face ids match the ones the `use_item_on` packet uses: 0 = down, 1 = up, 2 = north,
/// 3 = south, 4 = west, 5 = east.
const DOWN: i8 = 0;
const UP: i8 = 1;
const NORTH: i8 = 2;
const SOUTH: i8 = 3;
const WEST: i8 = 4;
const EAST: i8 = 5;

const HORIZONTAL: [i8; 4] = [NORTH, SOUTH, EAST, WEST];

/// `Direction.getOpposite`.
const fn opposite(face: i8) -> i8 {
    match face {
        DOWN => UP,
        UP => DOWN,
        NORTH => SOUTH,
        SOUTH => NORTH,
        WEST => EAST,
        EAST => WEST,
        _ => DOWN,
    }
}

/// The block offset of a face.
const fn offset_of(face: i8) -> (i32, i32, i32) {
    match face {
        DOWN => (0, -1, 0),
        UP => (0, 1, 0),
        NORTH => (0, 0, -1),
        SOUTH => (0, 0, 1),
        WEST => (-1, 0, 0),
        EAST => (1, 0, 0),
        _ => (0, 0, 0),
    }
}

const fn is_horizontal(face: i8) -> bool {
    face >= NORTH
}

/// `Direction.getClockWise` for horizontal faces.
const fn clockwise(face: i8) -> i8 {
    match face {
        NORTH => EAST,
        EAST => SOUTH,
        SOUTH => WEST,
        WEST => NORTH,
        _ => face,
    }
}

/// `Direction.getCounterClockWise` for horizontal faces.
const fn counter_clockwise(face: i8) -> i8 {
    match face {
        NORTH => WEST,
        WEST => SOUTH,
        SOUTH => EAST,
        EAST => NORTH,
        _ => face,
    }
}

/// `Direction.getStepX`.
const fn step_x(face: i8) -> i32 {
    match face {
        EAST => 1,
        WEST => -1,
        _ => 0,
    }
}

/// `Direction.getStepZ`.
const fn step_z(face: i8) -> i32 {
    match face {
        SOUTH => 1,
        NORTH => -1,
        _ => 0,
    }
}

/// `Direction.fromYRot`: the horizontal direction a player with this yaw is facing.
fn yaw_direction(yaw: f32) -> i8 {
    match (yaw / 90.0 + 0.5).floor() as i32 & 3 {
        0 => SOUTH,
        1 => WEST,
        2 => NORTH,
        _ => EAST,
    }
}

/// 1 for the x axis, 2 for the z axis (the y axis is never used here).
const fn axis_of(face: i8) -> u8 {
    match face {
        WEST | EAST => 1,
        _ => 2,
    }
}

const fn face_name(face: i8) -> &'static str {
    match face {
        DOWN => "down",
        UP => "up",
        NORTH => "north",
        SOUTH => "south",
        WEST => "west",
        _ => "east",
    }
}

fn face_from_name(name: &str) -> Option<i8> {
    match name {
        "down" => Some(DOWN),
        "up" => Some(UP),
        "north" => Some(NORTH),
        "south" => Some(SOUTH),
        "west" => Some(WEST),
        "east" => Some(EAST),
        _ => None,
    }
}

/// `Direction.orderedByNearest(Entity)`: the six directions sorted by how closely the player is
/// looking at them. Used to pick a torch's orientation, exactly like vanilla.
fn ordered_looking_directions(yaw: f32, pitch: f32) -> [i8; 6] {
    let f = pitch.to_radians();
    let f1 = -yaw.to_radians();
    let f2 = f.sin();
    let f3 = f.cos();
    let f4 = f1.sin();
    let f5 = f1.cos();
    let east = f4 > 0.0;
    let up = f2 < 0.0;
    let south = f5 > 0.0;
    let f6 = if east { f4 } else { -f4 };
    let f7 = if up { -f2 } else { f2 };
    let f8 = if south { f5 } else { -f5 };
    let f9 = f6 * f3;
    let f10 = f8 * f3;
    let direction = if east { EAST } else { WEST };
    let direction1 = if up { UP } else { DOWN };
    let direction2 = if south { SOUTH } else { NORTH };

    let (first, second, third) = if f6 > f8 {
        if f7 > f9 {
            (direction1, direction, direction2)
        } else if f10 > f7 {
            (direction, direction2, direction1)
        } else {
            (direction, direction1, direction2)
        }
    } else if f7 > f10 {
        (direction1, direction2, direction)
    } else if f9 > f7 {
        (direction2, direction, direction1)
    } else {
        (direction2, direction1, direction)
    };

    [
        first,
        second,
        third,
        opposite(third),
        opposite(second),
        opposite(first),
    ]
}

/// A fallback order that starts at the clicked face, for when the player rotation is unknown.
fn face_order(face: i8) -> [i8; 6] {
    let first = if face == DOWN { UP } else { face };
    [
        first,
        UP,
        NORTH,
        SOUTH,
        WEST,
        EAST,
    ]
}

fn prop<'a>(props: Option<&'a BTreeMap<String, String>>, key: &str) -> Option<&'a str> {
    props.and_then(|map| map.get(key)).map(String::as_str)
}

fn make_block_data(name: &str, props: &[(&str, &str)]) -> BlockData {
    if props.is_empty() {
        return BlockData {
            name: name.to_string(),
            properties: None,
        };
    }
    let mut map = BTreeMap::new();
    for (key, value) in props {
        map.insert((*key).to_string(), (*value).to_string());
    }
    BlockData {
        name: name.to_string(),
        properties: Some(map),
    }
}

fn block_data_at(
    state: &GlobalState,
    pos: BlockPos,
    dimension: &str,
) -> Option<BlockData> {
    state
        .world
        .get_block_and_fetch(pos, dimension)
        .ok()
        .and_then(|id| id.to_block_data())
}

fn write_block(state: &GlobalState, dimension: &str, pos: BlockPos, block: BlockStateId) -> bool {
    match ferrumc_utils::world::load_or_generate_mut(state, pos.chunk(), dimension) {
        Ok(mut chunk) => {
            chunk.set_block(pos.chunk_block_pos(), block);
            true
        }
        Err(_) => false,
    }
}

fn short_name(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn is_fluid(name: &str) -> bool {
    matches!(
        name,
        "minecraft:water" | "minecraft:lava" | "minecraft:bubble_column"
    )
}

fn is_fence(name: &str) -> bool {
    short_name(name).ends_with("_fence")
}

fn is_fence_gate(name: &str) -> bool {
    short_name(name).ends_with("_fence_gate")
}

fn is_wall(name: &str) -> bool {
    short_name(name).ends_with("_wall")
}

fn is_pane(name: &str) -> bool {
    let short = short_name(name);
    short.ends_with("_pane") || short.ends_with("_bars")
}

fn is_slab(name: &str) -> bool {
    short_name(name).ends_with("_slab")
}

fn is_stairs(name: &str) -> bool {
    short_name(name).ends_with("_stairs")
}

fn is_trapdoor(name: &str) -> bool {
    short_name(name).ends_with("_trapdoor")
}

fn is_door(name: &str) -> bool {
    short_name(name).ends_with("_door")
}

/// `DoorBlock.type().canOpenByHand()`: every door can be opened by hand except the iron one
/// (the gold block set is not used by any door). Copper doors can be opened by hand.
fn can_open_by_hand(name: &str) -> bool {
    short_name(name) != "iron_door"
}

/// `BlockState.canBeReplaced()`: the block is air, a fluid, or a plant that gets replaced when a
/// block is placed into its cell. Used for the cell above a door.
fn can_be_replaced(name: &str) -> bool {
    if is_air(name) || is_fluid(name) {
        return true;
    }
    let short = short_name(name);
    short.ends_with("_grass")
        || short == "grass"
        || short.ends_with("_fern")
        || short == "fern"
        || short.ends_with("_flower")
        || short.ends_with("_sapling")
        || short.ends_with("_mushroom")
        || short.ends_with("_roots")
        || short.ends_with("_sprouts")
        || short.ends_with("_bush")
        || short.ends_with("_petals")
        || matches!(
            short,
            "snow"
                | "fire"
                | "soul_fire"
                | "vine"
                | "wheat"
                | "carrots"
                | "potatoes"
                | "beetroots"
                | "seagrass"
                | "tall_seagrass"
                | "structure_void"
                | "light"
        )
}

/// Blocks vanilla registers with `BlockBehaviour.Properties.noCollission()` (Blocks.java,
/// 1.21.8), plus the four stems (registered through `net.minecraft.references.Blocks` constants).
/// `cobweb` and `scaffolding` are also registered with `noCollission` but override
/// `getCollisionShape` with a non-empty shape, so they are deliberately not listed.
static NO_COLLISION_BLOCKS: &[&str] = &[
    "minecraft:acacia_hanging_sign", "minecraft:acacia_pressure_plate", "minecraft:acacia_sapling",
    "minecraft:acacia_sign", "minecraft:acacia_wall_hanging_sign", "minecraft:acacia_wall_sign",
    "minecraft:activator_rail", "minecraft:air", "minecraft:allium",
    "minecraft:attached_melon_stem", "minecraft:attached_pumpkin_stem", "minecraft:azure_bluet",
    "minecraft:bamboo_hanging_sign", "minecraft:bamboo_pressure_plate", "minecraft:bamboo_sapling",
    "minecraft:bamboo_sign", "minecraft:bamboo_wall_hanging_sign", "minecraft:bamboo_wall_sign",
    "minecraft:beetroots", "minecraft:big_dripleaf_stem", "minecraft:birch_hanging_sign",
    "minecraft:birch_pressure_plate", "minecraft:birch_sapling", "minecraft:birch_sign",
    "minecraft:birch_wall_hanging_sign", "minecraft:birch_wall_sign", "minecraft:black_banner",
    "minecraft:black_wall_banner", "minecraft:blue_banner", "minecraft:blue_orchid",
    "minecraft:blue_wall_banner", "minecraft:brain_coral", "minecraft:brain_coral_fan",
    "minecraft:brain_coral_wall_fan", "minecraft:brown_banner", "minecraft:brown_mushroom",
    "minecraft:brown_wall_banner", "minecraft:bubble_column", "minecraft:bubble_coral",
    "minecraft:bubble_coral_fan", "minecraft:bubble_coral_wall_fan", "minecraft:bush",
    "minecraft:cactus_flower", "minecraft:carrots", "minecraft:cave_air", "minecraft:cave_vines",
    "minecraft:cave_vines_plant", "minecraft:cherry_hanging_sign",
    "minecraft:cherry_pressure_plate", "minecraft:cherry_sapling", "minecraft:cherry_sign",
    "minecraft:cherry_wall_hanging_sign", "minecraft:cherry_wall_sign",
    "minecraft:closed_eyeblossom", "minecraft:cornflower",
    "minecraft:crimson_fungus", "minecraft:crimson_hanging_sign",
    "minecraft:crimson_pressure_plate", "minecraft:crimson_roots", "minecraft:crimson_sign",
    "minecraft:crimson_wall_hanging_sign", "minecraft:crimson_wall_sign", "minecraft:cyan_banner",
    "minecraft:cyan_wall_banner", "minecraft:dandelion", "minecraft:dark_oak_hanging_sign",
    "minecraft:dark_oak_pressure_plate", "minecraft:dark_oak_sapling", "minecraft:dark_oak_sign",
    "minecraft:dark_oak_wall_hanging_sign", "minecraft:dark_oak_wall_sign",
    "minecraft:dead_brain_coral", "minecraft:dead_brain_coral_fan",
    "minecraft:dead_brain_coral_wall_fan", "minecraft:dead_bubble_coral",
    "minecraft:dead_bubble_coral_fan", "minecraft:dead_bubble_coral_wall_fan",
    "minecraft:dead_bush", "minecraft:dead_fire_coral", "minecraft:dead_fire_coral_fan",
    "minecraft:dead_fire_coral_wall_fan", "minecraft:dead_horn_coral",
    "minecraft:dead_horn_coral_fan", "minecraft:dead_horn_coral_wall_fan",
    "minecraft:dead_tube_coral", "minecraft:dead_tube_coral_fan",
    "minecraft:dead_tube_coral_wall_fan", "minecraft:detector_rail", "minecraft:end_gateway",
    "minecraft:end_portal", "minecraft:fern", "minecraft:fire", "minecraft:fire_coral",
    "minecraft:fire_coral_fan", "minecraft:fire_coral_wall_fan", "minecraft:firefly_bush",
    "minecraft:frogspawn", "minecraft:glow_lichen", "minecraft:gray_banner",
    "minecraft:gray_wall_banner", "minecraft:green_banner", "minecraft:green_wall_banner",
    "minecraft:hanging_roots", "minecraft:heavy_weighted_pressure_plate", "minecraft:horn_coral",
    "minecraft:horn_coral_fan", "minecraft:horn_coral_wall_fan", "minecraft:jungle_hanging_sign",
    "minecraft:jungle_pressure_plate", "minecraft:jungle_sapling", "minecraft:jungle_sign",
    "minecraft:jungle_wall_hanging_sign", "minecraft:jungle_wall_sign", "minecraft:kelp",
    "minecraft:kelp_plant", "minecraft:large_fern", "minecraft:lava", "minecraft:leaf_litter",
    "minecraft:lever", "minecraft:light_blue_banner", "minecraft:light_blue_wall_banner",
    "minecraft:light_gray_banner", "minecraft:light_gray_wall_banner",
    "minecraft:light_weighted_pressure_plate", "minecraft:lilac", "minecraft:lily_of_the_valley",
    "minecraft:lime_banner", "minecraft:lime_wall_banner", "minecraft:magenta_banner",
    "minecraft:magenta_wall_banner", "minecraft:mangrove_hanging_sign",
    "minecraft:mangrove_pressure_plate", "minecraft:mangrove_propagule", "minecraft:mangrove_sign",
    "minecraft:mangrove_wall_hanging_sign", "minecraft:mangrove_wall_sign", "minecraft:melon_stem",
    "minecraft:nether_portal", "minecraft:nether_sprouts", "minecraft:nether_wart",
    "minecraft:oak_hanging_sign", "minecraft:oak_pressure_plate", "minecraft:oak_sapling",
    "minecraft:oak_sign", "minecraft:oak_wall_hanging_sign", "minecraft:oak_wall_sign",
    "minecraft:open_eyeblossom", "minecraft:orange_banner", "minecraft:orange_tulip",
    "minecraft:orange_wall_banner", "minecraft:oxeye_daisy", "minecraft:pale_hanging_moss",
    "minecraft:pale_oak_hanging_sign", "minecraft:pale_oak_pressure_plate",
    "minecraft:pale_oak_sapling", "minecraft:pale_oak_sign", "minecraft:pale_oak_wall_hanging_sign",
    "minecraft:pale_oak_wall_sign", "minecraft:peony", "minecraft:pink_banner",
    "minecraft:pink_petals", "minecraft:pink_tulip", "minecraft:pink_wall_banner",
    "minecraft:pitcher_crop", "minecraft:pitcher_plant",
    "minecraft:polished_blackstone_pressure_plate", "minecraft:poppy", "minecraft:potatoes",
    "minecraft:powered_rail", "minecraft:pumpkin_stem", "minecraft:purple_banner",
    "minecraft:purple_wall_banner", "minecraft:rail", "minecraft:red_banner",
    "minecraft:red_mushroom", "minecraft:red_tulip", "minecraft:red_wall_banner",
    "minecraft:redstone_torch", "minecraft:redstone_wall_torch", "minecraft:redstone_wire",
    "minecraft:resin_clump", "minecraft:rose_bush", "minecraft:sculk_vein",
    "minecraft:seagrass", "minecraft:short_dry_grass", "minecraft:short_grass",
    "minecraft:small_dripleaf", "minecraft:soul_fire", "minecraft:soul_torch",
    "minecraft:soul_wall_torch", "minecraft:spore_blossom", "minecraft:spruce_hanging_sign",
    "minecraft:spruce_pressure_plate", "minecraft:spruce_sapling", "minecraft:spruce_sign",
    "minecraft:spruce_wall_hanging_sign", "minecraft:spruce_wall_sign",
    "minecraft:stone_pressure_plate", "minecraft:structure_void", "minecraft:sugar_cane",
    "minecraft:sunflower", "minecraft:sweet_berry_bush", "minecraft:tall_dry_grass",
    "minecraft:tall_grass", "minecraft:tall_seagrass", "minecraft:torch", "minecraft:torchflower",
    "minecraft:torchflower_crop", "minecraft:tripwire", "minecraft:tripwire_hook",
    "minecraft:tube_coral", "minecraft:tube_coral_fan", "minecraft:tube_coral_wall_fan",
    "minecraft:twisting_vines", "minecraft:twisting_vines_plant", "minecraft:vine",
    "minecraft:void_air", "minecraft:wall_torch", "minecraft:warped_fungus",
    "minecraft:warped_hanging_sign", "minecraft:warped_pressure_plate", "minecraft:warped_roots",
    "minecraft:warped_sign", "minecraft:warped_wall_hanging_sign", "minecraft:warped_wall_sign",
    "minecraft:water", "minecraft:weeping_vines", "minecraft:weeping_vines_plant",
    "minecraft:wheat", "minecraft:white_banner", "minecraft:white_tulip",
    "minecraft:white_wall_banner", "minecraft:wildflowers", "minecraft:wither_rose",
    "minecraft:yellow_banner", "minecraft:yellow_wall_banner",
];

/// Whether a block's collision shape is empty, i.e. vanilla `CollisionGetter.isUnobstructed`
/// returns true for it no matter which entities are in its cell. A block with an empty
/// collision shape can be placed inside the player's own cell: redstone dust on the ground
/// under the player's feet, a torch at their feet, a flower where they stand.
pub fn has_empty_collision(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    // Buttons are registered through `buttonProperties()`, which is `noCollission()`.
    if NO_COLLISION_BLOCKS.contains(&name) || short_name(name).ends_with("_button") {
        return true;
    }
    // `SnowLayerBlock`: only the first layer has an empty collision shape (SHAPES[0]).
    short_name(name) == "snow" && prop(props, "layers") == Some("1")
}

fn is_carpet(name: &str) -> bool {
    short_name(name).ends_with("_carpet")
}

fn is_redstone_wire(name: &str) -> bool {
    name == "minecraft:redstone_wire"
}

fn is_wooden_fence(name: &str) -> bool {
    use ferrumc_data::generated::tags::TagData;
    TagData::BLOCK_TAGS_MINECRAFT_WOODEN_FENCES
        .values
        .contains(&name)
}

/// True when the block is a full cube, i.e. `isCollisionShapeFullBlock` / `isFaceSturdy(FULL)`.
///
/// The server only ships block names and state properties, not shapes, so the few non-cube
/// families are excluded by name. Everything that is not listed here is treated as a full cube,
/// which is correct for the overwhelming majority of blocks (stone, planks, glass, logs, ...).
fn is_full_cube(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    if is_air(name) || is_fluid(name) {
        return false;
    }

    let short = short_name(name);

    // Slabs only fill a whole face when doubled.
    if is_slab(name) {
        return matches!(prop(props, "type"), Some("double"));
    }

    if is_stairs(name)
        || is_fence(name)
        || is_fence_gate(name)
        || is_wall(name)
        || is_pane(name)
        || short.ends_with("torch")
        || short.ends_with("_door")
        || short.ends_with("_trapdoor")
        || short.ends_with("_button")
        || short.ends_with("_pressure_plate")
        || short.ends_with("_sign")
        || short.ends_with("_banner")
        || short.ends_with("_bed")
        || short.ends_with("_carpet")
        || short.ends_with("_rail")
        || short.ends_with("_bud")
        || short.ends_with("_coral")
        || short.ends_with("_coral_fan")
        || short.ends_with("_coral_wall_fan")
        || short.ends_with("_head")
        || short.ends_with("_skull")
        || short.ends_with("_candle")
        || short.ends_with("_lantern")
        || short.ends_with("_chain")
        || short.ends_with("_anvil")
        || short.ends_with("_cauldron")
        || short.ends_with("_chest")
        || short.ends_with("_barrel")
        || short.ends_with("_shulker_box")
        || short.ends_with("_sapling")
        || short.ends_with("_mushroom")
        || short.ends_with("_flower")
        || short.ends_with("_roots")
        || short.ends_with("_vines")
        || short.ends_with("_vines_plant")
        || short.ends_with("_wart")
        || short.ends_with("_bush")
        || short.ends_with("_sprouts")
        || short.ends_with("_fern")
    {
        return false;
    }

    !matches!(
        short,
        "air"
            | "cave_air"
            | "void_air"
            | "water"
            | "lava"
            | "bubble_column"
            | "redstone_wire"
            | "repeater"
            | "comparator"
            | "lever"
            | "tripwire"
            | "tripwire_hook"
            | "target"
            | "daylight_detector"
            | "observer"
            | "hopper"
            | "chest"
            | "barrel"
            | "chain"
            | "anvil"
            | "cauldron"
            | "shulker_box"
            | "composter"
            | "jukebox"
            | "lectern"
            | "loom"
            | "stonecutter"
            | "grindstone"
            | "bell"
            | "brewing_stand"
            | "enchanting_table"
            | "end_portal_frame"
            | "fire"
            | "soul_fire"
            | "frosted_ice"
            | "dragon_egg"
            | "turtle_egg"
            | "cake"
            | "candle_cake"
            | "ladder"
            | "vine"
            | "scaffolding"
            | "lily_pad"
            | "sea_pickle"
            | "snow"
            | "cocoa"
            | "sculk_sensor"
            | "calibrated_sculk_sensor"
            | "piston_head"
            | "moving_piston"
            | "nether_portal"
            | "end_portal"
            | "end_gateway"
            | "cobweb"
            | "sweet_berry_bush"
            | "chorus_flower"
            | "chorus_plant"
            | "short_grass"
            | "grass"
            | "tall_grass"
            | "large_fern"
            | "dead_bush"
            | "dandelion"
            | "poppy"
            | "blue_orchid"
            | "allium"
            | "azure_bluet"
            | "red_tulip"
            | "orange_tulip"
            | "white_tulip"
            | "pink_tulip"
            | "oxeye_daisy"
            | "cornflower"
            | "lily_of_the_valley"
            | "wither_rose"
            | "torchflower"
            | "sunflower"
            | "lilac"
            | "rose_bush"
            | "peony"
            | "brown_mushroom"
            | "red_mushroom"
            | "crimson_fungus"
            | "warped_fungus"
            | "nether_sprouts"
            | "nether_wart"
            | "wheat"
            | "carrots"
            | "potatoes"
            | "beetroots"
            | "sugar_cane"
            | "kelp"
            | "kelp_plant"
            | "seagrass"
            | "tall_seagrass"
            | "big_dripleaf"
            | "small_dripleaf"
            | "hanging_roots"
            | "glow_lichen"
            | "sculk_vein"
            | "azalea"
            | "flowering_azalea"
            | "pointed_dripstone"
            | "amethyst_cluster"
            | "spore_blossom"
            | "pink_petals"
            | "torchflower_crop"
            | "pitcher_crop"
            | "pitcher_plant"
            | "dried_ghast"
    )
}

/// `BlockBehaviour.Properties.isRedstoneConductor` defaults to "collision shape is a full block".
fn is_redstone_conductor(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    is_full_cube(name, props)
}

/// `BlockState.isFaceSturdy(level, pos, Direction.UP)`, for the support of redstone wire and the
/// lower half of a door.
fn has_sturdy_top(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    if is_full_cube(name, props) {
        return true;
    }
    // Slabs always present a full top face (at y = 0.5 or 1.0).
    if is_slab(name) {
        return true;
    }
    // An upside-down stair presents a full top face.
    if is_stairs(name) {
        return matches!(prop(props, "half"), Some("top"));
    }
    if short_name(name) == "snow" {
        return prop(props, "layers") == Some("8");
    }
    false
}

/// `RedStoneWireBlock.canSurviveOn`: a sturdy top face, or a hopper.
fn supports_redstone_on(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    short_name(name) == "hopper" || has_sturdy_top(name, props)
}

/// `SupportType.CENTER` support, used by standing torches.
fn supports_torch_center(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    if is_air(name) || is_fluid(name) {
        return false;
    }
    supports_redstone_on(name, props)
        || is_fence(name)
        || is_wall(name)
        || is_pane(name)
        || is_slab(name)
        || is_stairs(name)
        || is_carpet(name)
}

/// `BlockState.isFaceSturdy(level, pos, Direction.DOWN)`: a full bottom face, used by blocks
/// that hang from the block above (hanging signs, lanterns, vines...).
fn has_sturdy_bottom(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    if is_full_cube(name, props) {
        return true;
    }
    // Slabs always present a full bottom face (at y = 0 or 0.5).
    if is_slab(name) {
        return true;
    }
    if is_stairs(name) {
        return matches!(prop(props, "half"), Some("bottom"));
    }
    // Any snow layer has a full bottom face.
    short_name(name) == "snow"
}

/// `BlockState.isSolid()`: a non-air block with a non-empty collision shape.
fn is_solid_block(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    !is_air(name) && !is_fluid(name) && !has_empty_collision(name, props)
}

/// Offset of a `Direction` constant (DOWN..EAST).
fn face_offset(face: i8) -> (i32, i32, i32) {
    match face {
        DOWN => (0, -1, 0),
        UP => (0, 1, 0),
        NORTH => (0, 0, -1),
        SOUTH => (0, 0, 1),
        WEST => (-1, 0, 0),
        _ => (1, 0, 0), // EAST
    }
}

/// `BlockState.isFaceSturdy` on a horizontal face, approximated: full cubes, fences, walls and
/// panes are valid attachment points for wall torches, ladders, wall signs, etc.
fn is_attachable(name: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    is_full_cube(name, props) || is_fence(name) || is_wall(name) || is_pane(name)
}

fn in_tag(tag: &ferrumc_data::generated::tags::TagData, name: &str) -> bool {
    tag.values.contains(&name)
}

/// The block a `facing`-property block is attached to (the opposite of its facing), and
/// whether that block can hold it. `None` (unreadable world) counts as attached.
fn side_attached(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    props: Option<&BTreeMap<String, String>>,
) -> bool {
    let Some(face) = prop(props, "facing").and_then(face_from_name) else {
        return true;
    };
    let offset = face_offset(opposite(face));
    block_data_at(state, pos + offset, dimension)
        .is_none_or(|b| is_attachable(&b.name, b.properties.as_ref()))
}

/// `Block.isExceptionForConnection`: blocks that never take part in fence/wall connections.
fn is_exception_for_connection(name: &str) -> bool {
    let short = short_name(name);
    short.ends_with("_leaves")
        || matches!(
            short,
            "barrier" | "carved_pumpkin" | "jack_o_lantern" | "melon" | "pumpkin"
        )
        || short.ends_with("_shulker_box")
}

/// `RedStoneWireBlock.shouldConnectTo`.
fn should_connect_to(block: &BlockData, direction: Option<i8>) -> bool {
    let short = short_name(&block.name);
    if short == "redstone_wire" {
        return true;
    }

    let facing = prop(block.properties.as_ref(), "facing");
    match short {
        "repeater" | "comparator" => {
            let Some(direction) = direction else {
                return false;
            };
            facing.is_some_and(|facing| {
                face_from_name(facing) == Some(direction)
                    || face_from_name(facing) == Some(opposite(direction))
            })
        }
        "observer" => {
            let Some(direction) = direction else {
                return false;
            };
            facing.is_some_and(|facing| face_from_name(facing) == Some(direction))
        }
        _ => direction.is_some() && is_signal_source(short, block.properties.as_ref()),
    }
}

/// `BlockState.isSignalSource` for the blocks that can power redstone wire.
fn is_signal_source(short: &str, props: Option<&BTreeMap<String, String>>) -> bool {
    match short {
        "lever"
        | "daylight_detector"
        | "detector_rail"
        | "jukebox"
        | "lectern"
        | "lightning_rod"
        | "observer"
        | "redstone_block"
        | "sculk_sensor"
        | "calibrated_sculk_sensor"
        | "target"
        | "trapped_chest"
        | "tripwire_hook"
        | "repeater"
        | "comparator" => true,
        "redstone_torch" | "redstone_wall_torch" => prop(props, "lit") != Some("false"),
        _ => short.ends_with("_button") || short.ends_with("_pressure_plate"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RedstoneSide {
    None,
    Side,
    Up,
}

impl RedstoneSide {
    const fn is_connected(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Side => "side",
            Self::Up => "up",
        }
    }
}

/// `RedStoneWireBlock.getConnectingSide`.
fn connecting_side(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    face: i8,
    non_normal_cube_above: bool,
) -> RedstoneSide {
    let neighbour_pos = pos + offset_of(face);
    let Some(neighbour) = block_data_at(state, neighbour_pos, dimension) else {
        return RedstoneSide::None;
    };

    if non_normal_cube_above {
        let supports = is_trapdoor(&neighbour.name)
            || supports_redstone_on(&neighbour.name, neighbour.properties.as_ref());
        if supports
            && block_data_at(state, neighbour_pos + (0, 1, 0), dimension)
                .is_some_and(|above| is_redstone_wire(&above.name))
        {
            if is_full_cube(&neighbour.name, neighbour.properties.as_ref()) {
                return RedstoneSide::Up;
            }
            return RedstoneSide::Side;
        }
    }

    let below_connects = block_data_at(state, neighbour_pos + (0, -1, 0), dimension)
        .is_some_and(|below| is_redstone_wire(&below.name));

    if !should_connect_to(&neighbour, Some(face))
        && (is_redstone_conductor(&neighbour.name, neighbour.properties.as_ref())
            || !below_connects)
    {
        RedstoneSide::None
    } else {
        RedstoneSide::Side
    }
}

/// `RedStoneWireBlock.getStateForPlacement` / `getConnectionState`.
///
/// `was_dot` is the right-click "dot" state of an existing wire; a freshly placed wire always
/// starts from the cross state, so pass `false` for placements.
fn place_wire(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    current: &BlockData,
    was_dot: bool,
) -> Option<BlockStateId> {
    let below = block_data_at(state, pos + (0, -1, 0), dimension)?;
    if !supports_redstone_on(&below.name, below.properties.as_ref()) {
        return None;
    }

    let non_normal_cube_above = block_data_at(state, pos + (0, 1, 0), dimension)
        .is_none_or(|above| !is_redstone_conductor(&above.name, above.properties.as_ref()));

    let mut sides = [RedstoneSide::None; 4];
    for (index, face) in HORIZONTAL.iter().enumerate() {
        sides[index] = connecting_side(state, dimension, pos, *face, non_normal_cube_above);
    }

    let is_dot = sides.iter().all(|side| !side.is_connected());
    if !(was_dot && is_dot) {
        let north = sides[0].is_connected();
        let south = sides[1].is_connected();
        let east = sides[2].is_connected();
        let west = sides[3].is_connected();
        let vertical_missing = !north && !south;
        let horizontal_missing = !east && !west;

        if !west && vertical_missing {
            sides[3] = RedstoneSide::Side;
        }
        if !east && vertical_missing {
            sides[2] = RedstoneSide::Side;
        }
        if !north && horizontal_missing {
            sides[0] = RedstoneSide::Side;
        }
        if !south && horizontal_missing {
            sides[1] = RedstoneSide::Side;
        }
    }

    let power = prop(current.properties.as_ref(), "power").unwrap_or("0");
    let props = [
        ("north", sides[0].as_str()),
        ("south", sides[1].as_str()),
        ("east", sides[2].as_str()),
        ("west", sides[3].as_str()),
        ("power", power),
    ];
    Some(BlockStateId::from_block_data(&make_block_data(
        &current.name,
        &props,
    )))
}

/// `FenceBlock.connectsTo`: the same fence family, a matching fence gate, or a solid neighbour.
fn fence_connects(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    face: i8,
    wooden: bool,
) -> bool {
    let neighbour_pos = pos + offset_of(face);
    let Some(neighbour) = block_data_at(state, neighbour_pos, dimension) else {
        return false;
    };

    if is_fence(&neighbour.name) {
        return is_wooden_fence(&neighbour.name) == wooden;
    }

    if is_fence_gate(&neighbour.name) {
        // `FenceGateBlock.connectsToDirection(state, opposite(face))`:
        // the gate connects when its facing axis is perpendicular to the fence.
        let Some(facing) = prop(neighbour.properties.as_ref(), "facing").and_then(face_from_name)
        else {
            return false;
        };
        return axis_of(facing) == axis_of(clockwise(opposite(face)));
    }

    !is_exception_for_connection(&neighbour.name)
        && is_full_cube(&neighbour.name, neighbour.properties.as_ref())
}

/// `FenceBlock.getStateForPlacement`.
fn place_fence(state: &GlobalState, dimension: &str, pos: BlockPos, name: &str) -> BlockStateId {
    let wooden = is_wooden_fence(name);
    let north = fence_connects(state, dimension, pos, NORTH, wooden);
    let south = fence_connects(state, dimension, pos, SOUTH, wooden);
    let east = fence_connects(state, dimension, pos, EAST, wooden);
    let west = fence_connects(state, dimension, pos, WEST, wooden);

    let props = [
        ("north", if north { "true" } else { "false" }),
        ("south", if south { "true" } else { "false" }),
        ("east", if east { "true" } else { "false" }),
        ("west", if west { "true" } else { "false" }),
        ("waterlogged", "false"),
    ];
    BlockStateId::from_block_data(&make_block_data(name, &props))
}

/// `FenceGateBlock.getStateForPlacement`'s `in_wall` term: a wall on either side of the gate
/// (perpendicular to its facing axis) raises it.
fn fence_gate_in_wall(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    facing: i8,
) -> bool {
    let (first, second) = if axis_of(facing) == 2 {
        (WEST, EAST)
    } else {
        (NORTH, SOUTH)
    };
    block_data_at(state, pos + offset_of(first), dimension)
        .is_some_and(|block| is_wall(&block.name))
        || block_data_at(state, pos + offset_of(second), dimension)
            .is_some_and(|block| is_wall(&block.name))
}

fn fence_gate_state(state: &GlobalState, dimension: &str, pos: BlockPos, block: &BlockData) -> BlockStateId {
    let facing = prop(block.properties.as_ref(), "facing")
        .and_then(face_from_name)
        .unwrap_or(NORTH);
    let open = prop(block.properties.as_ref(), "open").unwrap_or("false");
    let powered = prop(block.properties.as_ref(), "powered").unwrap_or("false");
    let in_wall = fence_gate_in_wall(state, dimension, pos, facing);
    BlockStateId::from_block_data(&make_block_data(
        &block.name,
        &[
            ("facing", face_name(facing)),
            ("open", open),
            ("powered", powered),
            ("in_wall", if in_wall { "true" } else { "false" }),
        ],
    ))
}

/// `FenceGateBlock.getStateForPlacement`.
fn place_fence_gate(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    name: &str,
    yaw: Option<f32>,
) -> BlockStateId {
    let facing = yaw_direction(yaw.unwrap_or(0.0));
    let in_wall = fence_gate_in_wall(state, dimension, pos, facing);
    BlockStateId::from_block_data(&make_block_data(
        name,
        &[
            ("facing", face_name(facing)),
            ("open", "false"),
            ("powered", "false"),
            ("in_wall", if in_wall { "true" } else { "false" }),
        ],
    ))
}

/// `TrapDoorBlock.getStateForPlacement`. The fluid state is ignored, so the trapdoor is never
/// waterlogged here.
fn place_trapdoor(
    name: &str,
    face: i8,
    yaw: Option<f32>,
    cursor_y: f32,
) -> BlockStateId {
    let (facing, half) = if is_horizontal(face) {
        (
            face,
            if cursor_y > 0.5 { "top" } else { "bottom" },
        )
    } else {
        (
            opposite(yaw_direction(yaw.unwrap_or(0.0))),
            if face == UP { "bottom" } else { "top" },
        )
    };
    BlockStateId::from_block_data(&make_block_data(
        name,
        &[
            ("facing", face_name(facing)),
            ("half", half),
            ("open", "false"),
            ("powered", "false"),
            ("waterlogged", "false"),
        ],
    ))
}

/// `DoorBlock.getHinge`: which side of the door the hinge is on. The score prefers a hinge
/// against a neighbouring door or a solid block, and the click position breaks ties.
fn door_hinge(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    direction: i8,
    clicked: BlockPos,
    cursor: (f32, f32, f32),
) -> &'static str {
    let left = counter_clockwise(direction);
    let right = clockwise(direction);
    let above = pos + (0, 1, 0);

    let left_lower = block_data_at(state, pos + offset_of(left), dimension);
    let left_upper = block_data_at(state, above + offset_of(left), dimension);
    let right_lower = block_data_at(state, pos + offset_of(right), dimension);
    let right_upper = block_data_at(state, above + offset_of(right), dimension);

    let full = |block: &Option<BlockData>| {
        block
            .as_ref()
            .is_some_and(|block| is_full_cube(&block.name, block.properties.as_ref()))
    };

    let mut score = 0;
    if full(&left_lower) {
        score -= 1;
    }
    if full(&left_upper) {
        score -= 1;
    }
    if full(&right_lower) {
        score += 1;
    }
    if full(&right_upper) {
        score += 1;
    }

    let lower_door = |block: &Option<BlockData>| {
        block.as_ref().is_some_and(|block| {
            is_door(&block.name) && prop(block.properties.as_ref(), "half") == Some("lower")
        })
    };
    let left_door = lower_door(&left_lower);
    let right_door = lower_door(&right_lower);

    if (!left_door || right_door) && score <= 0 {
        if (!right_door || left_door) && score >= 0 {
            let dx = step_x(direction);
            let dz = step_z(direction);
            // `getClickLocation()` is absolute, and the door may be placed in a cell next to
            // the clicked one, so the cursor is offset by the clicked block.
            let click_x = clicked.pos.x as f32 + cursor.0 - pos.pos.x as f32;
            let click_z = clicked.pos.z as f32 + cursor.2 - pos.pos.z as f32;
            let hinge_left = (dx >= 0 || click_z >= 0.5)
                && (dx <= 0 || click_z <= 0.5)
                && (dz >= 0 || click_x <= 0.5)
                && (dz <= 0 || click_x >= 0.5);
            if hinge_left {
                "left"
            } else {
                "right"
            }
        } else {
            "left"
        }
    } else {
        "right"
    }
}

/// `DoorBlock.getStateForPlacement` plus `setPlacedBy`: a door is two blocks, the lower one at
/// the clicked position and the upper one right above it.
fn place_door(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    clicked: BlockPos,
    current: &BlockData,
    yaw: Option<f32>,
    cursor: (f32, f32, f32),
) -> Option<Vec<(BlockPos, BlockStateId)>> {
    let above = pos + (0, 1, 0);

    // `BlockPlaceContext.canBeReplaced` for the cell the upper half needs.
    if block_data_at(state, above, dimension)
        .is_some_and(|block| !can_be_replaced(&block.name))
    {
        return None;
    }

    // `DoorBlock.canSurvive` for the lower half.
    let below = block_data_at(state, pos + (0, -1, 0), dimension)?;
    if !has_sturdy_top(&below.name, below.properties.as_ref()) {
        return None;
    }

    let direction = yaw_direction(yaw.unwrap_or(0.0));
    let hinge = door_hinge(state, dimension, pos, direction, clicked, cursor);
    let make = |half: &str| {
        BlockStateId::from_block_data(&make_block_data(
            &current.name,
            &[
                ("facing", face_name(direction)),
                ("half", half),
                ("hinge", hinge),
                ("open", "false"),
                ("powered", "false"),
            ],
        ))
    };

    Some(vec![(pos, make("lower")), (above, make("upper"))])
}

struct TorchKind {
    floor: &'static str,
    wall: &'static str,
    keeps_lit: bool,
}

impl TorchKind {
    fn from_name(name: &str) -> Option<Self> {
        match short_name(name) {
            "torch" | "wall_torch" => Some(Self {
                floor: "minecraft:torch",
                wall: "minecraft:wall_torch",
                keeps_lit: false,
            }),
            "soul_torch" | "soul_wall_torch" => Some(Self {
                floor: "minecraft:soul_torch",
                wall: "minecraft:soul_wall_torch",
                keeps_lit: false,
            }),
            "redstone_torch" | "redstone_wall_torch" => Some(Self {
                floor: "minecraft:redstone_torch",
                wall: "minecraft:redstone_wall_torch",
                keeps_lit: true,
            }),
            _ => None,
        }
    }
}

fn torch_block(name: &str, facing: Option<i8>, lit: Option<&str>) -> BlockStateId {
    let mut props: Vec<(&str, &str)> = Vec::new();
    if let Some(facing) = facing {
        props.push(("facing", face_name(facing)));
    }
    if let Some(lit) = lit {
        props.push(("lit", lit));
    }
    BlockStateId::from_block_data(&make_block_data(name, &props))
}

/// `StandingAndWallBlockItem.getPlacementState` for torches (attachment direction is up).
fn place_torch(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    face: i8,
    rotation: Option<Rotation>,
    kind: TorchKind,
    current: &BlockData,
) -> Option<BlockStateId> {
    let lit = prop(current.properties.as_ref(), "lit").unwrap_or("true");
    let lit = kind.keeps_lit.then_some(lit);
    let directions = rotation
        .map(|rotation| ordered_looking_directions(rotation.yaw, rotation.pitch))
        .unwrap_or_else(|| face_order(face));

    // The wall state is resolved once, from the first horizontal direction the torch can hang
    // from, and then reused for every horizontal step of the outer loop. This mirrors
    // `StandingAndWallBlockItem`, which computes it before iterating the nearest directions.
    let wall_state = directions
        .iter()
        .copied()
        .filter(|direction| is_horizontal(*direction))
        .find_map(|direction| {
            let support_pos = pos + offset_of(direction);
            let support = block_data_at(state, support_pos, dimension)?;
            let facing = opposite(direction);
            if is_full_cube(&support.name, support.properties.as_ref()) {
                Some(torch_block(kind.wall, Some(facing), lit))
            } else {
                None
            }
        });

    for direction in directions {
        if direction == DOWN {
            continue;
        }
        if direction == UP {
            let support = block_data_at(state, pos + (0, -1, 0), dimension)?;
            if supports_torch_center(&support.name, support.properties.as_ref()) {
                return Some(torch_block(kind.floor, None, lit));
            }
        } else if let Some(wall_state) = wall_state {
            return Some(wall_state);
        }
    }

    None
}

/// Returns a copy of `block` with one property replaced, like `BlockState.setValue`.
///
/// Returns `None` when the block has no such property, so a block without `facing` is left
/// untouched instead of being turned into an unknown (air) state.
fn with_property(block: &BlockData, key: &str, value: &str) -> Option<BlockStateId> {
    let mut props = block.properties.clone()?;
    if !props.contains_key(key) {
        return None;
    }
    props.insert(key.to_string(), value.to_string());
    Some(BlockStateId::from_block_data(&BlockData {
        name: block.name.clone(),
        properties: Some(props),
    }))
}

/// Blocks whose `getStateForPlacement` sets `facing` to
/// `context.getHorizontalDirection().getOpposite()` (chests, furnaces, pumpkins, beehives...).
fn faces_horizontal_opposite(short: &str) -> bool {
    matches!(
        short,
        "ender_chest"
            | "trapped_chest"
            | "furnace"
            | "blast_furnace"
            | "smoker"
            | "carved_pumpkin"
            | "jack_o_lantern"
            | "bee_nest"
            | "beehive"
            | "chiseled_bookshelf"
            | "loom"
            | "stonecutter"
            | "end_portal_frame"
            | "vault"
            | "repeater"
            | "comparator"
    ) || short.ends_with("_glazed_terracotta")
}

/// Blocks whose `getStateForPlacement` sets `facing` to
/// `context.getNearestLookingDirection().getOpposite()` (barrel, dispenser, dropper, pistons...).
fn faces_nearest_looking_opposite(short: &str) -> bool {
    matches!(
        short,
        "barrel"
            | "dispenser"
            | "dropper"
            | "piston"
            | "sticky_piston"
            | "command_block"
            | "chain_command_block"
            | "repeating_command_block"
    )
}

/// The `facing` value a directional block gets when placed, following the block's
/// `getStateForPlacement` (1.21.8). Returns `None` when the block has no player-facing rule.
fn placement_facing(name: &str, rotation: Option<Rotation>, face: i8) -> Option<i8> {
    let short = short_name(name);
    let yaw = rotation.map(|rotation| rotation.yaw).unwrap_or(0.0);
    let horizontal = yaw_direction(yaw);

    if faces_horizontal_opposite(short) {
        // `context.getHorizontalDirection().getOpposite()`
        return Some(opposite(horizontal));
    }

    if faces_nearest_looking_opposite(short) {
        // `context.getNearestLookingDirection().getOpposite()`
        let pitch = rotation.map(|rotation| rotation.pitch).unwrap_or(0.0);
        return Some(opposite(ordered_looking_directions(yaw, pitch)[0]));
    }

    // `ObserverBlock` faces the player: `getNearestLookingDirection().getOpposite().getOpposite()`.
    if short == "observer" {
        let pitch = rotation.map(|rotation| rotation.pitch).unwrap_or(0.0);
        return Some(ordered_looking_directions(yaw, pitch)[0]);
    }

    // `HopperBlock`: the clicked face's opposite, but always down on the y axis.
    if short == "hopper" {
        let direction = opposite(face);
        return Some(if direction == UP || direction == DOWN {
            DOWN
        } else {
            direction
        });
    }

    // `ShulkerBoxBlock`, `EndRodBlock` and `LightningRodBlock` face the clicked face.
    if short == "shulker_box"
        || short.ends_with("_shulker_box")
        || short == "end_rod"
        || short == "lightning_rod"
    {
        return Some(face);
    }

    // `AnvilBlock`: `getHorizontalDirection().getClockWise()`.
    if matches!(short, "anvil" | "chipped_anvil" | "damaged_anvil") {
        return Some(clockwise(horizontal));
    }

    None
}

/// Where a placement goes and how the player clicked: the target cell, the clicked block, the
/// clicked face (same ids as `use_item_on`) and the hit position inside the clicked block.
#[derive(Debug, Clone, Copy)]
pub struct PlaceContext {
    /// The cell the block is written to (clicked position + face offset).
    pub pos: BlockPos,
    /// The block that was right-clicked.
    pub clicked: BlockPos,
    pub face: i8,
    /// Hit position inside the clicked block, each component in `0..1`.
    pub cursor: (f32, f32, f32),
}

/// Computes the blocks a placement writes: the placed block itself, plus the extra blocks some
/// blocks need (the upper half of a door).
///
/// Returns `None` when vanilla would refuse the placement (a torch with nothing to attach to, a
/// door without room above it, a redstone wire without a supporting block, ...).
pub fn compute_placement(
    state: &GlobalState,
    dimension: &str,
    context: PlaceContext,
    rotation: Option<Rotation>,
    default_state: BlockStateId,
) -> Option<Vec<(BlockPos, BlockStateId)>> {
    let pos = context.pos;
    let Some(current) = default_state.to_block_data() else {
        return Some(vec![(pos, default_state)]);
    };
    let yaw = rotation.map(|rotation| rotation.yaw);

    if let Some(kind) = TorchKind::from_name(&current.name) {
        return place_torch(
            state,
            dimension,
            pos,
            context.face,
            rotation,
            kind,
            &current,
        )
        .map(|placed| vec![(pos, placed)]);
    }

    if is_fence(&current.name) {
        return Some(vec![(
            pos,
            place_fence(state, dimension, pos, &current.name),
        )]);
    }

    if is_door(&current.name) {
        return place_door(
            state,
            dimension,
            pos,
            context.clicked,
            &current,
            yaw,
            context.cursor,
        );
    }

    if is_trapdoor(&current.name) {
        return Some(vec![(
            pos,
            place_trapdoor(&current.name, context.face, yaw, context.cursor.1),
        )]);
    }

    if is_fence_gate(&current.name) {
        return Some(vec![(
            pos,
            place_fence_gate(state, dimension, pos, &current.name, yaw),
        )]);
    }

    if is_redstone_wire(&current.name) {
        return place_wire(state, dimension, pos, &current, false)
            .map(|placed| vec![(pos, placed)]);
    }

    // Directional blocks (ender chest, furnace, barrel, ...) face away from the player, so the
    // default `facing=north` state has to be overridden with the player's orientation.
    if let Some(facing) = placement_facing(&current.name, rotation, context.face) {
        if let Some(faced) = with_property(&current, "facing", face_name(facing)) {
            return Some(vec![(pos, faced)]);
        }
    }

    Some(vec![(pos, default_state)])
}

/// Toggles the state of the block a `use_item_on` right click targeted, the `useWithoutItem`
/// half of vanilla's block interaction.
///
/// Returns the states that changed (already written to the world), or `None` when the block has
/// no hand interaction, in which case the click falls through to item use and placement.
pub fn interact(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    yaw: Option<f32>,
) -> Option<Vec<(BlockPos, BlockStateId)>> {
    let block = block_data_at(state, pos, dimension)?;

    if is_door(&block.name) {
        if !can_open_by_hand(&block.name) {
            return None;
        }
        return toggle_door(state, dimension, pos, &block);
    }

    if is_fence_gate(&block.name) {
        return Some(vec![toggle_fence_gate(
            state, dimension, pos, &block, yaw,
        )]);
    }

    if is_trapdoor(&block.name) {
        return Some(vec![toggle_trapdoor(state, dimension, pos, &block)]);
    }

    None
}

/// `DoorBlock.useWithoutItem`: both halves of the door flip together.
fn toggle_door(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
) -> Option<Vec<(BlockPos, BlockStateId)>> {
    let props = block.properties.as_ref();
    let open = prop(props, "open") == Some("true");
    let new_open = if open { "false" } else { "true" };
    let facing = prop(props, "facing").unwrap_or("north");
    let hinge = prop(props, "hinge").unwrap_or("left");
    let powered = prop(props, "powered").unwrap_or("false");

    let lower = if prop(props, "half") == Some("upper") {
        pos + (0, -1, 0)
    } else {
        pos
    };

    let mut changed = Vec::new();
    for (target, half) in [(lower, "lower"), (lower + (0, 1, 0), "upper")] {
        let Some(other) = block_data_at(state, target, dimension) else {
            continue;
        };
        if other.name != block.name {
            continue;
        }
        let new_state = BlockStateId::from_block_data(&make_block_data(
            &block.name,
            &[
                ("facing", facing),
                ("half", half),
                ("hinge", hinge),
                ("open", new_open),
                ("powered", powered),
            ],
        ));
        if state
            .world
            .get_block_and_fetch(target, dimension)
            .is_ok_and(|current| current == new_state)
        {
            continue;
        }
        if write_block(state, dimension, target, new_state) {
            changed.push((target, new_state));
        }
    }

    Some(changed)
}

/// `FenceGateBlock.useWithoutItem`: close it, or open it and turn it to face the player.
fn toggle_fence_gate(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
    yaw: Option<f32>,
) -> (BlockPos, BlockStateId) {
    let props = block.properties.as_ref();
    let open = prop(props, "open") == Some("true");
    let mut facing = prop(props, "facing")
        .and_then(face_from_name)
        .unwrap_or(NORTH);
    let powered = prop(props, "powered").unwrap_or("false");

    if !open {
        let direction = yaw_direction(yaw.unwrap_or(0.0));
        if facing == opposite(direction) {
            facing = direction;
        }
    }

    let in_wall = fence_gate_in_wall(state, dimension, pos, facing);
    let new_state = BlockStateId::from_block_data(&make_block_data(
        &block.name,
        &[
            ("facing", face_name(facing)),
            ("open", if open { "false" } else { "true" }),
            ("powered", powered),
            ("in_wall", if in_wall { "true" } else { "false" }),
        ],
    ));
    let _ = write_block(state, dimension, pos, new_state);
    (pos, new_state)
}

/// `TrapDoorBlock.toggle`.
fn toggle_trapdoor(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
) -> (BlockPos, BlockStateId) {
    let props = block.properties.as_ref();
    let open = prop(props, "open") == Some("true");
    let new_state = BlockStateId::from_block_data(&make_block_data(
        &block.name,
        &[
            (
                "facing",
                prop(props, "facing").unwrap_or("north"),
            ),
            ("half", prop(props, "half").unwrap_or("bottom")),
            ("open", if open { "false" } else { "true" }),
            ("powered", prop(props, "powered").unwrap_or("false")),
            (
                "waterlogged",
                prop(props, "waterlogged").unwrap_or("false"),
            ),
        ],
    ));
    let _ = write_block(state, dimension, pos, new_state);
    (pos, new_state)
}

/// Recomputes the connection state of every fence and redstone wire in the 3x3x3 block cube
/// around `center`, writing the changed states back and returning them.
///
/// The cube covers the direct neighbours, the wires one level above/below (their diagonal
/// connections), and the neighbour-of-neighbour positions that `updateIndirectNeighbourShapes`
/// would touch.
pub fn refresh_around(
    state: &GlobalState,
    dimension: &str,
    center: BlockPos,
    commands: &mut Commands,
    conn_query: &Query<ConnQuery>,
) -> Vec<(BlockPos, BlockStateId)> {
    let mut changed = Vec::new();
    // Positions popped to air, so the block right above them can be re-checked (vanilla
    // cascades `canSurvive` failures upwards, e.g. a whole cactus column).
    let mut popped: Vec<BlockPos> = Vec::new();

    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                let pos = BlockPos::of(center.pos.x + dx, center.pos.y + dy, center.pos.z + dz);
                let Some(block) = block_data_at(state, pos, dimension) else {
                    continue;
                };
                let Ok(current) = state.world.get_block_and_fetch(pos, dimension) else {
                    continue;
                };

                let cleared = clear_if_unsupported(
                    state,
                    dimension,
                    pos,
                    &block,
                    &mut changed,
                    commands,
                    conn_query,
                );
                if !cleared.is_empty() {
                    popped.extend(cleared);
                    continue;
                }

                let new_state = if is_fence(&block.name) {
                    place_fence(state, dimension, pos, &block.name)
                } else if is_fence_gate(&block.name) {
                    fence_gate_state(state, dimension, pos, &block)
                } else if is_redstone_wire(&block.name) {
                    let was_dot = HORIZONTAL.iter().all(|face| {
                        prop(block.properties.as_ref(), face_name(*face)) == Some("none")
                    });
                    match place_wire(state, dimension, pos, &block, was_dot) {
                        Some(new_state) => new_state,
                        // `RedStoneWireBlock.canSurvive` failed: no support underneath, the
                        // wire pops and drops itself, like vanilla `destroyBlock(pos, true)`.
                        None => {
                            let air = BlockStateId::default();
                            if current != air && write_block(state, dimension, pos, air) {
                                changed.push((pos, air));
                                drop_pop_loot(current, pos, dimension, commands, conn_query);
                                popped.push(pos);
                            }
                            continue;
                        }
                    }
                } else {
                    continue;
                };

                if new_state == current {
                    continue;
                }
                if write_block(state, dimension, pos, new_state) {
                    changed.push((pos, new_state));
                }
            }
        }
    }

    // Upward cascade: a block standing on a popped block may lose its own support.
    let mut i = 0;
    while i < popped.len() {
        let above = popped[i] + (0, 1, 0);
        i += 1;
        if state
            .world
            .get_block_and_fetch(above, dimension)
            .is_ok_and(|current| current == BlockStateId::default())
        {
            continue;
        }
        let Some(block) = block_data_at(state, above, dimension) else {
            continue;
        };
        let cleared = clear_if_unsupported(
            state,
            dimension,
            above,
            &block,
            &mut changed,
            commands,
            conn_query,
        );
        popped.extend(cleared);
    }

    changed
}

/// Vanilla `Block.updateOrDestroy` -> `Level.destroyBlock(pos, true)`: a block that pops drops
/// its loot with no tool and the default pickup delay.
fn drop_pop_loot(
    popped: BlockStateId,
    pos: BlockPos,
    dimension: &str,
    commands: &mut Commands,
    conn_query: &Query<ConnQuery>,
) {
    let dimension = Dimension::from_name(dimension).unwrap_or_default();
    crate::systems::item_entities::drop_pop_loot(popped, pos, dimension, commands, conn_query);
}

/// Turns `pos` (and the partner of a two-block structure) to air when the block there is
/// orphaned or has lost its support, dropping each cleared block's loot. Returns the positions
/// that were cleared.
fn clear_if_unsupported(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
    changed: &mut Vec<(BlockPos, BlockStateId)>,
    commands: &mut Commands,
    conn_query: &Query<ConnQuery>,
) -> Vec<BlockPos> {
    // Doors, double plants and beds are two-block structures: `DoorBlock.updateShape`,
    // `DoublePlantBlock.updateShape` and `BedBlock.updateShape` turn a half into air when its
    // partner (or, for the lower half, its support) is gone. The partner is cleared in the
    // same pass, even when it is outside the scanned cube.
    let orphaned = if is_door(&block.name) {
        door_is_orphaned(state, dimension, pos, block)
    } else if is_double_plant(block) {
        double_plant_is_orphaned(state, dimension, pos, block)
    } else if is_bed(block) {
        bed_is_orphaned(state, dimension, pos, block)
    } else {
        false
    };

    let mut targets = Vec::new();
    if orphaned {
        let partner = if prop(block.properties.as_ref(), "half") == Some("upper")
            || prop(block.properties.as_ref(), "part") == Some("head")
        {
            pos + (0, -1, 0)
        } else {
            pos + (0, 1, 0)
        };
        targets.push(pos);
        if block_data_at(state, partner, dimension)
            .is_some_and(|other| other.name == block.name)
        {
            targets.push(partner);
        }
    } else if support_lost(state, dimension, pos, block) {
        targets.push(pos);
    }

    let mut cleared = Vec::new();
    for target in targets {
        let air = BlockStateId::default();
        let old = state.world.get_block_and_fetch(target, dimension);
        if old.as_ref().is_ok_and(|current| *current == air) {
            continue;
        }
        if write_block(state, dimension, target, air) {
            changed.push((target, air));
            if let Ok(old) = old {
                drop_pop_loot(old, target, dimension, commands, conn_query);
            }
            cleared.push(target);
        }
    }
    cleared
}

/// Vanilla `Block.updateOrDestroy`: after a neighbour changes, a block whose `canSurvive`
/// fails is destroyed. True when `block` at `pos` has lost the block it stands on or is
/// attached to. Blocks without a support dependency return false.
fn support_lost(state: &GlobalState, dimension: &str, pos: BlockPos, block: &BlockData) -> bool {
    use ferrumc_data::generated::tags::TagData;

    let short = short_name(&block.name);
    let props = block.properties.as_ref();
    let below = block_data_at(state, pos + (0, -1, 0), dimension);
    let above = block_data_at(state, pos + (0, 1, 0), dimension);

    // `None` means the neighbour could not be read: assume the support is still there.
    let below_sturdy = || below.as_ref().is_none_or(|b| has_sturdy_top(&b.name, b.properties.as_ref()));
    let below_torch = || {
        below.as_ref().is_none_or(|b| supports_torch_center(&b.name, b.properties.as_ref()))
    };
    let below_redstone = || {
        below.as_ref().is_none_or(|b| supports_redstone_on(&b.name, b.properties.as_ref()))
    };
    let below_solid = || {
        below.as_ref().is_none_or(|b| is_solid_block(&b.name, b.properties.as_ref()))
    };
    let above_sturdy = || above.as_ref().is_none_or(|b| has_sturdy_bottom(&b.name, b.properties.as_ref()));

    // Vegetation: `BushBlock.canSurvive` -> `mayPlaceOn(below)`.
    if let Some(lost) = plant_support_lost(block, below.as_ref()) {
        return lost;
    }

    match short {
        "redstone_wire" => !below_redstone(),
        // `DiodeBlock.canSurviveOn`: sturdy top face below.
        "repeater" | "comparator" => !below_sturdy(),
        // `BasePressurePlateBlock.canSurvive`: rigid or center support below.
        s if s.ends_with("_pressure_plate") => !below_sturdy(),
        // `BaseRailBlock.canSurvive`: `canSupportRigidBlock` below.
        s if s == "rail" || s.ends_with("_rail") => !below_sturdy(),
        // `CarpetBlock.canSurvive`: the block below is not empty.
        s if s.ends_with("_carpet") => below.as_ref().is_some_and(|b| is_air(&b.name) || is_fluid(&b.name)),
        // `SnowLayerBlock.canSurvive`.
        "snow" => match below.as_ref() {
            None => false,
            Some(b) => {
                in_tag(&TagData::BLOCK_TAGS_MINECRAFT_SNOW_LAYER_CANNOT_SURVIVE_ON, &b.name)
                    || !(in_tag(&TagData::BLOCK_TAGS_MINECRAFT_SNOW_LAYER_CAN_SURVIVE_ON, &b.name)
                        || has_sturdy_top(&b.name, b.properties.as_ref())
                        || (b.name == "minecraft:snow"
                            && prop(b.properties.as_ref(), "layers") == Some("8")))
            }
        },
        // Standing torches: `BaseTorchBlock.canSurvive` = `canSupportCenter(below, UP)`.
        s if s.ends_with("wall_torch") => !side_attached(state, dimension, pos, props),
        s if s.ends_with("torch") => !below_torch(),
        // `LeverBlock` / `ButtonBlock.canSurvive`: depends on the `face` property.
        s if s == "lever" || s.ends_with("_button") => match prop(props, "face") {
            Some("ceiling") => !above_sturdy(),
            Some("wall") => !side_attached(state, dimension, pos, props),
            _ => !below_sturdy(),
        },
        // `BellBlock.canSurvive`: depends on the `attachment` property.
        "bell" => match prop(props, "attachment") {
            Some("ceiling") => !above_sturdy(),
            Some("floor") => !below_sturdy(),
            _ => !side_attached(state, dimension, pos, props),
        },
        // `LanternBlock` / `ChainBlock.canSurvive`: hanging from above, else standing below.
        s if s == "lantern" || s == "soul_lantern" || s == "chain" => {
            if prop(props, "hanging") == Some("true") {
                !above_sturdy()
            } else {
                !below_torch()
            }
        }
        // `StandingSignBlock` / `BannerBlock.canSurvive`: solid block below; wall variants
        // attach to the side; hanging signs hang from above.
        s if s.ends_with("_sign") => {
            if short.contains("wall") {
                !side_attached(state, dimension, pos, props)
            } else if short.contains("hanging") {
                !above_sturdy()
            } else {
                !below_solid()
            }
        }
        s if s.ends_with("_banner") => {
            if short.contains("wall") {
                !side_attached(state, dimension, pos, props)
            } else {
                !below_solid()
            }
        }
        "flower_pot" => !below_solid(),
        s if s.starts_with("potted_") => !below_solid(),
        "turtle_egg" => !below_solid(),
        // `FlowerPotBlock`-style candles need a sturdy block below.
        s if s == "candle" || s == "candle_cake" || s.ends_with("_candle") => !below_sturdy(),
        "ladder" => !side_attached(state, dimension, pos, props),
        "tripwire_hook" => !side_attached(state, dimension, pos, props),
        // `CocoaBlock.canSurvive`: a jungle log at the facing direction.
        "cocoa" => {
            let Some(face) = prop(props, "facing").and_then(face_from_name) else {
                return false;
            };
            let offset = face_offset(face);
            !block_data_at(state, pos + offset, dimension)
                .is_none_or(|b| in_tag(&TagData::BLOCK_TAGS_MINECRAFT_JUNGLE_LOGS, &b.name))
        }
        // `AmethystClusterBlock.canSurvive`: sturdy face opposite the facing.
        s if s == "amethyst_cluster" || s.ends_with("_bud") => {
            !side_attached(state, dimension, pos, props)
        }
        // Heads and skulls: wall variants attach to the side, standing ones need solid below.
        s if s.ends_with("_head") || s.ends_with("_skull") => {
            if prop(props, "facing").is_some() {
                !side_attached(state, dimension, pos, props)
            } else {
                !below_solid()
            }
        }
        // Blocks that hang from the block above.
        s if s == "hanging_roots"
            || s == "spore_blossom"
            || s.ends_with("_vines")
            || s.ends_with("_vines_plant") =>
        {
            !above_sturdy()
        }
        "vine" => {
            let any_side = [NORTH, SOUTH, WEST, EAST].iter().any(|face| {
                let offset = face_offset(*face);
                block_data_at(state, pos + offset, dimension)
                    .is_some_and(|b| is_attachable(&b.name, b.properties.as_ref()))
            });
            !any_side && !above_sturdy()
        }
        // Multi-face blocks pop only when no adjacent face is solid at all.
        s if s == "glow_lichen" || s == "sculk_vein" => {
            ![UP, DOWN, NORTH, SOUTH, WEST, EAST].iter().any(|face| {
                let offset = face_offset(*face);
                block_data_at(state, pos + offset, dimension)
                    .is_some_and(|b| is_solid_block(&b.name, b.properties.as_ref()))
            })
        }
        "sea_pickle" => !below_solid(),
        // `FireBlock.canSurvive`: sturdy top below (or a valid fire base, approximated by
        // the soul-fire base tag for soul fire).
        "fire" | "soul_fire" => {
            !below_sturdy()
                && !(short == "soul_fire"
                    && below
                        .as_ref()
                        .is_some_and(|b| in_tag(&TagData::BLOCK_TAGS_MINECRAFT_SOUL_FIRE_BASE_BLOCKS, &b.name)))
        }
        _ => false,
    }
}

/// `BushBlock.canSurvive` -> `mayPlaceOn(below)` and the per-class overrides: true when the
/// plant at `block` has LOST the block under it. `None` when `block` is not a support-dependent
/// plant, or when the block below could not be read.
fn plant_support_lost(block: &BlockData, below: Option<&BlockData>) -> Option<bool> {
    use ferrumc_data::generated::tags::TagData;

    let short = short_name(&block.name);
    let below = below?;
    let dirt = |b: &BlockData| {
        in_tag(&TagData::BLOCK_TAGS_MINECRAFT_DIRT, &b.name) || b.name == "minecraft:farmland"
    };
    let sand = |b: &BlockData| in_tag(&TagData::BLOCK_TAGS_MINECRAFT_SAND, &b.name);

    let supported = match short {
        // `CropBlock.mayPlaceOn`: farmland only.
        "wheat" | "carrots" | "potatoes" | "beetroots" | "melon_stem" | "pumpkin_stem"
        | "attached_melon_stem" | "attached_pumpkin_stem" | "pitcher_crop" | "torchflower_crop" => {
            below.name == "minecraft:farmland"
        }
        "nether_wart" => below.name == "minecraft:soul_sand",
        "bamboo" | "bamboo_sapling" => {
            in_tag(&TagData::BLOCK_TAGS_MINECRAFT_BAMBOO_PLANTABLE_ON, &below.name)
                || below.name == "minecraft:bamboo"
                || dirt(below)
        }
        "nether_sprouts" | "crimson_roots" | "warped_roots" => {
            in_tag(&TagData::BLOCK_TAGS_MINECRAFT_NYLIUM, &below.name)
                || below.name == "minecraft:soul_soil"
                || dirt(below)
        }
        "crimson_fungus" | "warped_fungus" => {
            in_tag(&TagData::BLOCK_TAGS_MINECRAFT_NYLIUM, &below.name)
                || below.name == "minecraft:mycelium"
                || below.name == "minecraft:soul_soil"
                || dirt(below)
        }
        // `MushroomBlock`: the grow-block tag, or any solid-render block in the dark.
        "brown_mushroom" | "red_mushroom" => {
            in_tag(&TagData::BLOCK_TAGS_MINECRAFT_MUSHROOM_GROW_BLOCK, &below.name)
                || is_full_cube(&below.name, below.properties.as_ref())
        }
        "cactus" | "cactus_flower" => below.name == "minecraft:cactus" || sand(below),
        "sugar_cane" => below.name == "minecraft:sugar_cane" || dirt(below) || sand(below),
        "chorus_plant" | "chorus_flower" => {
            below.name == "minecraft:chorus_plant" || below.name == "minecraft:end_stone"
        }
        "kelp" | "kelp_plant" => below.name == block.name || dirt(below),
        // `SeagrassBlock.mayPlaceOn`: sturdy top face that is not magma.
        "seagrass" => {
            has_sturdy_top(&below.name, below.properties.as_ref())
                && below.name != "minecraft:magma_block"
        }
        "azalea" | "flowering_azalea" => below.name == "minecraft:clay" || dirt(below),
        "leaf_litter" => has_sturdy_top(&below.name, below.properties.as_ref()),
        s if is_dirt_plant(s) => dirt(below),
        _ => return None,
    };
    Some(!supported)
}

/// The default `VegetationBlock.mayPlaceOn` plants: the dirt tag or farmland below.
fn is_dirt_plant(short: &str) -> bool {
    matches!(
        short,
        "grass"
            | "short_grass"
            | "fern"
            | "dead_bush"
            | "bush"
            | "poppy"
            | "dandelion"
            | "blue_orchid"
            | "allium"
            | "azure_bluet"
            | "red_tulip"
            | "orange_tulip"
            | "white_tulip"
            | "pink_tulip"
            | "oxeye_daisy"
            | "cornflower"
            | "lily_of_the_valley"
            | "wither_rose"
            | "torchflower"
            | "pink_petals"
            | "sweet_berry_bush"
            | "firefly_bush"
            | "big_dripleaf"
            | "big_dripleaf_stem"
            | "tall_dry_grass"
            | "dry_grass"
    ) || (short.ends_with("_flower")
        || short.ends_with("_fern")
        || short.ends_with("_grass")
        || short.ends_with("_sapling")
        || short.ends_with("_bush")
        || short.ends_with("_petals")
        || short.ends_with("_sprouts")
        || (short.ends_with("_roots") && short != "hanging_roots"))
}

/// A `BedBlock` state: two halves joined by the `part` property (`head`/`foot`).
fn is_bed(block: &BlockData) -> bool {
    short_name(&block.name).ends_with("_bed")
}

/// `BedBlock.updateShape` / `canSurvive`: true when this half of a bed should turn into air
/// because its partner is gone or (for the foot) it lost the sturdy block under it.
fn bed_is_orphaned(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
) -> bool {
    let props = block.properties.as_ref();
    if prop(props, "part") == Some("head") {
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        below.name != block.name || prop(below.properties.as_ref(), "part") != Some("foot")
    } else {
        let Some(above) = block_data_at(state, pos + (0, 1, 0), dimension) else {
            return false;
        };
        if above.name != block.name || prop(above.properties.as_ref(), "part") != Some("head") {
            return true;
        }
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        !has_sturdy_top(&below.name, below.properties.as_ref())
    }
}

/// A `DoublePlantBlock` state: any non-door block whose `half` property is `lower`/`upper`
/// (sunflower, lilac, peony, rose_bush, tall_grass, large_fern, tall_seagrass, pitcher_plant,
/// pitcher_crop, small_dripleaf). Stairs and trapdoors use `top`/`bottom`, so they do not match.
fn is_double_plant(block: &BlockData) -> bool {
    !is_door(&block.name)
        && matches!(
            prop(block.properties.as_ref(), "half"),
            Some("lower") | Some("upper")
        )
}

/// `DoublePlantBlock.updateShape` / `canSurvive`: true when this half of a double plant should
/// turn into air because its partner half is gone, or (for the lower half) it lost the
/// plantable block under it (`VegetationBlock.mayPlaceOn`: the dirt tag or farmland).
fn double_plant_is_orphaned(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
) -> bool {
    use ferrumc_data::generated::tags::TagData;

    let props = block.properties.as_ref();
    if prop(props, "half") == Some("upper") {
        // The upper half survives only on the lower half of the same plant below it.
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        below.name != block.name || prop(below.properties.as_ref(), "half") != Some("lower")
    } else {
        // The lower half needs the upper half above it...
        let Some(above) = block_data_at(state, pos + (0, 1, 0), dimension) else {
            return false;
        };
        if above.name != block.name || prop(above.properties.as_ref(), "half") != Some("upper") {
            return true;
        }
        // ...and a plantable block below it.
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        !(TagData::BLOCK_TAGS_MINECRAFT_DIRT
            .values
            .contains(&below.name.as_str())
            || below.name == "minecraft:farmland")
    }
}

/// `DoorBlock.updateShape` / `DoorBlock.canSurvive`: true when this half of a door should turn
/// into air because its partner is gone or (for the lower half) it lost its support.
fn door_is_orphaned(
    state: &GlobalState,
    dimension: &str,
    pos: BlockPos,
    block: &BlockData,
) -> bool {
    let props = block.properties.as_ref();
    let half = prop(props, "half").unwrap_or("lower");

    if half == "lower" {
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        if !has_sturdy_top(&below.name, below.properties.as_ref()) {
            return true;
        }
        let Some(above) = block_data_at(state, pos + (0, 1, 0), dimension) else {
            return false;
        };
        above.name != block.name || prop(above.properties.as_ref(), "half") != Some("upper")
    } else {
        let Some(below) = block_data_at(state, pos + (0, -1, 0), dimension) else {
            return false;
        };
        below.name != block.name || prop(below.properties.as_ref(), "half") != Some("lower")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_south_starts_with_south() {
        assert_eq!(
            ordered_looking_directions(0.0, 0.0)[0],
            SOUTH,
            "yaw 0 looks towards +Z (south)"
        );
    }

    #[test]
    fn looking_east_starts_with_east() {
        assert_eq!(ordered_looking_directions(-90.0, 0.0)[0], EAST);
    }

    #[test]
    fn looking_straight_down_starts_with_down() {
        assert_eq!(ordered_looking_directions(0.0, 90.0)[0], DOWN);
    }

    #[test]
    fn looking_straight_up_starts_with_up() {
        assert_eq!(ordered_looking_directions(0.0, -90.0)[0], UP);
    }

    #[test]
    fn fence_connects_only_within_the_same_fence_family() {
        assert!(is_wooden_fence("minecraft:oak_fence"));
        assert!(!is_wooden_fence("minecraft:nether_brick_fence"));
        assert!(is_fence("minecraft:nether_brick_fence"));
    }

    #[test]
    fn full_cube_excludes_partial_blocks() {
        assert!(is_full_cube("minecraft:stone", None));
        assert!(is_full_cube("minecraft:glass", None));
        assert!(!is_full_cube("minecraft:oak_fence", None));
        assert!(!is_full_cube("minecraft:oak_stairs", None));
        assert!(!is_full_cube("minecraft:oak_door", None));
        assert!(!is_full_cube("minecraft:torch", None));
    }

    #[test]
    fn yaw_converts_to_facing_like_direction_from_y_rot() {
        assert_eq!(yaw_direction(0.0), SOUTH);
        assert_eq!(yaw_direction(90.0), WEST);
        assert_eq!(yaw_direction(180.0), NORTH);
        assert_eq!(yaw_direction(-90.0), EAST);
        assert_eq!(yaw_direction(360.0), SOUTH);
    }

    #[test]
    fn double_plants_are_half_lower_upper_blocks_that_are_not_doors() {
        assert!(is_double_plant(&make_block_data(
            "minecraft:sunflower",
            &[("half", "upper")]
        )));
        assert!(is_double_plant(&make_block_data(
            "minecraft:tall_grass",
            &[("half", "lower")]
        )));
        assert!(is_double_plant(&make_block_data(
            "minecraft:large_fern",
            &[("half", "upper")]
        )));
        // Doors are handled by the door branch, not the double-plant branch.
        assert!(!is_double_plant(&make_block_data(
            "minecraft:oak_door",
            &[("half", "lower")]
        )));
        // Stairs/trapdoors use top/bottom, not lower/upper.
        assert!(!is_double_plant(&make_block_data(
            "minecraft:oak_stairs",
            &[("half", "top")]
        )));
        assert!(!is_double_plant(&make_block_data("minecraft:stone", &[])));
        assert!(!is_double_plant(&make_block_data("minecraft:poppy", &[])));
    }

    #[test]
    fn plants_pop_when_the_block_under_them_stops_qualifying() {
        let below = |name: &str| make_block_data(name, &[]);
        // Default vegetation: dirt tag or farmland.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:poppy", &[]), Some(&below("minecraft:grass_block"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:poppy", &[]), Some(&below("minecraft:stone"))),
            Some(true)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:short_grass", &[]), Some(&below("minecraft:farmland"))),
            Some(false)
        );
        // Crops need farmland.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:wheat", &[("age", "0")]), Some(&below("minecraft:farmland"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:wheat", &[("age", "0")]), Some(&below("minecraft:dirt"))),
            Some(true)
        );
        // Nether wart needs soul sand; nether sprouts accept nylium/soul soil.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:nether_wart", &[]), Some(&below("minecraft:soul_sand"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:nether_wart", &[]), Some(&below("minecraft:dirt"))),
            Some(true)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:crimson_roots", &[]), Some(&below("minecraft:crimson_nylium"))),
            Some(false)
        );
        // Cactus stacks on cactus or sand; sugar cane also accepts dirt.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:cactus", &[]), Some(&below("minecraft:cactus"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:cactus", &[]), Some(&below("minecraft:sand"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:cactus", &[]), Some(&below("minecraft:stone"))),
            Some(true)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:sugar_cane", &[]), Some(&below("minecraft:dirt"))),
            Some(false)
        );
        // Chorus on end stone or chorus plant; kelp stacks; azalea also accepts clay.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:chorus_flower", &[]), Some(&below("minecraft:end_stone"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:chorus_flower", &[]), Some(&below("minecraft:dirt"))),
            Some(true)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:kelp", &[]), Some(&below("minecraft:kelp"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:azalea", &[]), Some(&below("minecraft:clay"))),
            Some(false)
        );
        // Mushrooms: grow-block tag or any full cube.
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:red_mushroom", &[]), Some(&below("minecraft:mycelium"))),
            Some(false)
        );
        assert_eq!(
            plant_support_lost(&make_block_data("minecraft:red_mushroom", &[]), Some(&below("minecraft:stone"))),
            Some(false)
        );
        // Non-plants and unreadable surroundings are not support-dependent.
        assert_eq!(plant_support_lost(&make_block_data("minecraft:stone", &[]), Some(&below("minecraft:air"))), None);
        assert_eq!(plant_support_lost(&make_block_data("minecraft:poppy", &[]), None), None);
    }

    #[test]
    fn sturdy_bottom_face_for_hanging_blocks() {
        assert!(has_sturdy_bottom("minecraft:stone", None));
        assert!(has_sturdy_bottom("minecraft:oak_slab", None));
        assert!(has_sturdy_bottom("minecraft:oak_stairs", Some(&BTreeMap::from([("half".to_string(), "bottom".to_string())]))));
        assert!(!has_sturdy_bottom("minecraft:oak_stairs", Some(&BTreeMap::from([("half".to_string(), "top".to_string())]))));
        assert!(has_sturdy_bottom("minecraft:snow", Some(&BTreeMap::from([("layers".to_string(), "3".to_string())]))));
        assert!(!has_sturdy_bottom("minecraft:air", None));
        assert!(!has_sturdy_bottom("minecraft:oak_fence", None));
    }

    #[test]
    fn trapdoors_are_not_doors() {
        assert!(is_door("minecraft:oak_door"));
        assert!(!is_door("minecraft:oak_trapdoor"));
        assert!(!is_door("minecraft:iron_trapdoor"));
        assert!(is_trapdoor("minecraft:iron_trapdoor"));
    }

    #[test]
    fn only_the_iron_door_needs_a_button() {
        assert!(can_open_by_hand("minecraft:oak_door"));
        assert!(can_open_by_hand("minecraft:copper_door"));
        assert!(!can_open_by_hand("minecraft:iron_door"));
    }

    #[test]
    fn replaceable_cells_are_air_fluids_and_plants() {
        assert!(can_be_replaced("minecraft:air"));
        assert!(can_be_replaced("minecraft:water"));
        assert!(can_be_replaced("minecraft:short_grass"));
        assert!(!can_be_replaced("minecraft:stone"));
        assert!(!can_be_replaced("minecraft:oak_door"));
    }

    #[test]
    fn horizontal_blocks_face_away_from_the_player() {
        // yaw 0 looks south, so the block faces north (`getHorizontalDirection().getOpposite()`).
        let rot = Rotation::new(0.0, 0.0);
        assert_eq!(
            placement_facing("minecraft:ender_chest", Some(rot), UP),
            Some(NORTH)
        );
        assert_eq!(
            placement_facing("minecraft:furnace", Some(rot), UP),
            Some(NORTH)
        );
        assert_eq!(
            placement_facing("minecraft:trapped_chest", Some(rot), UP),
            Some(NORTH)
        );
        // Looking east (yaw -90) makes the front face west.
        let rot = Rotation::new(-90.0, 0.0);
        assert_eq!(
            placement_facing("minecraft:ender_chest", Some(rot), UP),
            Some(WEST)
        );
        assert_eq!(
            placement_facing("minecraft:white_glazed_terracotta", Some(rot), UP),
            Some(WEST)
        );
    }

    #[test]
    fn six_way_blocks_use_the_nearest_looking_direction() {
        // Looking straight down places a barrel facing up.
        let rot = Rotation::new(0.0, 90.0);
        assert_eq!(placement_facing("minecraft:barrel", Some(rot), UP), Some(UP));
        // Looking south places a dispenser facing north.
        let rot = Rotation::new(0.0, 0.0);
        assert_eq!(
            placement_facing("minecraft:dispenser", Some(rot), UP),
            Some(NORTH)
        );
        assert_eq!(
            placement_facing("minecraft:piston", Some(rot), UP),
            Some(NORTH)
        );
        // The observer faces the player instead (double opposite).
        assert_eq!(
            placement_facing("minecraft:observer", Some(rot), UP),
            Some(SOUTH)
        );
    }

    #[test]
    fn hopper_shulker_rod_and_anvil_have_their_own_rules() {
        let rot = Rotation::new(0.0, 0.0);
        // Clicking the top face makes the hopper point down; side faces are mirrored.
        assert_eq!(placement_facing("minecraft:hopper", Some(rot), UP), Some(DOWN));
        assert_eq!(placement_facing("minecraft:hopper", Some(rot), NORTH), Some(SOUTH));
        // Shulker boxes and rods face the clicked face.
        assert_eq!(
            placement_facing("minecraft:shulker_box", Some(rot), WEST),
            Some(WEST)
        );
        assert_eq!(
            placement_facing("minecraft:purple_shulker_box", Some(rot), UP),
            Some(UP)
        );
        assert_eq!(
            placement_facing("minecraft:end_rod", Some(rot), DOWN),
            Some(DOWN)
        );
        // Anvils turn clockwise from the player's direction.
        assert_eq!(placement_facing("minecraft:anvil", Some(rot), UP), Some(WEST));
    }

    #[test]
    fn plain_blocks_have_no_placement_facing() {
        let rot = Rotation::new(0.0, 0.0);
        assert_eq!(placement_facing("minecraft:stone", Some(rot), UP), None);
        assert_eq!(placement_facing("minecraft:oak_log", Some(rot), UP), None);
        assert_eq!(placement_facing("minecraft:campfire", Some(rot), UP), None);
    }

    #[test]
    fn redstone_plants_and_redstone_components_have_empty_collision() {
        // Vanilla places these inside the player's own cell (`isUnobstructed` on an empty
        // collision shape).
        assert!(has_empty_collision("minecraft:redstone_wire", None));
        assert!(has_empty_collision("minecraft:torch", None));
        assert!(has_empty_collision("minecraft:wall_torch", None));
        assert!(has_empty_collision("minecraft:short_grass", None));
        assert!(has_empty_collision("minecraft:dandelion", None));
        assert!(has_empty_collision("minecraft:wheat", None));
        assert!(has_empty_collision("minecraft:melon_stem", None));
        assert!(has_empty_collision("minecraft:oak_button", None));
        assert!(has_empty_collision("minecraft:stone_pressure_plate", None));
        assert!(has_empty_collision("minecraft:lever", None));
        assert!(has_empty_collision("minecraft:rail", None));
        assert!(has_empty_collision("minecraft:oak_wall_sign", None));
        assert!(has_empty_collision("minecraft:air", None));
        assert!(has_empty_collision("minecraft:water", None));
        assert!(has_empty_collision("minecraft:fire", None));
    }

    #[test]
    fn solid_blocks_keep_their_collision() {
        assert!(!has_empty_collision("minecraft:stone", None));
        assert!(!has_empty_collision("minecraft:oak_planks", None));
        assert!(!has_empty_collision("minecraft:oak_leaves", None));
        assert!(!has_empty_collision("minecraft:azalea", None));
        assert!(!has_empty_collision("minecraft:chorus_flower", None));
        // Registered `noCollission` but with a non-empty collision shape override.
        assert!(!has_empty_collision("minecraft:cobweb", None));
        assert!(!has_empty_collision("minecraft:scaffolding", None));
        // Repeaters and comparators have a 2px collision box.
        assert!(!has_empty_collision("minecraft:repeater", None));
        assert!(!has_empty_collision("minecraft:comparator", None));
        // Only the first snow layer has an empty collision shape.
        let one = BTreeMap::from([("layers".to_string(), "1".to_string())]);
        let three = BTreeMap::from([("layers".to_string(), "3".to_string())]);
        assert!(has_empty_collision("minecraft:snow", Some(&one)));
        assert!(!has_empty_collision("minecraft:snow", Some(&three)));
        assert!(!has_empty_collision("minecraft:snow", None));
    }

    #[test]
    fn with_property_only_touches_existing_properties() {
        let block = make_block_data(
            "minecraft:ender_chest",
            &[("facing", "north"), ("waterlogged", "false")],
        );
        let faced = with_property(&block, "facing", "west").expect("facing exists");
        let data = faced.to_block_data().expect("known state");
        assert_eq!(data.name, "minecraft:ender_chest");
        assert_eq!(prop(data.properties.as_ref(), "facing"), Some("west"));
        assert_eq!(
            prop(data.properties.as_ref(), "waterlogged"),
            Some("false")
        );
        // A property the block does not have is left alone.
        assert!(with_property(&block, "half", "upper").is_none());
    }
}
