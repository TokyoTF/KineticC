//! What a broken block drops.
//!
//! Vanilla decides this with loot tables. This module models the parts of them that matter for
//! a survival server: which item a block drops, how many of it, and whether the player was
//! holding the right tool to get it at all.
//!
//! The harvest rule follows vanilla: a block that is flagged as needing the correct tool only
//! drops anything when the held item is correct for it, and every other block always drops. An
//! item is correct for a block when the item's `minecraft:tool` component says so. Its rules are
//! checked in order and the first rule that matches the block wins, so a tool that is too weak
//! for a block is rejected by the rule that says so even when a later rule would allow it.
//!
//! Fortune and Silk Touch are not modelled. The held item is only known by its id, so the
//! enchantments on it cannot be read, and every block that vanilla only drops with Silk Touch
//! drops nothing here.

use ferrumc_data::generated::items::{DataComponent, Item, ToolImpl, ToolRule};
use ferrumc_data::generated::tags::TagData;
use ferrumc_inventories::item::ItemID;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_world::block_state_id::BlockStateId;
use rand::Rng;

/// What a broken block drops.
pub struct BlockDrop {
    /// The item handed to the player.
    pub item: ItemID,
    /// How many units of it.
    pub count: u32,
}

/// How many items a block drops.
#[derive(Clone, Copy)]
enum Count {
    /// Always the same amount.
    Fixed(u32),
    /// A random amount between the two bounds, both of them included.
    Uniform(u32, u32),
}

impl Count {
    /// Picks how many of the item are dropped.
    fn roll(self) -> u32 {
        match self {
            Count::Fixed(count) => count,
            Count::Uniform(min, max) => rand::thread_rng().gen_range(min..=max),
        }
    }
}

/// Blocks that never drop anything, no matter which tool is used.
///
/// The air-like blocks are not blocks at all, the unbreakable ones cannot be mined and the
/// remaining ones have an empty loot table or one that needs Silk Touch, so mining them drops
/// nothing.
const NO_DROP_BLOCKS: &[&str] = &[
    // Air and the other air-like blocks.
    "minecraft:air",
    "minecraft:cave_air",
    "minecraft:void_air",
    // Blocks that cannot be broken.
    "minecraft:barrier",
    "minecraft:bedrock",
    "minecraft:chain_command_block",
    "minecraft:command_block",
    "minecraft:end_gateway",
    "minecraft:end_portal",
    "minecraft:end_portal_frame",
    "minecraft:jigsaw",
    "minecraft:light",
    "minecraft:nether_portal",
    "minecraft:repeating_command_block",
    "minecraft:structure_block",
    "minecraft:structure_void",
    "minecraft:test_block",
    "minecraft:test_instance_block",
    // Blocks whose loot table is empty.
    "minecraft:blue_ice",
    "minecraft:cave_vines",
    "minecraft:cave_vines_plant",
    "minecraft:chiseled_bookshelf",
    "minecraft:frosted_ice",
    "minecraft:glass",
    "minecraft:glass_pane",
    "minecraft:ice",
    "minecraft:packed_ice",
    "minecraft:reinforced_deepslate",
    "minecraft:spawner",
    "minecraft:trial_spawner",
    "minecraft:turtle_egg",
    // Blocks that are used up when they are broken.
    "minecraft:sculk_catalyst",
    "minecraft:sculk_sensor",
    "minecraft:sculk_shrieker",
];

/// The chance of leaves dropping a stick.
const LEAF_STICK_CHANCE: f64 = 0.02;

/// The chance of leaves dropping an apple. Only oak leaves drop them.
const LEAF_APPLE_CHANCE: f64 = 0.005;

/// The chance of a plant dropping seeds.
const SEED_CHANCE: f64 = 0.125;

/// The chance of gravel dropping flint.
const FLINT_CHANCE: f64 = 0.1;

