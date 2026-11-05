//! Chests: block entities with an inventory, opened as a container screen.
//!
//! A chest is a block that carries state, so the block state alone is not enough to describe the
//! world: the contents live in a block entity keyed by position and dimension. The contents are held
//! in an ECS resource, written through to the database whenever they change, so they survive a
//! restart exactly like the blocks do.
//!
//! Two chests placed side by side become a double chest. Vanilla models this with the `type` block
//! state property (`single`, `left`, `right`): each half keeps its own block entity and inventory,
//! and the screen shows a compound view of both. Breaking either half drops everything and turns the
//! survivor back into a single chest.
//!
//! Opening a chest is a request from the client that arrives in the *block placement* packet, because
//! a right click is the same packet whether the player places or interacts. [`super::chests`] only
//! supplies the state model; the placement handler owns that receiver and calls [`open_chest`].
//!
//! The lid animation is a `block_event` packet: the block state never changes, the client only plays
//! the open or close animation. Vanilla sends it for both halves of a double chest, so the whole lid
//! moves, and lets the client play the matching sound itself.

use bevy_ecs::prelude::{Commands, Component, Entity, Query, Res, Resource};
use ferrumc_core::transform::position::Position;
use ferrumc_inventories::inventory::Inventory;
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_net::connection::StreamWriter;
use ferrumc_net::packets::outgoing::block_event::BlockEvent;
use ferrumc_net::packets::outgoing::block_update::BlockUpdate;
use ferrumc_net::packets::outgoing::container_close::ContainerClose;
use ferrumc_net::packets::outgoing::open_screen::OpenScreen;
use ferrumc_net::packets::outgoing::set_container_content::SetContainerContent;
use ferrumc_net_codec::net_types::network_position::NetworkPosition;
use ferrumc_net_codec::net_types::var_int::VarInt;
use ferrumc_state::GlobalStateResource;
use ferrumc_text::{TextComponent, TextComponentBuilder};
use ferrumc_world::block_state_id::BlockStateId;
use ferrumc_world::dimension::Dimension;
use ferrumc_world::pos::BlockPos;
use ferrumc_world::vanilla_chunk_format::BlockData;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use tracing::{debug, warn};

/// Number of slots in one chest half.
pub const CHEST_SLOT_COUNT: usize = 27;

/// Number of slots in a double chest, which is two halves side by side.
pub const DOUBLE_CHEST_SLOT_COUNT: usize = CHEST_SLOT_COUNT * 2;

/// The window id a chest screen is opened in.
pub const CHEST_WINDOW_ID: i32 = 1;

/// The block name every chest state shares.
pub const CHEST_BLOCK_NAME: &str = "minecraft:chest";

/// Radius, in blocks, within which a lid animation is broadcast. Vanilla uses the same distance for
/// every block event so distant players are not woken up by a chest opening.
const LID_EVENT_RANGE: f64 = 64.0;

/// The four horizontal directions a chest can face.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    North,
    South,
    West,
    East,
}

impl Facing {
    /// The value the `facing` block state property uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            Facing::North => "north",
            Facing::South => "south",
            Facing::West => "west",
            Facing::East => "east",
        }
    }

    /// Parses a `facing` block state property.
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "north" => Some(Facing::North),
            "south" => Some(Facing::South),
            "west" => Some(Facing::West),
            "east" => Some(Facing::East),
            _ => None,
        }
    }

    /// The horizontal block offset this direction points at.
    pub const fn offset(self) -> (i32, i32) {
        match self {
            Facing::North => (0, -1),
            Facing::South => (0, 1),
            Facing::West => (-1, 0),
            Facing::East => (1, 0),
        }
    }

    /// The direction pointing the other way.
    pub const fn opposite(self) -> Self {
        match self {
            Facing::North => Facing::South,
            Facing::South => Facing::North,
            Facing::West => Facing::East,
            Facing::East => Facing::West,
        }
    }

    /// The next direction clockwise when seen from above.
    pub const fn clockwise(self) -> Self {
        match self {
            Facing::North => Facing::East,
            Facing::East => Facing::South,
            Facing::South => Facing::West,
            Facing::West => Facing::North,
        }
    }

    /// The next direction counter-clockwise when seen from above.
    pub const fn counter_clockwise(self) -> Self {
        match self {
            Facing::North => Facing::West,
            Facing::West => Facing::South,
            Facing::South => Facing::East,
            Facing::East => Facing::North,
        }
    }

    /// The direction a player is looking at, from their yaw.
    ///
    /// This is vanilla's `Direction.fromYRot`: yaw 0 looks south, 90 west, 180 north and 270 east.
    pub fn from_player_yaw(yaw: f32) -> Self {
        let index = (yaw / 90.0 + 0.5).floor() as i32;
        match index.rem_euclid(4) {
            0 => Facing::South,
            1 => Facing::West,
            2 => Facing::North,
            _ => Facing::East,
        }
    }
}

