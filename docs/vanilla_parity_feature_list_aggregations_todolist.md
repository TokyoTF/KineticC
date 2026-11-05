# 🛠️ Vanilla 1:1 Parity Feature Implementation Checklist for FerrumC

> **Legend:**
> - [x] = Done
> - [/] = Partial
> - [ ] = Not Done


## I. Core Infrastructure & Lifecycle

### **Bootstrap**
- [x] **Command Line Argument Parsing** — Uses `clap` crate in [`src/bin/src/cli.rs`](../src/bin/src/cli.rs). Supports `--log` (trace/debug/info/warn/error), subcommands: `setup`, `import --import-path`, `run`. **Missing:** `--nogui`, `--port`, `--world` flags (port/world configured via TOML instead).
- [ ] **EULA Acceptance/Enforcement** — Not implemented.
- [x] **Logging System** — Uses `tracing` + `tracing-subscriber` in [`src/lib/utils/logging/src/lib.rs`](../src/lib/utils/logging/src/lib.rs). Rolling file appenders via `tracing-appender` (daily rotation), console colors via ANSI. **DEVIATION:** Uses Rust `tracing` ecosystem instead of Log4j.
- [x] **Native Library Loading** — **DEVIATION:** Pure Rust implementations. Compression uses `yazi`/`libdeflater` crates, encryption uses `aes`/`cfb8`/`rsa` crates. No native JNI loading needed.

### **The Main Loop (The Tick)**
- [x] **The Tick Loop** — Implemented in [`src/bin/src/game_loop.rs`](../src/bin/src/game_loop.rs). Configurable TPS via `config.toml`, period calculated as `Duration::from_secs(1) / tps`. Default 20 TPS = 50ms.
- [x] **Catch-up logic** — Implemented via `MissedTickBehavior::Burst` in [`src/lib/scheduler/src/lib.rs`](../src/lib/scheduler/src/lib.rs). Supports Burst (run missed ticks up to `max_catch_up`), Skip, and Delay behaviors. Global catch-up cap of 64 schedules per iteration.
- [/] **Tick splitting** — Has separate `TimedSchedule`s for tick, world_sync (15s), keepalive (1s) with phase offsets. **Missing:** Explicit World/Connection/Task sub-tick separation like vanilla.
- [ ] **Watchdog** — Not implemented. No crash detection for hung ticks.

### **Threading Model**
- [x] **Main Thread (Logic)** — Game loop runs on main thread, Bevy ECS schedules execute `SingleThreaded`.
- [x] **Netty IO Threads (Networking)** — **DEVIATION:** Uses Tokio async TCP instead of Netty NIO. TCP acceptor spawns on dedicated thread with its own Tokio runtime in [`src/bin/src/game_loop.rs:tcp_conn_acceptor()`](../src/bin/src/game_loop.rs). Connection handlers in [`src/lib/net/src/connection.rs`](../src/lib/net/src/connection.rs).
- [x] **Chunk Loading/Gen Threads (Async)** — Chunk loading uses thread pool batches in [`src/bin/src/systems/chunk_sending.rs`](../src/bin/src/systems/chunk_sending.rs). `ferrumc_state.thread_pool.batch()` for parallel chunk generation.
- [ ] **Light Calculation Threads** — Not implemented. Light data imported from vanilla chunks but not recalculated.

### **Scheduler**
- [x] **Sync Tasks (Tick-aligned)** — `TimedSchedule` with configurable periods in [`src/lib/scheduler/src/lib.rs`](../src/lib/scheduler/src/lib.rs). Priority-based heap scheduling.
- [x] **Async Tasks** — Tokio `spawn` used throughout codebase for async operations.
- [x] **Delayed Tasks** — Supported via `TimedSchedule` with `with_phase()` for initial delay.
- [x] **Repeating Tasks** — `TimedSchedule` runs at fixed intervals with configurable `period`.

---

## II. Networking & Protocol (The Stack)