/// What the given block state drops when mined with the held item.
///
/// Vanilla can drop more than one item at a time, for example a bush that drops both sticks and
/// an apple, so this returns a list.
pub fn block_drops(block: BlockStateId, held_item: Option<&ItemID>) -> Vec<BlockDrop> {
    let Some(name) = block_name(block) else {
        return Vec::new();
    };

    if NO_DROP_BLOCKS.contains(&name) {
        return Vec::new();
    }

    if !can_harvest(name, held_item) {
        return Vec::new();
    }

    drops_of(block, name, held_item)
}

/// The items dropped by a block that passed the harvest checks.
fn drops_of(block: BlockStateId, name: &str, held_item: Option<&ItemID>) -> Vec<BlockDrop> {
    let shears = holds_shears(held_item);

    // Leaves and plants have loot tables of their own.
    if let Some(drops) = leaf_drops(name, shears) {
        return drops;
    }

    if let Some(drops) = plant_drops(name, shears) {
        return drops;
    }

    // A snow layer drops one snowball for every layer that it has.
    if name == "minecraft:snow" {
        let layers = block_property(block, "layers")
            .and_then(|layers| layers.parse::<u32>().ok())
            .unwrap_or(1);

        return drops_of_item("minecraft:snowball", layers);
    }

    // A double slab drops two slabs.
    if is_double_slab(block, name) {
        return drops_of_item(name, 2);
    }

    // Cave vines only drop the berries that they carry.
    if name == "minecraft:cave_vines" || name == "minecraft:cave_vines_plant" {
        return match block_property(block, "berries").as_deref() {
            Some("true") => drops_of_item("minecraft:glow_berries", 1),
            _ => Vec::new(),
        };
    }

    // A berry bush only drops berries when it is ripe.
    if name == "minecraft:sweet_berry_bush" {
        let berries = match block_property(block, "age").as_deref() {
            Some("3") => Some(rand::thread_rng().gen_range(2..=3)),
            Some("2") => Some(rand::thread_rng().gen_range(1..=2)),
            _ => None,
        };

        return match berries {
            Some(count) => drops_of_item("minecraft:sweet_berries", count),
            None => Vec::new(),
        };
    }

    // Gravel has a small chance of dropping flint as well.
    if name == "minecraft:gravel" {
        let mut drops = drops_of_item(name, 1);

        if roll_chance(FLINT_CHANCE) {
            drops.extend(drops_of_item("minecraft:flint", 1));
        }

        return drops;
    }

    // The wall versions of a block drop the block itself.
    if let Some(item) = wall_variant_item(name) {
        return drops_of_item(&item, 1);
    }

    // The blocks with a loot table of their own.
    if let Some((item, count)) = override_drop(name) {
        return drops_of_item(item, count.roll());
    }

    // Everything else drops itself.
    drops_of_item(name, 1)
}