/// Which half of a double chest a block is, or whether it stands alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChestKind {
    Single,
    Left,
    Right,
}

impl ChestKind {
    /// The value the `type` block state property uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            ChestKind::Single => "single",
            ChestKind::Left => "left",
            ChestKind::Right => "right",
        }
    }

    /// Parses a `type` block state property.
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "single" => Some(ChestKind::Single),
            "left" => Some(ChestKind::Left),
            "right" => Some(ChestKind::Right),
            _ => None,
        }
    }

    /// The kind of the other half, given this one.
    pub const fn opposite(self) -> Self {
        match self {
            ChestKind::Single => ChestKind::Single,
            ChestKind::Left => ChestKind::Right,
            ChestKind::Right => ChestKind::Left,
        }
    }
}

/// The parts of a chest's block state that matter to gameplay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChestState {
    pub facing: Facing,
    pub kind: ChestKind,
    pub waterlogged: bool,
}

impl ChestState {
    /// Reads the chest state out of a block state, or `None` when the block is not a chest.
    pub fn from_block(state: BlockStateId) -> Option<Self> {
        let data = state.to_block_data()?;
        if data.name != CHEST_BLOCK_NAME {
            return None;
        }
        let properties = data.properties.as_ref()?;
        let facing = properties
            .get("facing")
            .and_then(|value| Facing::from_str(value))?;
        let kind = properties
            .get("type")
            .and_then(|value| ChestKind::from_str(value))?;
        let waterlogged = properties
            .get("waterlogged")
            .is_some_and(|value| value == "true");
        Some(Self {
            facing,
            kind,
            waterlogged,
        })
    }

    /// Builds the chest block state for these properties.
    pub fn to_block_state(self) -> BlockStateId {
        let mut properties = BTreeMap::new();
        properties.insert("facing".to_string(), self.facing.as_str().to_string());
        properties.insert("type".to_string(), self.kind.as_str().to_string());
        properties.insert(
            "waterlogged".to_string(),
            if self.waterlogged { "true" } else { "false" }.to_string(),
        );
        BlockStateId::from_block_data(&BlockData {
            name: CHEST_BLOCK_NAME.to_string(),
            properties: Some(properties),
        })
    }
}

/// Whether a block state is any kind of chest.
pub fn is_chest(block: BlockStateId) -> bool {
    ChestState::from_block(block).is_some()
}

/// A position and dimension, used as the key of a block entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChestLocation {
    pub pos: BlockPos,
    pub dimension: Dimension,
}

impl ChestLocation {
    /// The location of a block in the given dimension.
    pub fn new(pos: BlockPos, dimension: Dimension) -> Self {
        Self { pos, dimension }
    }

    /// Moves the location by a horizontal block offset.
    fn offset(self, (x, z): (i32, i32)) -> Self {
        Self {
            pos: self.pos + (x, 0, z),
            dimension: self.dimension,
        }
    }

    /// Reads the stored contents through the world's storage.
    fn load(&self, world: &GlobalStateResource) -> Option<Inventory> {
        let (x, y, z) = (self.pos.pos.x, self.pos.pos.y, self.pos.pos.z);
        world
            .0
            .world
            .get_block_entity_data(self.dimension.as_str(), x, y, z)
            .ok()
            .flatten()
            .and_then(|bytes| bitcode::decode(&bytes).ok())
    }