### **Transport Layer**
- [x] **TCP Listener** — [`src/lib/net/src/server.rs`](../src/lib/net/src/server.rs) binds to configurable host:port from `config.toml`. Default port 25565.
- [/] **UDP Listener (Query Protocol)** — LAN broadcast pinger implemented in [`src/bin/src/systems/lan_pinger.rs`](../src/bin/src/systems/lan_pinger.rs) (224.0.2.60:4445). **Missing:** Full Query protocol (GameSpy4 protocol).
- [x] **Netty Pipeline Configuration** — **DEVIATION:** Custom packet pipeline. Derive macros `#[derive(NetEncode, NetDecode)]` in [`src/lib/net/crates/codec/`](../src/lib/net/crates/codec/). Packet routing via `#[packet(packet_id = "...", state = "...")]` attributes.

### **Packet Handling**
- [x] **Framing: VarInt Length Prefixing** — [`src/lib/net/crates/codec/src/net_types/var_int.rs`](../src/lib/net/crates/codec/src/net_types/var_int.rs). Full VarInt read/write with async support.
- [x] **Compression: zlib** — [`src/lib/net/src/compression.rs`](../src/lib/net/src/compression.rs) using `yazi` crate (Zlib format). Configurable threshold via `network_compression_threshold` in config.
- [x] **Encryption: AES/CFB8 + RSA** — [`src/lib/net/crates/encryption/src/lib.rs`](../src/lib/net/crates/encryption/src/lib.rs). RSA 1024-bit key generation, AES-128-CFB8 stream cipher, DER public key encoding, Minecraft hex digest for session auth.
- [x] **Data Types** — All in [`src/lib/net/crates/codec/src/net_types/`](../src/lib/net/crates/codec/src/net_types/):
  - `var_int.rs` — VarInt (and VarLong support)
  - `network_position.rs` — Position (26-bit X/Z, 12-bit Y packed into i64)
  - `bitset.rs` — BitSet for chunk data
  - `length_prefixed_vec.rs` — Length-prefixed arrays
  - NBT via `ferrumc-nbt` crate (tape-based parser with derive macros)
  - JSON Text Components in [`src/lib/text/src/lib.rs`](../src/lib/text/src/lib.rs)

### **Protocol Stages**
- [x] **Handshaking** — [`src/lib/net/src/packets/incoming/handshake.rs`](../src/lib/net/src/packets/incoming/handshake.rs). Reads protocol version, server address, port, next state (1=Status, 2=Login).
- [x] **Status (SLP)** — [`src/lib/net/src/packets/incoming/status_request.rs`](../src/lib/net/src/packets/incoming/status_request.rs), [`ping_request.rs`](../src/lib/net/src/packets/incoming/ping.rs). Full JSON response with MOTD, player count, max players, favicon (base64), version info.
- [x] **Login** — [`src/lib/net/src/packets/incoming/login_start.rs`](../src/lib/net/src/packets/incoming/login_start.rs), [`encryption_response.rs`](../src/lib/net/src/packets/incoming/encryption_response.rs). Mojang session auth via `sessionserver.mojang.com/session/minecraft/hasJoined` in [`src/lib/net/src/auth.rs`](../src/lib/net/src/auth.rs). Compression packet, Login Success with UUID/username/properties.
- [/] **Play (In-Game)** — Partial implementation. **Implemented:** Chunk sending, keep-alive, player position/rotation sync, block break/place (creative), entity spawning, chat messages, command handling, sneaking state sync, arm swing animation, pick block (creative/survival), render distance syncing. **Missing:** Most gameplay packets (combat, enchanting, villagers, etc.). See [`src/lib/net/src/packets/outgoing/`](../src/lib/net/src/packets/outgoing/) for ~45 outgoing packets.
- [ ] **RCON** — Not implemented.

---

## III. World Management (The Environment)

