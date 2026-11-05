//! Vanilla block-break timing.
//!
//! Vanilla does not measure a break in seconds, it measures it in *damage per tick*:
//!
//! ```text
//! damage = tool_speed / hardness / (is_correct_tool ? 30 : 100)
//! ticks  = ceil(1 / damage)     when damage < 1
//!        = 1                    otherwise
//! ```
//!
//! A hand has a tool speed of 1 and is never the correct tool, so stone takes 7.5 seconds by hand
//! and a few hundred milliseconds with a pickaxe. The tool speed and whether the tool is correct for
//! a block come from the item's `minecraft:tool` component, whose rules are tag based - a rule
//! applies to a block when the block is listed in the rule's tag.

use ferrumc_data::generated::items::{DataComponent, Item, ToolImpl, ToolRule};
use ferrumc_data::generated::tags::TagData;
use ferrumc_inventories::item::ItemID;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_world::block_state_id::BlockStateId;

/// Mining speed of an empty hand.
const HAND_SPEED: f32 = 1.0;
/// Divisor used when the held item is the correct tool for the block.
const CORRECT_TOOL_DIVISOR: f32 = 30.0;
/// Divisor used when the held item is not the correct tool for the block.
const WRONG_TOOL_DIVISOR: f32 = 100.0;
/// Hardness at or below which a block breaks in a single tick.
const INSTANT_HARDNESS: f32 = 0.0;
/// Hardness below which a block cannot be broken at all.
const UNBREAKABLE_BELOW: f32 = 0.0;

/// How long a block takes to break with the given tool, in ticks.
///
/// Returns [`u64::MAX`] for unbreakable blocks. Those are normally rejected before a timer starts,
/// so the value only matters if a caller bypasses that check.
pub fn break_time_ticks(hardness: f32, item: Option<&ItemID>, block: BlockStateId) -> u64 {
    // Unbreakable blocks have a hardness of -1, which is below zero, so they are rejected before
    // the instant-break check that treats a hardness of exactly zero as one tick.
    if hardness < UNBREAKABLE_BELOW {
        return u64::MAX;
    }
    if hardness <= INSTANT_HARDNESS {
        return 1;
    }

    let (speed, correct) = tool_for_block(item, block);
    let divisor = if correct {
        CORRECT_TOOL_DIVISOR
    } else {
        WRONG_TOOL_DIVISOR
    };

    let damage = speed / hardness / divisor;
    if damage >= 1.0 {
        1
    } else {
        (1.0 / damage).ceil() as u64
    }
}

/// The mining speed of the held item against a block, and whether that item can harvest it.
fn tool_for_block(item: Option<&ItemID>, block: BlockStateId) -> (f32, bool) {
    let Some(block_name) = block_name(block) else {
        return (HAND_SPEED, false);
    };

    tool_for_block_name(item, block_name)
}

/// The mining speed of the held item against a named block, and whether that item can harvest it.
///
/// Vanilla uses the first rule that matches the block, so a rule that says the tool is too weak
/// for the block wins over a later rule that would allow it.
///
/// A block that does not require a correct tool for drops (`requires_correct_tool_for_drops ==
/// false`) is harvested with the "correct tool" divisor by *anything*, including an empty hand or
/// a block held in the hand. Using the slow divisor for those blocks made the server expect a
/// break far longer than the client actually takes, which tripped the anti-cheat revert and
/// made those blocks unbreakable whenever something was held. See the `knows_the_tool` check in
/// `digging_system.rs`.
fn tool_for_block_name(item: Option<&ItemID>, block_name: &str) -> (f32, bool) {
    let bare = block_name.strip_prefix("minecraft:").unwrap_or(block_name);
    // Blocks that do not require a correct tool are harvested by anything.
    let requires_correct = ferrumc_data::blocks::Block::by_name(bare)
        .map(|block| block.requires_correct_tool_for_drops)
        .unwrap_or(true);

    let Some(item) = item else {
        // Empty hand: correct only when the block does not require a correct tool.
        return (HAND_SPEED, !requires_correct);
    };
    let Some(item_id) = u16::try_from(item.as_u32()).ok() else {
        return (HAND_SPEED, !requires_correct);
    };
    let Some(vanilla_item) = Item::from_id(item_id) else {
        return (HAND_SPEED, !requires_correct);
    };

    let tool = vanilla_item
        .components
        .iter()
        .find(|(component, _)| *component == DataComponent::Tool)
        .and_then(|(_, component)| component.as_any().downcast_ref::<ToolImpl>());

    let Some(tool) = tool else {
        // No tool component (e.g. a block held in the hand): behave like the hand.
        return (HAND_SPEED, !requires_correct);
    };

    match tool
        .rules
        .iter()
        .find(|rule| rule_matches(rule, block_name))
    {
        Some(rule) => (
            rule.speed.unwrap_or(HAND_SPEED),
            !requires_correct || rule.correct_for_drops.unwrap_or(false),
        ),
        None => (HAND_SPEED, !requires_correct),
    }
}

/// Whether a tool rule applies to a block.
///
/// A rule names either a block, a list of blocks or a block tag. The generated data keeps the
/// JSON form of the value, so a tag is quoted and prefixed with a `#` while a list is a JSON
/// array.
pub fn rule_matches(rule: &ToolRule, block_name: &str) -> bool {
    let raw = rule.blocks.trim();

    if let Some(list) = raw
        .strip_prefix('[')
        .and_then(|list| list.strip_suffix(']'))
    {
        return list
            .split(',')
            .any(|entry| entry.trim().trim_matches('"') == block_name);
    }

    let blocks = raw.trim_matches('"');
    match blocks.strip_prefix('#') {
        Some(tag) => TagData::get_block_tag(tag)
            .is_some_and(|tag_data| tag_data.values.contains(&block_name)),
        None => blocks == block_name,
    }
}

