# Containers

This document describes how the server models and synchronises container screens, starting with the
chest. The same pieces apply to any future container that is not the player's own inventory.

## Block entities

A block state describes a block, but not everything attached to it. A chest carries an inventory, and
that inventory has to outlive the server process. The world therefore stores a small block-entity
payload per position, keyed by dimension and coordinates
(`World::set_block_entity_data` / `get_block_entity_data` / `remove_block_entity_data`, in
`src/lib/world/src/player.rs`).

`ChestStore` (`src/bin/src/systems/chests.rs`) caches those payloads in memory:

- The first look at a chest loads its payload, or creates an empty 27-slot inventory.
- Every write persists the payload before the cache is updated, so a restart sees the last state.
- Breaking the block removes the payload.

One chest *block* always owns one 27-slot inventory. A double chest is a *view* over two of them, not
a single 54-slot block entity. This mirrors vanilla, where each half is its own block entity and only
the menu merges them.

## Single and double chests

A chest block state has a `facing` and a `type` property:

| property | values |
| --- | --- |
| `facing` | `north`, `south`, `west`, `east` |
| `type` | `single`, `left`, `right` |
| `waterlogged` | `true`, `false` |

`ChestState` reads and writes those properties through `BlockStateId::to_block_data` /
`BlockStateId::from_block_data`.

Placement (`place_chest`, called from `place_block.rs`):

1. The chest faces the opposite of the player's yaw, as `Direction.getOpposite()` does in vanilla.
   `Facing::from_player_yaw` implements `Direction.fromYRot` (yaw 0 south, 90 west, 180 north,
   270 east).
2. The two horizontal neighbours perpendicular to the facing are checked. A neighbour joins when it
   is a single chest with the same facing.
3. The neighbour clockwise of the facing becomes `right` and the placed block becomes `left`. This is
   vanilla's `updateShape` mapping and it is what `resolve_chest` relies on later.
4. Both changed states are broadcast as block updates, so clients draw the whole chest.

`resolve_chest` reads a chest's `type` and finds its partner: `left` pairs clockwise, `right` pairs
counter-clockwise. The pair is always ordered by coordinate (lowest `x`, then `z` first), so the
screen's first 27 slots always belong to the same half no matter which half was clicked. A half whose
partner is missing is treated as a single chest.

## Container screens

Opening a chest is a right click on the block. The client sends that as `use_item_on`, which arrives
in the same packet as block placement. `place_block::handle` therefore checks the clicked block
first: if it is a chest, the event opens a screen and no block is placed. This handler is the only
reader of `PlaceBlockReceiver`, so a separate chest system cannot steal the events.

The window layout depends on the pair:

| chest | `minecraft:menu` id | window slots |
| --- | --- | --- |
| single | `generic_9x3` (2) | 27 container + 27 main + 9 hotbar |
| double | `generic_9x6` (5) | 54 container + 27 main + 9 hotbar |

There is no armour or offhand slot in a chest screen. `chest_slots::to_player_slot` maps a window
index to the player's own 46-slot inventory numbering; anything that is not a player slot returns
`None`.

The server always uses window id `1`. Opening a second chest sends a container-close for the old
window first, then the new open screen, so the client does not keep a stale screen.

`SetContainerContent` sends every menu slot: the container slots first, then the player's 27 main
inventory slots and 9 hotbar slots. The client re-initialises each menu slot from this list, so a
short list would leave the player's half of the screen blank. It is sent once when the screen opens;
clicks are not echoed back. Echoing a full content packet per click would also send the `carried_item`
field, which would overwrite the client's cursor stack.

## Clicks

`update_survival_mode_slot.rs` is the only reader of `ClickContainer`. The client reports the slots it
believes changed, and the server applies exactly that diff:

- Window `0` is the player's own inventory. Slots are the player's, including the 2x2 crafting grid;
  a click on the grid recomputes the result and taking the result consumes the ingredients.
- Any other window is a chest. Indices below the container size are chest slots; the rest map to the
  player's main inventory and hotbar with `chest_slots::to_player_slot`.

A reported slot whose data is absent means the client cleared it. `Inventory::set_slot_with_update`
applies `None` as a real clear; skipping those would make removed stacks reappear on the next open.

Every index is validated first: negative indices (clicking outside the window) and indices past the
end are logged and dropped, never unwrapped.

## Lid animation

The lid is not a block state change. Vanilla sends a `block_event` packet with action `1` and
parameter `1` to open, `0` to close. Both halves of a double chest receive the event, so the whole lid
moves. The client plays the matching open/close sound itself.

`BlockEvent` (`src/lib/net/src/packets/outgoing/block_event.rs`) carries the block *registry* id
(`188` for `minecraft:chest`, via `lookup_block_protocol_id`), not a block state id.

Events are broadcast to players within 64 blocks when a screen opens or closes. `CloseContainer` is
read by `close_container.rs`, which is also where the chest screen and lid are closed; the chest
system no longer reads that channel.

## Breaking a chest

`BlockBrokenEvent` carries the state the block had before it was removed, so
`drop_chest_contents_on_break` can tell whether it was a chest and which half it was.

- Both halves are removed from the store and their block-entity payloads deleted.
- All stacks from both halves drop at the broken block.
- The surviving half is rewritten with `type=single` and the new state is broadcast, so clients stop
  drawing a double chest with a missing half.
- Anyone with that screen open receives a container-close.

The system is ordered after both break paths: survival digging
(`digging_system::handle_finish_digging`) and creative instant break (`player_action::handle`).

## Known gaps

- Furnaces, hoppers, barrels, shulker boxes and other container types are not implemented.
- Chests placed in water are not waterlogged; the placed state always uses `waterlogged=false`.
- Vanilla refuses to form a double chest when a block sits above either half. That rule is not
  enforced, and placing a block above an existing pair does not split it.
- The click handler trusts the client's reported diff instead of re-implementing every click mode
  (quick move, swap, drag, drop). This is safe for well-behaved clients but is not the vanilla
  server-side authority model.
- Clicking outside a chest window does not drop the carried stack into the world.