    fn store(&self, world: &GlobalStateResource, inventory: &Inventory) {
        let (x, y, z) = (self.pos.pos.x, self.pos.pos.y, self.pos.pos.z);
        let payload = bitcode::encode(inventory);
        if let Err(err) =
            world
                .0
                .world
                .set_block_entity_data(self.dimension.as_str(), x, y, z, &payload)
        {
            warn!("Failed to save chest contents at {:?}: {err:?}", self.pos);
        }
    }

    fn clear(&self, world: &GlobalStateResource) {
        let (x, y, z) = (self.pos.pos.x, self.pos.pos.y, self.pos.pos.z);
        if let Err(err) = world
            .0
            .world
            .remove_block_entity_data(self.dimension.as_str(), x, y, z)
        {
            warn!("Failed to clear chest contents at {:?}: {err:?}", self.pos);
        }
    }
}

/// The one or two blocks a chest screen shows.
///
/// `first` is always the half at the lowest coordinates, whichever half was clicked, so the same
/// double chest always presents its slots in the same order. That matters because the screen is a
/// flat list: the upper half alone decides which block owns the first 27 slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChestPair {
    pub first: ChestLocation,
    pub second: Option<ChestLocation>,
    pub facing: Facing,
}

impl ChestPair {
    /// A lone chest.
    pub fn single(location: ChestLocation, facing: Facing) -> Self {
        Self {
            first: location,
            second: None,
            facing,
        }
    }

    /// Whether either half is at the given position.
    pub fn contains(&self, pos: BlockPos) -> bool {
        self.first.pos == pos || self.second.is_some_and(|second| second.pos == pos)
    }
}

/// Resolves the pair a chest belongs to by reading its `type` property and pairing block.
///
/// A `left` half pairs with the block clockwise of its facing, a `right` half with the block
/// counter-clockwise of it; vanilla's `getConnectedDirection` uses exactly that mapping. A half whose
/// partner is missing or no longer a chest is treated as a single chest, so a half-broken pair never
/// points at a block that is not there.
pub fn resolve_chest(world: &GlobalStateResource, location: ChestLocation) -> ChestPair {
    let Some(state) = chest_state(world, location) else {
        return ChestPair::single(location, Facing::North);
    };

    let partner_offset = match state.kind {
        ChestKind::Single => return ChestPair::single(location, state.facing),
        ChestKind::Left => state.facing.clockwise().offset(),
        ChestKind::Right => state.facing.counter_clockwise().offset(),
    };
    let partner_location = location.offset(partner_offset);

    match chest_state(world, partner_location) {
        Some(partner)
            if partner.facing == state.facing && partner.kind == state.kind.opposite() =>
        {
            let (first, second) = order_pair(location, partner_location);
            ChestPair {
                first,
                second: Some(second),
                facing: state.facing,
            }
        }
        _ => ChestPair::single(location, state.facing),
    }
}

/// Orders the two halves by coordinate so slot numbering is stable.
fn order_pair(a: ChestLocation, b: ChestLocation) -> (ChestLocation, ChestLocation) {
    if (a.pos.pos.x, a.pos.pos.z) <= (b.pos.pos.x, b.pos.pos.z) {
        (a, b)
    } else {
        (b, a)
    }
}

/// Reads the chest state at a position, or `None` when the block is not a chest.
pub fn chest_state(world: &GlobalStateResource, location: ChestLocation) -> Option<ChestState> {
    let state = world
        .0
        .world
        .get_block_and_fetch(location.pos, location.dimension.as_str())
        .ok()?;
    ChestState::from_block(state)
}

/// Writes a block state, logging instead of propagating the error.
fn write_state(world: &GlobalStateResource, location: ChestLocation, state: BlockStateId) {
    if let Err(err) =
        world
            .0
            .world
            .set_block_and_fetch(location.pos, location.dimension.as_str(), state)
    {
        warn!(
            "Failed to update the chest state at {:?}: {err:?}",
            location.pos
        );
    }
}