### **File I/O & Storage**
- [/] **Level.dat** — Seed reading from `level.dat` NBT. **Missing:** World settings, time, spawn point persistence.
- [x] **Region Files (.mca)** — Full Anvil format import in [`src/lib/world/src/importing.rs`](../src/lib/world/src/importing.rs) and [`vanilla_chunk_format.rs`](../src/lib/world/src/vanilla_chunk_format.rs). Supports Zlib, GZip compression via [`src/lib/storage/src/compressors/`](../src/lib/storage/src/compressors/). **DEVIATION:** Uses LMDB (heed) for runtime storage instead of region files. See [`src/lib/storage/src/lmdb.rs`](../src/lib/storage/src/lmdb.rs).
- [ ] **Player Data (.dat)** — Not implemented. No per-player NBT persistence.

### **Chunk System**
- [x] **Chunk Structure** — [`src/lib/world/src/chunk_format.rs`](../src/lib/world/src/chunk_format.rs). 16x384x16 columns (Y range -64 to 320 for 1.18+).
- [x] **Sections** — 16x16x16 `Section` structs with y-index, block states, biome states, block/sky light.
- [x] **Paletted Containers** — [`src/lib/world/src/chunk_format.rs`](../src/lib/world/src/chunk_format.rs). `PaletteType` enum: Single (one block type), Indirect (palette + packed data), Direct (global IDs).
- [x] **Bit Storage** — [`src/lib/utils/general_purpose/src/data_packing.rs`](../src/lib/utils/general_purpose/src/data_packing.rs). Packing block IDs into i64 arrays with variable bits-per-block.
- [x] **Heightmaps** — `MOTION_BLOCKING`, `WORLD_SURFACE` calculated and serialized as NBT in [`chunk_format.rs`](../src/lib/world/src/chunk_format.rs).
- [ ] **Chunk State** — No proto-chunk vs level chunk distinction. All chunks treated as fully loaded.

### **World Generation (The Pipeline)**
- [/] **Noise Generators** — OpenSimplex noise in [`src/lib/world_gen/src/lib.rs`](../src/lib/world_gen/src/lib.rs). Multi-layer noise with configurable scales. **Missing:** Full Perlin, octave noise.
- [/] **Biome Source** — Basic biome generator trait in [`src/lib/world_gen/src/biomes/`](../src/lib/world_gen/src/biomes/). Plains biome implemented. **Missing:** Multi-noise biome selection (Temperature, Humidity, etc.).
- [/] **Surface Builders** — Plains biome generates grass/dirt surface. **Missing:** Other biome surfaces.
- [ ] **Carvers** — Not implemented. No caves/ravines.
- [ ] **Features** — Not implemented. No trees/ores/lakes.
- [ ] **Structures** — Not implemented. No jigsaw system.

### **Lighting Engine**
- [/] **Sky Light** — Imported from vanilla chunks, stored per-section. **Missing:** Runtime propagation.
- [/] **Block Light** — Imported from vanilla chunks, stored per-section. **Missing:** Runtime propagation.
- [ ] **Light Updates** — Not implemented. No light recalculation system.

### **Block Logic**
- [x] **Block Registry** — [`src/lib/registry/src/lib.rs`](../src/lib/registry/src/lib.rs). Compile-time `phf::Map` for block name ↔ protocol ID lookups. Generated from vanilla data at build time.
- [x] **Block States** — [`src/lib/world/src/block_state_id.rs`](../src/lib/world/src/block_state_id.rs). State IDs map to property combinations. **Note:** Properties not individually accessible at runtime (stored as flat IDs).
- [/] **Voxel Shapes** — Basic collision bounds in [`src/lib/core/src/collisions/`](../src/lib/core/src/collisions/). **Missing:** Per-block collision shapes.
- [/] **Material Properties** — Block hardness lookup via `lookup_block_hardness()` in registry. **Missing:** Blast resistance, flammability, etc.
- [ ] **Tick Logic** — Not implemented. No random/scheduled block ticks.

---

## IV. Entity System (The Actors)