/// The item and the amount that a block drops when it does not drop itself.
///
/// These are the loot tables of vanilla 1.21.8 that differ from the block itself. The
/// alternative of a table that needs Silk Touch is left out, because the enchantments of the
/// held item cannot be read.
fn override_drop(name: &str) -> Option<(&'static str, Count)> {
    let (item, count) = match name {
        // Stone that is broken into cobblestone.
        "minecraft:stone" => ("minecraft:cobblestone", Count::Fixed(1)),
        "minecraft:deepslate" => ("minecraft:cobbled_deepslate", Count::Fixed(1)),
        // Soil that drops plain dirt.
        "minecraft:grass_block" => ("minecraft:dirt", Count::Fixed(1)),
        "minecraft:mycelium" => ("minecraft:dirt", Count::Fixed(1)),
        "minecraft:podzol" => ("minecraft:dirt", Count::Fixed(1)),
        "minecraft:dirt_path" => ("minecraft:dirt", Count::Fixed(1)),
        "minecraft:farmland" => ("minecraft:dirt", Count::Fixed(1)),
        "minecraft:crimson_nylium" => ("minecraft:netherrack", Count::Fixed(1)),
        "minecraft:warped_nylium" => ("minecraft:netherrack", Count::Fixed(1)),
        // Infested blocks drop the block that they took the place of.
        "minecraft:infested_stone" => ("minecraft:stone", Count::Fixed(1)),
        "minecraft:infested_cobblestone" => ("minecraft:cobblestone", Count::Fixed(1)),
        "minecraft:infested_stone_bricks" => ("minecraft:stone_bricks", Count::Fixed(1)),
        "minecraft:infested_mossy_stone_bricks" => {
            ("minecraft:mossy_stone_bricks", Count::Fixed(1))
        }
        "minecraft:infested_cracked_stone_bricks" => {
            ("minecraft:cracked_stone_bricks", Count::Fixed(1))
        }
        "minecraft:infested_chiseled_stone_bricks" => {
            ("minecraft:chiseled_stone_bricks", Count::Fixed(1))
        }
        "minecraft:infested_deepslate" => ("minecraft:deepslate", Count::Fixed(1)),
        // Ores drop the material inside of them.
        "minecraft:coal_ore" => ("minecraft:coal", Count::Fixed(1)),
        "minecraft:deepslate_coal_ore" => ("minecraft:coal", Count::Fixed(1)),
        "minecraft:iron_ore" => ("minecraft:raw_iron", Count::Fixed(1)),
        "minecraft:deepslate_iron_ore" => ("minecraft:raw_iron", Count::Fixed(1)),
        "minecraft:copper_ore" => ("minecraft:raw_copper", Count::Uniform(2, 5)),
        "minecraft:deepslate_copper_ore" => ("minecraft:raw_copper", Count::Uniform(2, 5)),
        "minecraft:gold_ore" => ("minecraft:raw_gold", Count::Fixed(1)),
        "minecraft:deepslate_gold_ore" => ("minecraft:raw_gold", Count::Fixed(1)),
        "minecraft:redstone_ore" => ("minecraft:redstone", Count::Uniform(4, 5)),
        "minecraft:deepslate_redstone_ore" => ("minecraft:redstone", Count::Uniform(4, 5)),
        "minecraft:lapis_ore" => ("minecraft:lapis_lazuli", Count::Uniform(4, 9)),
        "minecraft:deepslate_lapis_ore" => ("minecraft:lapis_lazuli", Count::Uniform(4, 9)),
        "minecraft:diamond_ore" => ("minecraft:diamond", Count::Fixed(1)),
        "minecraft:deepslate_diamond_ore" => ("minecraft:diamond", Count::Fixed(1)),
        "minecraft:emerald_ore" => ("minecraft:emerald", Count::Fixed(1)),
        "minecraft:deepslate_emerald_ore" => ("minecraft:emerald", Count::Fixed(1)),
        "minecraft:nether_quartz_ore" => ("minecraft:quartz", Count::Fixed(1)),
        "minecraft:nether_gold_ore" => ("minecraft:gold_nugget", Count::Uniform(2, 6)),
        // Blocks that break into several pieces.
        "minecraft:clay" => ("minecraft:clay_ball", Count::Fixed(4)),
        "minecraft:snow_block" => ("minecraft:snowball", Count::Fixed(4)),
        "minecraft:glowstone" => ("minecraft:glowstone_dust", Count::Uniform(2, 4)),
        "minecraft:melon" => ("minecraft:melon_slice", Count::Uniform(3, 7)),
        "minecraft:sea_lantern" => ("minecraft:prismarine_crystals", Count::Uniform(2, 3)),
        "minecraft:amethyst_cluster" => ("minecraft:amethyst_shard", Count::Fixed(4)),
        "minecraft:bookshelf" => ("minecraft:book", Count::Fixed(3)),
        "minecraft:cocoa" => ("minecraft:cocoa_beans", Count::Fixed(3)),
        "minecraft:ender_chest" => ("minecraft:obsidian", Count::Fixed(8)),
        "minecraft:creaking_heart" => ("minecraft:resin_clump", Count::Uniform(1, 3)),
        "minecraft:sea_pickle" => ("minecraft:sea_pickle", Count::Fixed(4)),
        "minecraft:nether_wart" => ("minecraft:nether_wart", Count::Uniform(2, 4)),
        "minecraft:red_mushroom_block" => ("minecraft:red_mushroom", Count::Uniform(0, 2)),
        "minecraft:brown_mushroom_block" => ("minecraft:brown_mushroom", Count::Uniform(0, 2)),
        // Crops drop their seeds instead of the plant.
        "minecraft:wheat" => ("minecraft:wheat_seeds", Count::Uniform(1, 3)),
        "minecraft:carrots" => ("minecraft:carrot", Count::Uniform(1, 3)),
        "minecraft:potatoes" => ("minecraft:potato", Count::Uniform(1, 3)),
        "minecraft:beetroots" => ("minecraft:beetroot_seeds", Count::Uniform(1, 3)),
        "minecraft:pumpkin_stem" => ("minecraft:pumpkin_seeds", Count::Uniform(1, 3)),
        "minecraft:attached_pumpkin_stem" => ("minecraft:pumpkin_seeds", Count::Uniform(1, 3)),
        "minecraft:melon_stem" => ("minecraft:melon_seeds", Count::Uniform(1, 3)),
        "minecraft:attached_melon_stem" => ("minecraft:melon_seeds", Count::Uniform(1, 3)),
        "minecraft:torchflower_crop" => ("minecraft:torchflower_seeds", Count::Fixed(1)),
        "minecraft:pitcher_crop" => ("minecraft:pitcher_pod", Count::Fixed(1)),
        // Coral turns into its dead version.
        "minecraft:tube_coral_block" => ("minecraft:dead_tube_coral_block", Count::Fixed(1)),
        "minecraft:brain_coral_block" => ("minecraft:dead_brain_coral_block", Count::Fixed(1)),
        "minecraft:bubble_coral_block" => ("minecraft:dead_bubble_coral_block", Count::Fixed(1)),
        "minecraft:fire_coral_block" => ("minecraft:dead_fire_coral_block", Count::Fixed(1)),
        "minecraft:horn_coral_block" => ("minecraft:dead_horn_coral_block", Count::Fixed(1)),
        // The odd ones out.
        "minecraft:cobweb" => ("minecraft:string", Count::Fixed(1)),
        "minecraft:tripwire" => ("minecraft:string", Count::Fixed(1)),
        "minecraft:redstone_wire" => ("minecraft:redstone", Count::Fixed(1)),
        "minecraft:dead_bush" => ("minecraft:stick", Count::Uniform(0, 2)),
        "minecraft:chorus_plant" => ("minecraft:chorus_fruit", Count::Uniform(0, 1)),
        "minecraft:campfire" => ("minecraft:charcoal", Count::Fixed(2)),
        "minecraft:soul_campfire" => ("minecraft:soul_soil", Count::Fixed(1)),
        _ => return None,
    };

    Some((item, count))
}