/// Places a chest, joining it to a matching neighbour when there is one.
///
/// Returns every block whose state changed, so the caller can broadcast it. Vanilla lets two single
/// chests of the same facing combine; the neighbour clockwise of the facing becomes the `right` half
/// and the placed block the `left` half, which is the naming [`resolve_chest`] relies on.
pub fn place_chest(
    world: &GlobalStateResource,
    dimension: Dimension,
    pos: BlockPos,
    facing: Facing,
) -> Vec<(BlockPos, BlockStateId)> {
    let location = ChestLocation::new(pos, dimension);
    let mut pair = None;

    for (side, own_kind) in [
        (facing.clockwise(), ChestKind::Left),
        (facing.counter_clockwise(), ChestKind::Right),
    ] {
        let neighbour_location = location.offset(side.offset());
        let Some(neighbour) = chest_state(world, neighbour_location) else {
            continue;
        };
        if neighbour.facing == facing && neighbour.kind == ChestKind::Single {
            pair = Some((neighbour_location, own_kind, neighbour));
            break;
        }
    }

    let own = ChestState {
        facing,
        kind: pair.map_or(ChestKind::Single, |(_, kind, _)| kind),
        waterlogged: false,
    };
    let own_state = own.to_block_state();
    write_state(world, location, own_state);

    let mut updates = vec![(pos, own_state)];

    if let Some((neighbour_location, own_kind, neighbour)) = pair {
        let partner_state = ChestState {
            facing,
            kind: own_kind.opposite(),
            waterlogged: neighbour.waterlogged,
        }
        .to_block_state();
        write_state(world, neighbour_location, partner_state);
        updates.push((neighbour_location.pos, partner_state));
    }

    debug!(
        "Placed a {:?} chest at {pos} facing {:?}",
        own.kind, own.facing
    );
    updates
}

/// The contents of every chest the server knows about.
///
/// Chests are read from the database the first time someone looks at them and stay cached for the
/// rest of the server's lifetime, which keeps the inventory mutable in memory and avoids hitting the
/// database on every click.
#[derive(Resource, Default)]
pub struct ChestStore {
    contents: Mutex<HashMap<ChestLocation, Inventory>>,
}

impl ChestStore {
    /// The contents of one half, loading them from the world if this is the first look.
    pub fn get_or_load(&self, world: &GlobalStateResource, location: ChestLocation) -> Inventory {
        let mut cache = self.contents.lock().expect("chest store poisoned");
        if let Some(contents) = cache.get(&location) {
            return contents.clone();
        }

        let inventory = location
            .load(world)
            .unwrap_or_else(|| Inventory::new(CHEST_SLOT_COUNT));
        cache.insert(location, inventory.clone());
        inventory
    }

    /// Replaces the contents of one half and persists them.
    pub fn set(&self, world: &GlobalStateResource, location: ChestLocation, inventory: Inventory) {
        location.store(world, &inventory);
        self.contents
            .lock()
            .expect("chest store poisoned")
            .insert(location, inventory);
    }

    /// Forgets one half and deletes its contents, returning what it held.
    pub fn remove(
        &self,
        world: &GlobalStateResource,
        location: ChestLocation,
    ) -> Vec<InventorySlot> {
        let cached = self
            .contents
            .lock()
            .expect("chest store poisoned")
            .remove(&location);

        let inventory = cached.unwrap_or_else(|| location.load(world).unwrap_or_default());
        location.clear(world);
        inventory.slots.iter().flatten().cloned().collect()
    }
}

/// The whole screen contents of a pair, halves concatenated in [`ChestPair`] order.
pub fn container_contents(
    store: &ChestStore,
    world: &GlobalStateResource,
    pair: &ChestPair,
) -> Inventory {
    let first = store.get_or_load(world, pair.first);
    let Some(second_location) = pair.second else {
        return first;
    };
    let second = store.get_or_load(world, second_location);

    let mut slots = Vec::with_capacity(first.slots.len() + second.slots.len());
    slots.extend(first.slots.iter().cloned());
    slots.extend(second.slots.iter().cloned());
    Inventory {
        slots: slots.into_boxed_slice(),
    }
}

/// Splits a screen inventory back into the halves of a pair and persists both.
pub fn store_contents(
    store: &ChestStore,
    world: &GlobalStateResource,
    pair: &ChestPair,
    inventory: &Inventory,
) {
    let Some(second_location) = pair.second else {
        store.set(world, pair.first, inventory.clone());
        return;
    };

    let mut first = Inventory::new(CHEST_SLOT_COUNT);
    let mut second = Inventory::new(CHEST_SLOT_COUNT);
    for (index, slot) in inventory.slots.iter().enumerate() {
        if index < CHEST_SLOT_COUNT {
            first.slots[index] = slot.clone();
        } else if index < DOUBLE_CHEST_SLOT_COUNT {
            second.slots[index - CHEST_SLOT_COUNT] = slot.clone();
        }
    }
    store.set(world, pair.first, first);
    store.set(world, second_location, second);
}