### **Base Entity Architecture**
- [x] **Entity IDs & UUIDs** — Bevy ECS entities with numeric IDs. UUIDs stored as `PlayerIdentity` component in [`src/lib/core/src/identity/`](../src/lib/core/src/identity/).
- [x] **Position & Rotation** — [`src/lib/core/src/transform/position.rs`](../src/lib/core/src/transform/position.rs), [`rotation.rs`](../src/lib/core/src/transform/rotation.rs). `Position` wraps `DVec3`, `Rotation` has yaw/pitch with normalization.
- [/] **Velocity** — Not as dedicated component. Movement handled via position deltas.
- [x] **Hitboxes** — `BoundingBox` and `PhysicalProperties` in [`src/lib/entities/src/components/physical.rs`](../src/lib/entities/src/components/physical.rs). AABB from vanilla entity data.
- [/] **DataTracker (Metadata)** — `EntityMetadata` in [`src/lib/entities/src/components/metadata.rs`](../src/lib/entities/src/components/metadata.rs). **Missing:** Full synced data key-value system.

### **Physics & Movement**
- [ ] **Gravity simulation** — Not implemented.
- [ ] **Drag/Air Resistance** — Not implemented.
- [ ] **Fluid handling** — Not implemented.
- [ ] **Piston pushing logic** — Not implemented.
- [/] **Collision resolution** — Basic collision bounds checking in [`src/lib/core/src/collisions/`](../src/lib/core/src/collisions/). **Missing:** Full collision response.

### **Player Entity**
- [x] **Gamemodes** — [`src/lib/components/src/player/gamemode.rs`](../src/lib/components/src/player/gamemode.rs). All 4 modes: Survival, Creative, Adventure, Spectator. Configurable default in config.
- [x] **Abilities** — [`src/lib/components/src/player/abilities.rs`](../src/lib/components/src/player/abilities.rs). Flying, invulnerable, may_fly, creative_mode, may_build, flying_speed, walking_speed. Mode-specific ability presets.
- [x] **Skins (online mode)** — The `textures` property Mojang returns at login is forwarded verbatim, together with its signature, in Login Success and Player Info Update, so premium players keep their skin, cape and signature. The identity that the server announces is the one Mojang verified. See [`docs/skins.md`](skins.md). **Missing:** premium skin lookup for offline-mode names.
- [/] **Food/Exhaustion/Saturation** — `Hunger` component in [`src/lib/components/src/player/hunger.rs`](../src/lib/components/src/player/hunger.rs) with level/saturation/exhaustion. **Missing:** Exhaustion tick logic.
- [/] **XP/Leveling** — `Experience` component in [`src/lib/components/src/player/experience.rs`](../src/lib/components/src/player/experience.rs) with progress/level/total_xp. **Missing:** Level calculation formulas.
- [ ] **Statistics tracking** — Not implemented.
- [ ] **Advancements** — Not implemented.

### **AI (Mob Brains)**
- [ ] **Pathfinding** — Not implemented.
- [ ] **Goal Selector** — Not implemented.
- [ ] **Sensing** — Not implemented.

### **Entity Categories**
- [/] **Living** — `Health` component in [`src/lib/components/src/health.rs`](../src/lib/components/src/health.rs). **Missing:** Potions, armor, hand items.
- [/] **Mobs** — Partial implementation. **Implemented:** Pig entity (`PigBundle`) with full component set (physics, combat, metadata). **Missing:** AI, other mobs.
- [ ] **Projectiles** — Not implemented.
- [ ] **Vehicles** — Not implemented.
- [/] **Items** — Dropped item entities exist: blocks drop an item when broken ([`src/bin/src/systems/item_entities.rs`](../src/bin/src/systems/item_entities.rs)), the drop key drops the item the player has selected ([`src/bin/src/packet_handlers/play_packets/player_action.rs`](../src/bin/src/packet_handlers/play_packets/player_action.rs)), the entity falls, rests on the surface and is collected by a nearby player into their inventory. The placement handler spends the item it placed ([`src/bin/src/packet_handlers/play_packets/place_block.rs`](../src/bin/src/packet_handlers/play_packets/place_block.rs)), and the server pushes the player inventory on join so the client and the server agree on the held item ([`src/bin/src/packet_handlers/play_packets/player_loaded.rs`](../src/bin/src/packet_handlers/play_packets/player_loaded.rs)). **Missing:** stack merging, item despawning after five minutes, item entities dropped by mobs or on death.
- [ ] **Displays** — Not implemented.