/// The sapling that a tree drops when its leaves are broken, with the chance of dropping one.
///
/// Mangrove leaves are not in here because they drop a propagule instead of a sapling.
fn leaf_sapling(name: &str) -> Option<(&'static str, f64)> {
    let (sapling, chance) = match name {
        "minecraft:oak_leaves" => ("minecraft:oak_sapling", 0.05),
        "minecraft:spruce_leaves" => ("minecraft:spruce_sapling", 0.05),
        "minecraft:birch_leaves" => ("minecraft:birch_sapling", 0.05),
        "minecraft:jungle_leaves" => ("minecraft:jungle_sapling", 0.025),
        "minecraft:acacia_leaves" => ("minecraft:acacia_sapling", 0.05),
        "minecraft:dark_oak_leaves" => ("minecraft:dark_oak_sapling", 0.05),
        "minecraft:pale_oak_leaves" => ("minecraft:pale_oak_sapling", 0.05),
        "minecraft:cherry_leaves" => ("minecraft:cherry_sapling", 0.05),
        "minecraft:azalea_leaves" => ("minecraft:azalea", 0.05),
        "minecraft:flowering_azalea_leaves" => ("minecraft:flowering_azalea", 0.05),
        _ => return None,
    };

    Some((sapling, chance))
}

/// The items dropped by a block of leaves.
///
/// Leaves drop a sapling now and then, some sticks and, for oak leaves, an apple. Each of those
/// is a pool of its own, so a single bush can drop more than one of them.
fn leaf_drops(name: &str, shears: bool) -> Option<Vec<BlockDrop>> {
    let sapling = leaf_sapling(name);

    let is_leaf = sapling.is_some()
        || TagData::get_block_tag("minecraft:leaves").is_some_and(|tag| tag.values.contains(&name));

    if !is_leaf {
        return None;
    }

    // Shears keep the leaves intact.
    if shears {
        return Some(drops_of_item(name, 1));
    }

    let mut drops = Vec::new();

    if let Some((sapling, chance)) = sapling {
        if roll_chance(chance) {
            drops.extend(drops_of_item(sapling, 1));
        }
    }

    if roll_chance(LEAF_STICK_CHANCE) {
        let count = rand::thread_rng().gen_range(1..=2);
        drops.extend(drops_of_item("minecraft:stick", count));
    }

    if (name == "minecraft:oak_leaves" || name == "minecraft:dark_oak_leaves")
        && roll_chance(LEAF_APPLE_CHANCE)
    {
        drops.extend(drops_of_item("minecraft:apple", 1));
    }

    Some(drops)
}