/// The title a chest screen shows, which vanilla names after the chest itself.
pub fn chest_title() -> TextComponent {
    TextComponentBuilder::new("Chest".to_string()).build()
}

/// Builds the full slot list a chest window expects: the container slots followed by the player's
/// main inventory and hotbar.
///
/// The client re-initialises every menu slot from this list, so it has to be complete. Sending only
/// the container slots would blank the player's half of the screen.
fn window_contents(chest: &Inventory, player: &Inventory) -> Vec<InventorySlot> {
    let mut slots: Vec<InventorySlot> = Vec::with_capacity(chest.slots.len() + CHEST_PLAYER_SLOTS);
    slots.extend(
        chest
            .slots
            .iter()
            .map(|slot| slot.clone().unwrap_or_default()),
    );
    // The player's own numbering keeps the 2x2 crafting grid and armour first; a chest screen only
    // shows the 27 main slots and the 9 hotbar slots.
    slots.extend(
        player.slots[PLAYER_MAIN_FIRST..=PLAYER_MAIN_LAST]
            .iter()
            .map(|slot| slot.clone().unwrap_or_default()),
    );
    slots.extend(
        player.slots[PLAYER_HOTBAR_FIRST..=PLAYER_HOTBAR_LAST]
            .iter()
            .map(|slot| slot.clone().unwrap_or_default()),
    );
    slots
}

/// A player opening a chest.
pub struct ChestViewer<'a> {
    pub entity: Entity,
    pub conn: &'a StreamWriter,
    /// The viewer's own inventory, which the chest screen also shows.
    pub inventory: &'a Inventory,
    /// The screen the player already had open, if any.
    pub open: Option<OpenChest>,
}

/// The chest a player currently has open.
///
/// While this is present, container clicks from that player are routed at the chest instead of their
/// own inventory.
#[derive(Component, Debug, Clone, Copy)]
pub struct OpenChest {
    /// The window id the screen was opened in.
    pub window_id: i32,
    /// Both halves of the chest being viewed.
    pub pair: ChestPair,
}

impl ChestViewer<'_> {
    /// Opens a chest screen, replacing whatever the player had open.
    pub fn open(
        self,
        commands: &mut Commands,
        location: ChestLocation,
        store: &ChestStore,
        world: &GlobalStateResource,
        listeners: &[(&StreamWriter, &Position)],
    ) {
        if let Some(previous) = self.open {
            // The client keeps one screen per window id, so the old one is dismissed before the new
            // one takes its place, exactly as if the player had pressed escape.
            if let Err(err) = self
                .conn
                .send_packet(ContainerClose::new(previous.window_id))
            {
                warn!("Failed to close the previous chest screen: {err:?}");
            }
            broadcast_lid(listeners, &previous.pair, false);
        }

        let pair = resolve_chest(world, location);
        let inventory = container_contents(store, world, &pair);
        let slots = window_contents(&inventory, self.inventory);
        let double = pair.second.is_some();

        if let Err(err) =
            self.conn
                .send_packet(OpenScreen::chest(CHEST_WINDOW_ID, double, chest_title()))
        {
            warn!("Failed to open the chest screen: {err:?}");
            return;
        }

        if let Err(err) = self
            .conn
            .send_packet(SetContainerContent::new(CHEST_WINDOW_ID, 0, slots))
        {
            warn!("Failed to send the chest contents: {err:?}");
        }

        commands.entity(self.entity).insert(OpenChest {
            window_id: CHEST_WINDOW_ID,
            pair,
        });
        broadcast_lid(listeners, &pair, true);
    }
}

/// Closes the chest screen a player has open and lowers the lid.
pub fn close_chest(
    commands: &mut Commands,
    entity: Entity,
    conn: &StreamWriter,
    open: OpenChest,
    listeners: &[(&StreamWriter, &Position)],
) {
    if let Err(err) = conn.send_packet(ContainerClose::new(open.window_id)) {
        warn!("Failed to close the chest screen: {err:?}");
    }
    commands.entity(entity).remove::<OpenChest>();
    broadcast_lid(listeners, &open.pair, false);
}