---

## V. Inventory & GUI System

### **Item Stack**
- [x] **Item Type, Count** — [`src/lib/inventories/src/item.rs`](../src/lib/inventories/src/item.rs). `ItemID` with `VarInt` protocol ID, name lookups via registry.
- [/] **Components/NBT** — Slot data supports NBT components. **Missing:** Full component system (enchantments, lore, etc.).

### **Container Logic**
- [x] **Slots** — [`src/lib/inventories/src/slot.rs`](../src/lib/inventories/src/slot.rs), [`defined_slots.rs`](../src/lib/inventories/src/defined_slots.rs). Slot indexing with item optional.
- [/] **Window Types** — The player inventory (window 0) and chests are implemented: single chests use `generic_9x3` and double chests `generic_9x6`, see [`docs/containers.md`](containers.md). **Missing:** furnace, anvil, hopper, barrel and other container types.
- [x] **Chests** — [`src/bin/src/systems/chests.rs`](../src/bin/src/systems/chests.rs). Per-block inventories persisted as block entities, double-chest pairing through the `type` block state property, container clicks routed between chest and player, contents dropped on break, lid animation through `block_event`. See [`docs/containers.md`](containers.md). **Missing:** waterlogging, the vanilla "no block above" pairing rule.
- [/] **Synchronization** — Container content/slot packets in [`src/lib/net/src/packets/outgoing/`](../src/lib/net/src/packets/outgoing/). Click handling in [`src/bin/src/packet_handlers/play_packets/`](../src/bin/src/packet_handlers/play_packets/). The server trusts the click diff the client reports instead of re-implementing every click mode.

### **Recipe System**
- [ ] **Recipe Registry** — Not implemented.
- [ ] **Recipe Types** — Not implemented.
- [ ] **Recipe Book** — Not implemented.

---

## VI. Gameplay Mechanics (The Logic)

### **Interaction Processing**
- [x] **Player Digging** — [`src/bin/src/packet_handlers/play_packets/player_action.rs`](../src/bin/src/packet_handlers/play_packets/player_action.rs) and [`src/bin/src/systems/listeners/digging_system.rs`](../src/bin/src/systems/listeners/digging_system.rs). Break timing uses the vanilla damage-per-tick formula from [`src/bin/src/systems/mining.rs`](../src/bin/src/systems/mining.rs): `tool_speed / hardness / (30 or 100)`, with the tool speed read from the item's `minecraft:tool` component and the hardness from the block data. Broken blocks drop what vanilla drops: [`src/bin/src/systems/drops.rs`](../src/bin/src/systems/drops.rs) follows the loot table of the block for the item and the amount, and a block that needs the correct tool only drops when the held tool is right for it, using the first matching rule of the item's `minecraft:tool` component. **Missing:** mining fatigue and haste effects, silk touch, fortune.
- [x] **Block Placing** — [`src/bin/src/packet_handlers/play_packets/place_block.rs`](../src/bin/src/packet_handlers/play_packets/place_block.rs). Item-to-block mapping, face-based offset calculation, collision checking with players. Chests are intercepted before placement so a right click opens the screen, and a placed chest joins a matching neighbour into a double chest.
- [/] **Item Usage** — Eating works: the `use_item` packet handler ([`src/bin/src/packet_handlers/play_packets/use_item.rs`](../src/bin/src/packet_handlers/play_packets/use_item.rs)) and [`src/bin/src/systems/hunger.rs`](../src/bin/src/systems/hunger.rs) read the `minecraft:food` component of the held item, consume one unit and raise the food level. **Missing:** bows, shields, buckets, potions, projectile entities.