/// The items dropped by a plant.
///
/// Grass and ferns drop seeds now and then, and the plants that are cut with shears drop
/// themselves instead.
fn plant_drops(name: &str, shears: bool) -> Option<Vec<BlockDrop>> {
    match name {
        // Grass and ferns drop seeds now and then.
        "minecraft:short_grass" | "minecraft:fern" | "minecraft:tall_grass" => {
            if shears {
                // Tall grass is two blocks high, so cutting it gives two short grass blocks.
                let (item, count) = if name == "minecraft:tall_grass" {
                    ("minecraft:short_grass", 2)
                } else {
                    (name, 1)
                };

                Some(drops_of_item(item, count))
            } else if roll_chance(SEED_CHANCE) {
                Some(drops_of_item("minecraft:wheat_seeds", 1))
            } else {
                Some(Vec::new())
            }
        }
        // These are only dropped when they are cut with shears.
        "minecraft:short_dry_grass"
        | "minecraft:tall_dry_grass"
        | "minecraft:vine"
        | "minecraft:seagrass"
        | "minecraft:tall_seagrass" => {
            if shears {
                // Tall sea grass is two blocks high as well.
                let (item, count) = if name == "minecraft:tall_seagrass" {
                    ("minecraft:seagrass", 2)
                } else {
                    (name, 1)
                };

                Some(drops_of_item(item, count))
            } else {
                Some(Vec::new())
            }
        }
        _ => None,
    }
}

/// The item that the wall version of a block drops.
///
/// Vanilla handles these in code instead of in a loot table: a wall torch drops a torch and a
/// wall sign drops the sign of the same wood.
fn wall_variant_item(name: &str) -> Option<String> {
    match name {
        "minecraft:wall_torch" => Some("minecraft:torch".to_string()),
        "minecraft:redstone_wall_torch" => Some("minecraft:redstone_torch".to_string()),
        "minecraft:soul_wall_torch" => Some("minecraft:soul_torch".to_string()),
        _ => {
            let block = name.strip_prefix("minecraft:")?;

            for (wall_suffix, suffix) in [("_wall_sign", "_sign"), ("_wall_banner", "_banner")] {
                if let Some(base) = block.strip_suffix(wall_suffix) {
                    return Some(format!("minecraft:{base}{suffix}"));
                }
            }

            None
        }
    }
}

/// Whether a block is a slab that is standing on another one.
fn is_double_slab(block: BlockStateId, name: &str) -> bool {
    TagData::get_block_tag("minecraft:slabs").is_some_and(|tag| tag.values.contains(&name))
        && block_property(block, "type").as_deref() == Some("double")
}

/// The value of a property of a block state.
fn block_property(block: BlockStateId, property: &str) -> Option<String> {
    block.to_block_data()?.properties?.get(property).cloned()
}

/// Whether the held item is a pair of shears.
fn holds_shears(held_item: Option<&ItemID>) -> bool {
    held_item.is_some_and(|item| item.to_name().as_deref() == Some("minecraft:shears"))
}