/// Tells every nearby player to play the lid animation for both halves of a chest.
pub fn broadcast_lid(listeners: &[(&StreamWriter, &Position)], pair: &ChestPair, open: bool) {
    let Some(block_type) = ferrumc_registry::lookup_block_protocol_id(CHEST_BLOCK_NAME) else {
        warn!("The block registry has no entry for {CHEST_BLOCK_NAME}");
        return;
    };

    let positions: Vec<BlockPos> = std::iter::once(pair.first.pos)
        .chain(pair.second.map(|location| location.pos))
        .collect();

    for position in positions {
        let packet = BlockEvent::chest(NetworkPosition::from(position), open, block_type);
        for (conn, player_position) in listeners {
            if !is_near(player_position, position) {
                continue;
            }
            if let Err(err) = conn.send_packet_ref(&packet) {
                warn!("Failed to send a chest lid event: {err:?}");
            }
        }
    }
}

/// Whether a player is close enough to see a block event at a position.
fn is_near(player: &Position, position: BlockPos) -> bool {
    let dx = player.coords.x - (f64::from(position.pos.x) + 0.5);
    let dy = player.coords.y - (f64::from(position.pos.y) + 0.5);
    let dz = player.coords.z - (f64::from(position.pos.z) + 0.5);
    dx * dx + dy * dy + dz * dz <= LID_EVENT_RANGE * LID_EVENT_RANGE
}

/// Empties a chest when its block is broken.
///
/// Both halves are emptied and dropped at the block that was mined, and the surviving half is turned
/// back into a single chest. Anyone showing the screen has it closed, because the container its
/// window points at no longer exists.
pub fn drop_chest_contents_on_break(
    mut commands: Commands,
    mut events: bevy_ecs::prelude::MessageReader<ferrumc_messages::BlockBrokenEvent>,
    openers: Query<(Entity, &OpenChest)>,
    conns: Query<(Entity, &StreamWriter)>,
    chest_store: Res<ChestStore>,
    world: Res<GlobalStateResource>,
) {
    for event in events.read() {
        let Some(chest) = ChestState::from_block(event.state) else {
            continue;
        };

        let dimension = Dimension::default();
        let broken = ChestLocation::new(event.position, dimension);
        let pair = pair_from_state(broken, chest);

        let mut contents = chest_store.remove(&world, pair.first);
        if let Some(second) = pair.second {
            contents.extend(chest_store.remove(&world, second));
        }

        // Close the screen for anyone who was looking at this chest.
        for (entity, open) in openers.iter() {
            if !open.pair.contains(event.position) {
                continue;
            }
            let Ok((_, conn)) = conns.get(entity) else {
                continue;
            };
            if let Err(err) = conn.send_packet(ContainerClose::new(open.window_id)) {
                warn!("Failed to close a chest screen: {err:?}");
            }
            commands.entity(entity).remove::<OpenChest>();
        }

        if let Some(survivor) = surviving_half(&pair, event.position) {
            if let Some(survivor_state) = chest_state(&world, survivor) {
                let single = ChestState {
                    kind: ChestKind::Single,
                    ..survivor_state
                }
                .to_block_state();
                write_state(&world, survivor, single);

                // The survivor was rendered as half of a pair; tell every client it is a full
                // chest again, or the broken half stays on screen.
                let packet = BlockUpdate {
                    location: NetworkPosition::from(survivor.pos),
                    block_state_id: VarInt::from(single),
                };
                for (_, conn) in conns.iter() {
                    if let Err(err) = conn.send_packet_ref(&packet) {
                        warn!("Failed to broadcast the surviving chest: {err:?}");
                    }
                }
            }
        }

        // Each stack is dropped on the ground at the block that was broken.
        for stack in contents {
            drop_stack_at(stack, event.position, &mut commands, &conns);
        }
    }
}