### **Redstone Engine**
- [ ] **Signal strength propagation** — Not implemented.
- [ ] **Quasi-connectivity** — Not implemented.
- [ ] **Update Order** — Not implemented.
- [ ] **Component logic** — Not implemented.

### **Fluid Dynamics**
- [ ] **Water/Lava spreading** — Not implemented.
- [ ] **Source block formation** — Not implemented.
- [ ] **Fluid mixing** — Not implemented.

### **Combat System**
- [ ] **Attack Cooldown** — Not implemented.
- [/] **Damage Sources** — The `interact` packet is handled by [`src/bin/src/packet_handlers/play_packets/interact.rs`](../src/bin/src/packet_handlers/play_packets/interact.rs), which validates the hit range and emits a `PlayerDamaged` event with a `DamageSource` ([`src/lib/messages/src/player_damage.rs`](../src/lib/messages/src/player_damage.rs)). [`src/bin/src/systems/health_system.rs`](../src/bin/src/systems/health_system.rs) applies it, sends the health and hurt-animation packets, fires `PlayerDied`, and [`src/bin/src/systems/listeners/player_death.rs`](../src/bin/src/systems/listeners/player_death.rs) respawns the player at the world spawn on `client_command`. **Missing:** only player-vs-player combat exists; mobs cannot be damaged yet, damage is not scaled by weapon, and there is no fall/void/starvation damage wire-up.
- [ ] **Armor formulas** — Not implemented.
- [ ] **Knockback** — Not implemented.
- [ ] **Critical Hits** — Not implemented.

### **Magic & Effects**
- [/] **Status Effects** — `ActiveEffects` component in [`src/lib/components/src/active_effects.rs`](../src/lib/components/src/active_effects.rs). **Missing:** Effect tick logic.
- [ ] **Enchantments** — Not implemented.
- [ ] **Potions** — Not implemented.

---

## VII. Command & Chat System

### **Chat**
- [x] **JSON Chat Components** — [`src/lib/text/src/lib.rs`](../src/lib/text/src/lib.rs). Full `TextComponent` with text/translate/keybind content, colors, formatting, click/hover events, NBT serialization.
- [/] **Chat Messages** — [`src/bin/src/packet_handlers/play_packets/chat_message.rs`](../src/bin/src/packet_handlers/play_packets/chat_message.rs). Basic `<player> message` broadcast via message queue.
- [ ] **Chat Signing (1.19+)** — Not implemented.

### **Brigadier (Command Parser)**
- [x] **Command Tree** — [`src/lib/commands/src/graph/`](../src/lib/commands/src/graph/). Command nodes with argument types, suggestions.
- [x] **Argument Types** — [`src/lib/commands/src/arg/`](../src/lib/commands/src/arg/). String, integer, and custom argument parsers.
- [/] **Target Selectors** — Not implemented as full selector system.
- [ ] **Permission Levels** — Not implemented.
- [/] **Command Logic** — [`src/lib/default_commands/src/`](../src/lib/default_commands/src/). Implemented: `/echo`, `/fly`, `/gamemode`, `/spawn`, `/nested` (test). **Missing:** Most vanilla commands.

---

### **Datapack Loader**
- [ ] **Zip/Folder parsing** — Not implemented.
- [ ] **pack.mcmeta reading** — Not implemented.
- [ ] **Functions (.mcfunction)** — Not implemented.
- [ ] **Loot Tables** — Not implemented.
- [ ] **Predicates** — Not implemented.
- [ ] **Tags** — Not implemented.

---

## IX. Scoreboard & Teams
- [ ] **Objectives** — Not implemented.
- [ ] **Scores** — Not implemented.
- [ ] **Teams** — Not implemented.
- [ ] **Display Slots** — Not implemented.

---

## X. Block Entities (Tile Entities)
- [ ] **Tickable Block Entities** — Not implemented.
- [ ] **Passive Block Entities** — Not implemented.
- [ ] **Complex Logic** — Not implemented.