/// Whether a random roll with the given chance succeeds.
fn roll_chance(chance: f64) -> bool {
    rand::thread_rng().gen_bool(chance)
}

/// The drops of a single item, if there are any of it.
fn drops_of_item(item: &str, count: u32) -> Vec<BlockDrop> {
    if count == 0 {
        return Vec::new();
    }

    match ItemID::from_name(item) {
        Some(item) => vec![BlockDrop { item, count }],
        None => Vec::new(),
    }
}

/// Whether mining this block with the held item produces a drop.
///
/// A block that does not need the correct tool always drops. A block that does only drops when
/// the held item is correct for it, which is decided by the rules of the item's
/// `minecraft:tool` component.
fn can_harvest(block_name: &str, held_item: Option<&ItemID>) -> bool {
    if !requires_correct_tool(block_name) {
        return true;
    }

    let Some(tool) = held_item.and_then(tool_component) else {
        return false;
    };

    // Vanilla uses the first rule that matches the block, so a rule that says the tool is too
    // weak for the block wins over a later rule that would allow it.
    tool.rules
        .iter()
        .find(|rule| rule_matches(rule, block_name))
        .is_some_and(|rule| rule.correct_for_drops.unwrap_or(false))
}

/// Whether a block only drops its items when it is mined with the correct tool.
///
/// Vanilla keeps this as a per block flag. Blocks without it, such as dirt or a flower, drop
/// their items even when they are mined by hand.
fn requires_correct_tool(block_name: &str) -> bool {
    let bare_name = block_name.strip_prefix("minecraft:").unwrap_or(block_name);

    ferrumc_data::blocks::Block::by_name(bare_name)
        .is_some_and(|block| block.requires_correct_tool_for_drops)
}

/// The `minecraft:tool` component of an item, if the item has one.
fn tool_component(item: &ItemID) -> Option<&'static ToolImpl> {
    let id = u16::try_from(item.as_u32()).ok()?;
    let vanilla_item = Item::from_id(id)?;

    vanilla_item
        .components
        .iter()
        .find(|(component, _)| *component == DataComponent::Tool)
        .and_then(|(_, component)| component.as_any().downcast_ref::<ToolImpl>())
}

/// Whether a tool rule applies to a block.
///
/// A rule names either a block, a list of blocks or a block tag. The generated data keeps the
/// JSON form of the value, so a tag is quoted and prefixed with a `#` while a list is a JSON
/// array.
fn rule_matches(rule: &ToolRule, block_name: &str) -> bool {
    crate::systems::mining::rule_matches(rule, block_name)
}