/// The pair implied by a chest state before its block is removed.
fn pair_from_state(location: ChestLocation, state: ChestState) -> ChestPair {
    let partner_offset = match state.kind {
        ChestKind::Single => return ChestPair::single(location, state.facing),
        ChestKind::Left => state.facing.clockwise().offset(),
        ChestKind::Right => state.facing.counter_clockwise().offset(),
    };
    let partner = location.offset(partner_offset);
    let (first, second) = order_pair(location, partner);
    ChestPair {
        first,
        second: Some(second),
        facing: state.facing,
    }
}

/// The half of a broken pair that is still standing, if any.
fn surviving_half(pair: &ChestPair, broken: BlockPos) -> Option<ChestLocation> {
    let second = pair.second?;
    if pair.first.pos == broken {
        Some(second)
    } else {
        Some(pair.first)
    }
}

/// Spawns an item entity for one stack at a block position.
fn drop_stack_at(
    stack: InventorySlot,
    position: BlockPos,
    commands: &mut Commands,
    conns: &Query<(Entity, &StreamWriter)>,
) {
    let drop_position = Position::new(
        f64::from(position.pos.x) + 0.5,
        f64::from(position.pos.y) + 0.25,
        f64::from(position.pos.z) + 0.5,
    );
    let drop_velocity = ferrumc_core::transform::velocity::Velocity::new(
        (rand::random::<f64>() - 0.5) * 0.15,
        0.2,
        (rand::random::<f64>() - 0.5) * 0.15,
    );

    crate::systems::item_entities::drop_item(
        commands,
        &drop_position,
        &drop_velocity,
        stack,
        Dimension::default(),
        // Loot popped out of a container: vanilla `setDefaultPickUpDelay()` = 10 ticks.
        ferrumc_components::item_entity::ItemEntity::PICKUP_DELAY,
        conns,
    );
}

/// Player inventory index of the first main-inventory slot.
const PLAYER_MAIN_FIRST: usize = 9;
/// Player inventory index of the last main-inventory slot.
const PLAYER_MAIN_LAST: usize = 35;
/// Player inventory index of the first hotbar slot.
const PLAYER_HOTBAR_FIRST: usize = 36;
/// Player inventory index of the last hotbar slot.
const PLAYER_HOTBAR_LAST: usize = 44;
/// Number of player slots a chest window shows: the main inventory plus the hotbar.
const CHEST_PLAYER_SLOTS: usize =
    (PLAYER_MAIN_LAST - PLAYER_MAIN_FIRST + 1) + (PLAYER_HOTBAR_LAST - PLAYER_HOTBAR_FIRST + 1);

/// The slot indices a chest window uses.
///
/// A generic chest screen lists the container's slots first, then the player's main inventory and
/// hotbar. The player's slots are numbered after the container's, so they have to be shifted to reach
/// the player's own 46-slot inventory. A chest screen has no armour or offhand slots.
pub mod chest_slots {
    use super::{PLAYER_HOTBAR_FIRST, PLAYER_HOTBAR_LAST, PLAYER_MAIN_FIRST, PLAYER_MAIN_LAST};

    /// Number of slots in the player's main inventory.
    const PLAYER_MAIN_SLOTS: i32 = (PLAYER_MAIN_LAST - PLAYER_MAIN_FIRST + 1) as i32;
    /// Number of slots in the player's hotbar.
    const PLAYER_HOTBAR_SLOTS: i32 = (PLAYER_HOTBAR_LAST - PLAYER_HOTBAR_FIRST + 1) as i32;