/// The registry name of a block state, or `None` when it is unknown.
fn block_name(block: BlockStateId) -> Option<&'static str> {
    ferrumc_registry::lookup_blockstate_name(&VarInt::from(block).0.to_string())
}

/// The hardness of a block state, or `None` when it cannot be determined.
///
/// Block ids and block state ids are two different numberings: a block id only identifies the
/// block itself, while a state id identifies one of its property combinations. Looking hardness
/// up by id therefore misses every block whose states are numbered after other blocks and reports
/// the hardness of a different block for the ones below, so the block is looked up by name
/// instead.
pub fn block_hardness(block: BlockStateId) -> Option<f32> {
    let name = block_name(block)?;
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);

    ferrumc_data::blocks::Block::by_name(bare).map(|block| block.hardness)
}

#[cfg(test)]
mod tests {
    /// The hardness vanilla stores for a block that cannot be broken at all.
    const UNBREAKABLE_VANILLA: f32 = -1.0;

    use super::*;

    /// Stone in the overworld is block state id 1.
    const STONE: BlockStateId = BlockStateId::new(1);

    #[test]
    fn instabreak_blocks_finish_in_one_tick() {
        assert_eq!(break_time_ticks(INSTANT_HARDNESS, None, STONE), 1);
    }

    #[test]
    fn a_hand_takes_ticks_equal_to_hundred_times_the_hardness() {
        // damage = 1 / 1.5 / 100, so ticks = 150, which is the vanilla 7.5 seconds for stone.
        assert_eq!(break_time_ticks(1.5, None, STONE), 150);
    }

    #[test]
    fn unbreakable_blocks_never_break() {
        assert_eq!(break_time_ticks(UNBREAKABLE_VANILLA, None, STONE), u64::MAX);
    }

    #[test]
    fn a_pickaxe_is_faster_than_a_hand_on_stone() {
        let pickaxe = ItemID::from_name("minecraft:wooden_pickaxe").expect("a wooden pickaxe");
        let by_hand = break_time_ticks(1.5, None, STONE);
        let by_pickaxe = break_time_ticks(1.5, Some(&pickaxe), STONE);

        assert!(by_pickaxe < by_hand, "a pickaxe must mine stone faster");
        // Vanilla: 1.15 seconds, so 23 ticks at 20 ticks per second.
        assert_eq!(by_pickaxe, 23);
    }

    #[test]
    fn a_pickaxe_still_has_to_mine_through_dirt_by_hand() {
        let pickaxe = ItemID::from_name("minecraft:wooden_pickaxe").expect("a wooden pickaxe");
        // Dirt is not in any of the pickaxe's tool tags, so it mines at hand speed.
        let dirt = BlockStateId::new(8);
        assert_eq!(
            break_time_ticks(0.5, Some(&pickaxe), dirt),
            break_time_ticks(0.5, None, dirt)
        );
    }

    #[test]
    fn hardness_is_found_for_states_whose_id_is_not_a_block_id() {
        // Flowers are numbered in the state table after many other blocks, so a lookup by block id
        // misses them. State 2128 is `minecraft:orange_tulip`.
        let tulip = BlockStateId::new(2128);
        let hardness = block_hardness(tulip).expect("a flower has a hardness");
        assert_eq!(hardness, 0.0, "flowers break instantly");
    }

    #[test]
    fn air_has_no_hardness() {
        assert!(block_hardness(BlockStateId::new(0)).is_some());
    }

    #[test]
    fn quoted_tags_are_normalised() {
        // The item data stores the tag as a quoted, `#`-prefixed literal.
        let rule = ToolRule {
            blocks: "\"#minecraft:mineable/pickaxe\"",
            speed: None,
            correct_for_drops: None,
        };

        assert!(rule_matches(&rule, "minecraft:stone"));
        assert!(!rule_matches(&rule, "minecraft:dirt"));
    }

    #[test]
    fn a_rule_can_name_a_single_block() {
        // The rules of a sword name the cobweb directly instead of using a tag.
        let rule = ToolRule {
            blocks: "\"minecraft:cobweb\"",
            speed: Some(15.0),
            correct_for_drops: Some(true),
        };

        assert!(rule_matches(&rule, "minecraft:cobweb"));
        assert!(!rule_matches(&rule, "minecraft:stone"));
    }

    #[test]
    fn the_hardness_of_a_block_is_found_by_name() {
        // The state id of a block is not a block id, so the hardness has to be looked up by name:
        // the grass block is state 8 but block 8 is something else entirely.
        let grass_block = BlockStateId::new(8);
        assert_eq!(block_name(grass_block), Some("minecraft:grass_block"));
        assert_eq!(block_hardness(grass_block), Some(0.6));

        // A block that is numbered late in the state table has a hardness as well.
        let iron_ore = BlockStateId::new(131);
        assert_eq!(block_hardness(iron_ore), Some(3.0));
    }

    #[test]
    fn stone_has_its_vanilla_hardness() {
        let hardness = block_hardness(BlockStateId::new(1)).expect("stone has a hardness");
        assert_eq!(hardness, 1.5);
    }
}