/// The registry name of a block state.
fn block_name(block: BlockStateId) -> Option<&'static str> {
    ferrumc_registry::lookup_blockstate_name(&VarInt::from(block).0.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stone in the overworld is block state id 1.
    const STONE: BlockStateId = BlockStateId::new(1);
    /// The grass block is block state id 8 and drops dirt.
    const GRASS_BLOCK: BlockStateId = BlockStateId::new(8);
    /// Dirt is block state id 10.
    const DIRT: BlockStateId = BlockStateId::new(10);
    /// Coarse dirt is block state id 11.
    const COARSE_DIRT: BlockStateId = BlockStateId::new(11);
    /// Red sand is block state id 123.
    const RED_SAND: BlockStateId = BlockStateId::new(123);
    /// Iron ore is block state id 131.
    const IRON_ORE: BlockStateId = BlockStateId::new(131);
    /// State 2128 is `minecraft:orange_tulip`.
    const TULIP: BlockStateId = BlockStateId::new(2128);
    /// Clay is block state id 5977.
    const CLAY: BlockStateId = BlockStateId::new(5977);
    /// Rooted dirt is block state id 25962.
    const ROOTED_DIRT: BlockStateId = BlockStateId::new(25962);

    fn item(name: &str) -> ItemID {
        ItemID::from_name(name).unwrap_or_else(|| panic!("{name} is an item"))
    }

    /// The only drop of a block, which fails when the block drops something else.
    fn only_drop(block: BlockStateId, held_item: Option<&ItemID>) -> BlockDrop {
        let drops = block_drops(block, held_item);

        assert_eq!(drops.len(), 1, "{block} should drop exactly one item");

        drops.into_iter().next().unwrap()
    }

    #[test]
    fn dirt_always_drops_dirt() {
        let drop = only_drop(DIRT, None);
        assert_eq!(drop.item, item("minecraft:dirt"));
        assert_eq!(drop.count, 1);
    }

    #[test]
    fn a_grass_block_drops_dirt() {
        let drop = only_drop(GRASS_BLOCK, None);
        assert_eq!(drop.item, item("minecraft:dirt"));
    }

    #[test]
    fn stone_drops_nothing_by_hand() {
        assert!(
            block_drops(STONE, None).is_empty(),
            "stone needs a pickaxe to drop anything"
        );
    }

    #[test]
    fn stone_drops_cobblestone_with_a_pickaxe() {
        let pickaxe = item("minecraft:wooden_pickaxe");
        let drop = only_drop(STONE, Some(&pickaxe));
        assert_eq!(drop.item, item("minecraft:cobblestone"));
        assert_eq!(drop.count, 1);
    }

    #[test]
    fn flowers_drop_themselves_by_hand() {
        let drop = only_drop(TULIP, None);
        assert_eq!(drop.item, item("minecraft:orange_tulip"));
        assert_eq!(drop.count, 1);
    }

    #[test]
    fn unbreakable_and_unknown_blocks_drop_nothing() {
        assert!(block_drops(BlockStateId::new(0), None).is_empty());
        assert!(block_drops(BlockStateId::new(u32::MAX), None).is_empty());
    }

    #[test]
    fn an_iron_ore_needs_a_stone_pickaxe() {
        // Iron ore is one of the blocks that need a stone pickaxe, so a wooden pickaxe is too
        // weak for it even though a wooden pickaxe is enough for stone.
        assert_eq!(block_name(IRON_ORE), Some("minecraft:iron_ore"));

        let wooden_pickaxe = item("minecraft:wooden_pickaxe");
        assert!(block_drops(IRON_ORE, Some(&wooden_pickaxe)).is_empty());

        let stone_pickaxe = item("minecraft:stone_pickaxe");
        let drop = only_drop(IRON_ORE, Some(&stone_pickaxe));
        assert_eq!(drop.item, item("minecraft:raw_iron"));
    }

    #[test]
    fn a_sword_does_not_harvest_stone() {
        let sword = item("minecraft:wooden_sword");
        assert!(block_drops(STONE, Some(&sword)).is_empty());
    }

    #[test]
    fn a_pickaxe_does_not_harvest_dirt() {
        // Dirt does not need the correct tool, so it drops no matter what is held.
        let pickaxe = item("minecraft:wooden_pickaxe");
        assert!(!block_drops(DIRT, Some(&pickaxe)).is_empty());
    }

    #[test]
    fn red_sand_drops_red_sand() {
        // Red sand is its own item, it does not drop plain sand.
        let drop = only_drop(RED_SAND, None);
        assert_eq!(drop.item, item("minecraft:red_sand"));
    }

    #[test]
    fn coarse_dirt_and_rooted_dirt_drop_themselves() {
        // Only the grass block and the path drop plain dirt.
        let coarse = only_drop(COARSE_DIRT, None);
        assert_eq!(coarse.item, item("minecraft:coarse_dirt"));

        let rooted = only_drop(ROOTED_DIRT, None);
        assert_eq!(rooted.item, item("minecraft:rooted_dirt"));
    }

    #[test]
    fn clay_drops_four_clay_balls() {
        let drop = only_drop(CLAY, None);
        assert_eq!(drop.item, item("minecraft:clay_ball"));
        assert_eq!(drop.count, 4);
    }

    #[test]
    fn a_snow_layer_needs_a_shovel_and_drops_one_snowball_per_layer() {
        // The snow layer with a single layer is block state id 5950 and the one with eight
        // layers is block state id 5957.
        let one_layer = BlockStateId::new(5950);
        let eight_layers = BlockStateId::new(5957);

        assert!(block_drops(one_layer, None).is_empty());

        let shovel = item("minecraft:wooden_shovel");

        let drop = only_drop(one_layer, Some(&shovel));
        assert_eq!(drop.item, item("minecraft:snowball"));
        assert_eq!(drop.count, 1);

        let drop = only_drop(eight_layers, Some(&shovel));
        assert_eq!(drop.item, item("minecraft:snowball"));
        assert_eq!(drop.count, 8);
    }

    #[test]
    fn a_double_slab_drops_two_slabs() {
        // The stone slab on the bottom half is block state id 12120 and the double one is
        // block state id 12122. A slab of stone needs a pickaxe to be mined.
        let bottom = BlockStateId::new(12120);
        let double = BlockStateId::new(12122);
        let pickaxe = item("minecraft:wooden_pickaxe");

        let drop = only_drop(bottom, Some(&pickaxe));
        assert_eq!(drop.item, item("minecraft:stone_slab"));
        assert_eq!(drop.count, 1);

        let drop = only_drop(double, Some(&pickaxe));
        assert_eq!(drop.item, item("minecraft:stone_slab"));
        assert_eq!(drop.count, 2);
    }

    #[test]
    fn an_infested_block_drops_the_block_it_replaced() {
        // Infested stone is block state id 6786.
        let drop = only_drop(BlockStateId::new(6786), None);
        assert_eq!(drop.item, item("minecraft:stone"));
    }

    #[test]
    fn crops_drop_their_seeds_instead_of_the_plant() {
        // Wheat is block state id 4342 and beetroots are block state id 13532.
        let wheat = only_drop(BlockStateId::new(4342), None);
        assert_eq!(wheat.item, item("minecraft:wheat_seeds"));
        assert!((1..=3).contains(&wheat.count));

        let beetroot = only_drop(BlockStateId::new(13532), None);
        assert_eq!(beetroot.item, item("minecraft:beetroot_seeds"));
        assert!((1..=3).contains(&beetroot.count));
    }

    #[test]
    fn a_copper_ore_drops_between_two_and_five_raw_copper() {
        // Copper ore is block state id 23970.
        let pickaxe = item("minecraft:stone_pickaxe");
        let drop = only_drop(BlockStateId::new(23970), Some(&pickaxe));

        assert_eq!(drop.item, item("minecraft:raw_copper"));
        assert!((2..=5).contains(&drop.count));
    }

    #[test]
    fn leaves_only_drop_saplings_sticks_and_apples() {
        // Oak leaves are block state id 252.
        let leaves = BlockStateId::new(252);

        for _ in 0..64 {
            for drop in block_drops(leaves, None) {
                let name = drop.item.to_name().expect("the drop has a name");

                match name.as_str() {
                    "minecraft:oak_sapling" | "minecraft:apple" => assert_eq!(drop.count, 1),
                    "minecraft:stick" => assert!((1..=2).contains(&drop.count)),
                    other => panic!("leaves dropped {other}"),
                }
            }
        }
    }

    #[test]
    fn shears_keep_leaves_intact() {
        let leaves = BlockStateId::new(252);
        let shears = item("minecraft:shears");

        let drop = only_drop(leaves, Some(&shears));
        assert_eq!(drop.item, item("minecraft:oak_leaves"));
        assert_eq!(drop.count, 1);
    }

    #[test]
    fn gravel_drops_gravel_and_maybe_flint() {
        // Gravel is block state id 124.
        let drops = block_drops(BlockStateId::new(124), None);

        let gravel = drops
            .iter()
            .find(|drop| drop.item == item("minecraft:gravel"))
            .expect("gravel drops itself");

        assert_eq!(gravel.count, 1);

        for drop in &drops {
            assert!(
                drop.item == item("minecraft:gravel") || drop.item == item("minecraft:flint"),
                "gravel dropped something else"
            );
        }
    }

    #[test]
    fn a_wall_torch_drops_a_torch() {
        // The wall torch is block state id 2402.
        let drop = only_drop(BlockStateId::new(2402), None);
        assert_eq!(drop.item, item("minecraft:torch"));
    }
}