    /// Maps a chest window slot index to the player inventory slot it refers to.
    ///
    /// `container_slots` is the number of slots the chest itself occupies (27 for a single chest,
    /// 54 for a double one). Returns `None` for the chest's own slots and for indices outside the
    /// window.
    pub fn to_player_slot(slot: i32, container_slots: i32) -> Option<u8> {
        let main_last = container_slots + PLAYER_MAIN_SLOTS - 1;
        let hotbar_first = container_slots + PLAYER_MAIN_SLOTS;
        let hotbar_last = hotbar_first + PLAYER_HOTBAR_SLOTS - 1;

        if (container_slots..=main_last).contains(&slot) {
            Some((PLAYER_MAIN_FIRST as i32 + slot - container_slots) as u8)
        } else if (hotbar_first..=hotbar_last).contains(&slot) {
            Some((PLAYER_HOTBAR_FIRST as i32 + slot - hotbar_first) as u8)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_zero_looks_south() {
        assert_eq!(Facing::from_player_yaw(0.0), Facing::South);
        assert_eq!(Facing::from_player_yaw(90.0), Facing::West);
        assert_eq!(Facing::from_player_yaw(180.0), Facing::North);
        assert_eq!(Facing::from_player_yaw(-90.0), Facing::East);
    }

    #[test]
    fn clockwise_rotates_through_all_four_directions() {
        let mut facing = Facing::North;
        for _ in 0..4 {
            facing = facing.clockwise();
        }
        assert_eq!(facing, Facing::North);
        assert_eq!(Facing::North.clockwise(), Facing::East);
        assert_eq!(Facing::East.clockwise(), Facing::South);
    }

    #[test]
    fn chest_states_round_trip() {
        for facing in [Facing::North, Facing::South, Facing::West, Facing::East] {
            for kind in [ChestKind::Single, ChestKind::Left, ChestKind::Right] {
                let state = ChestState {
                    facing,
                    kind,
                    waterlogged: false,
                };
                assert_eq!(ChestState::from_block(state.to_block_state()), Some(state));
            }
        }
    }

    #[test]
    fn the_chests_own_slots_are_not_player_slots() {
        for slot in 0..27 {
            assert_eq!(chest_slots::to_player_slot(slot, 27), None);
            assert_eq!(chest_slots::to_player_slot(slot, 54), None);
        }
    }

    #[test]
    fn a_single_chest_window_maps_the_player_inventory() {
        assert_eq!(chest_slots::to_player_slot(27, 27), Some(9));
        assert_eq!(chest_slots::to_player_slot(53, 27), Some(35));
        assert_eq!(chest_slots::to_player_slot(54, 27), Some(36));
        assert_eq!(chest_slots::to_player_slot(62, 27), Some(44));
        assert_eq!(chest_slots::to_player_slot(63, 27), None);
    }

    #[test]
    fn a_double_chest_window_maps_the_player_inventory() {
        assert_eq!(chest_slots::to_player_slot(54, 54), Some(9));
        assert_eq!(chest_slots::to_player_slot(80, 54), Some(35));
        assert_eq!(chest_slots::to_player_slot(81, 54), Some(36));
        assert_eq!(chest_slots::to_player_slot(89, 54), Some(44));
        assert_eq!(chest_slots::to_player_slot(90, 54), None);
    }

    #[test]
    fn a_single_chest_window_lists_every_menu_slot() {
        let chest = Inventory::new(CHEST_SLOT_COUNT);
        let player = Inventory::new(46);

        let slots = window_contents(&chest, &player);

        assert_eq!(slots.len(), CHEST_SLOT_COUNT + CHEST_PLAYER_SLOTS);
        assert_eq!(slots.len(), 63);
    }

    #[test]
    fn a_double_chest_window_lists_every_menu_slot() {
        let chest = Inventory::new(DOUBLE_CHEST_SLOT_COUNT);
        let player = Inventory::new(46);

        let slots = window_contents(&chest, &player);

        assert_eq!(slots.len(), DOUBLE_CHEST_SLOT_COUNT + CHEST_PLAYER_SLOTS);
        assert_eq!(slots.len(), 90);
    }

    #[test]
    fn the_player_inventory_follows_the_container_slots() {
        let mut player = Inventory::new(46);
        let main_stack = InventorySlot {
            count: VarInt(2),
            item_id: Some(ferrumc_inventories::item::ItemID(VarInt(1))),
            ..Default::default()
        };
        let hotbar_stack = InventorySlot {
            count: VarInt(5),
            item_id: Some(ferrumc_inventories::item::ItemID(VarInt(2))),
            ..Default::default()
        };
        player
            .set_item(PLAYER_MAIN_FIRST, main_stack.clone())
            .expect("player inventory is large enough");
        player
            .set_item(PLAYER_HOTBAR_LAST, hotbar_stack.clone())
            .expect("player inventory is large enough");

        let slots = window_contents(&Inventory::new(CHEST_SLOT_COUNT), &player);

        assert_eq!(slots[CHEST_SLOT_COUNT], main_stack);
        assert_eq!(
            slots[CHEST_SLOT_COUNT + CHEST_PLAYER_SLOTS - 1],
            hotbar_stack
        );
    }
}