---

## XI. Miscellaneous Subsystems
- [ ] **Raids** — Not implemented.
- [ ] **Patrols** — Not implemented.
- [ ] **Villager Gossiping** — Not implemented.
- [ ] **Explosions** — Not implemented.
- [ ] **Maps** — Not implemented.
- [ ] **Statistics** — Not implemented.
- [ ] **Boss Bars** — Not implemented.
- [ ] **Particles** — Not implemented.
- [ ] **Sounds** — Not implemented.

---

## XII. Security & Anti-Cheat (Vanilla)
- [ ] **Packet Limiter** — Not implemented.
- [ ] **Movement Checks** — Not implemented.
- [ ] **Chat Validation** — Not implemented.
- [ ] **Spam Filtering** — Not implemented.

---

# Significant Deviations from Vanilla Java Server
(This is just for reference for like funsies or something)

| Area | Vanilla (Java) | FerrumC (Rust) | Notes |
|------|----------------|----------------|-------|
| **Runtime** | JVM with GC | Native binary | No garbage collection, manual memory via Rust ownership. Faster startup, lower memory overhead. |
| **Async I/O** | Netty (Java NIO) | Tokio | Rust async/await with `mio` underneath. Connection handling in [`src/lib/net/src/connection.rs`](../src/lib/net/src/connection.rs). |
| **ECS** | Object-oriented entities | Bevy ECS | Archetype-based component storage. Entities are just IDs, components attached separately. See [`src/lib/entities/`](../src/lib/entities/). |
| **NBT** | Mojang's NBT lib | `ferrumc-nbt` | Custom tape-based parser with derive macros (`#[derive(NBTSerialize, NBTDeserialize)]`). Zero-copy parsing where possible. |
| **Config** | `server.properties` | TOML via `figment` | [`src/lib/config/src/server_config.rs`](../src/lib/config/src/server_config.rs). Hierarchical config merging (defaults + file override). |
| **Storage Backend** | Anvil (.mca) files | LMDB (heed) | [`src/lib/storage/src/lmdb.rs`](../src/lib/storage/src/lmdb.rs). Memory-mapped B+tree database. Import from Anvil supported. Configurable map size in GB. |
| **Packet Codec** | Hand-written | Derive macros | `#[derive(NetEncode, NetDecode)]` with `#[packet(...)]` attributes in [`src/lib/net/crates/codec/`](../src/lib/net/crates/codec/). |
| **World Import** | N/A | Dedicated CLI | `ferrumc import --import-path <path>` for batch chunk import from vanilla worlds. |
| **Logging** | Log4j | tracing | Zero-cost when disabled, structured logging, daily file rotation in `logs/`. |
| **Compression** | JDK zlib | yazi (Rust) | Pure Rust Zlib implementation. [`src/lib/net/src/compression.rs`](../src/lib/net/src/compression.rs). |
| **Encryption** | JCE/BouncyCastle | rsa/aes crates | Pure Rust crypto. RSA 1024-bit, AES-128-CFB8. [`src/lib/net/crates/encryption/`](../src/lib/net/crates/encryption/). |
| **Scheduling** | Single tick loop | Multi-schedule system | `ferrumc-scheduler` with independent schedules (tick, world_sync, keepalive) and missed-tick behaviors. |
| **Registry** | Runtime JSON parsing | Compile-time PHF maps | Block/item registries generated at build time via `build.rs`. Instant O(1) lookups. [`src/lib/registry/`](../src/lib/registry/). |
| **Chunk Cache** | Soft references | Moka cache | Weighted LRU cache with configurable TTL and capacity. [`src/lib/world/src/lib.rs`](../src/lib/world/src/lib.rs). |
| **Thread Pool** | Fork-join pool | Custom batch executor | Thread pool with `batch()` API for parallel chunk operations. |
| **Entity Data** | Static Java classes | Generated from JSON | Entity types, dimensions, and properties extracted from vanilla and accessed via `ferrumc-data` crate. |