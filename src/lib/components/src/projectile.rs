//! Components for projectile entities: the thrown items (ender pearls, snowballs, eggs) and the
//! firework rockets that boost an elytra flight.
//!
//! The entity itself is an ordinary [`EntityIdentity`] entity, so the existing position,
//! rotation and entity-update plumbing carries it. What makes it a projectile is [`Projectile`].

use bevy_ecs::prelude::{Component, Entity};
use ferrumc_inventories::slot::InventorySlot;
use ferrumc_world::dimension::Dimension;

/// What kind of projectile an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileKind {
    /// A thrown ender pearl: teleports its owner where it lands.
    EnderPearl,
    /// A thrown snowball.
    Snowball,
    /// A thrown egg.
    Egg,
    /// A firework rocket attached to its owner, boosting an elytra flight.
    FireworkRocket,
}

/// A projectile entity.
#[derive(Component, Debug, Clone)]
pub struct Projectile {
    /// What kind of projectile this is: decides the entity type clients render and the effect
    /// when it hits something.
    pub kind: ProjectileKind,
    /// The entity that threw it. Its protocol id goes in the spawn packet's `data` field, which
    /// is how the client links the projectile to its owner.
    pub owner: Entity,
    /// The stack the projectile was thrown from, rendered as the projectile's item.
    pub item: InventorySlot,
    /// The dimension the projectile flies in.
    pub dimension: Dimension,
    /// Ticks the projectile has existed.
    pub age: u32,
}

impl Projectile {
    /// Ticks after which a projectile is removed even if it never hit anything.
    ///
    /// Vanilla removes a throwable the moment it hits something; this cap only catches the ones
    /// that fly away forever, so they cannot pile up.
    pub const MAX_AGE: u32 = 20 * 60;

    pub fn new(
        kind: ProjectileKind,
        owner: Entity,
        item: InventorySlot,
        dimension: Dimension,
    ) -> Self {
        Self {
            kind,
            owner,
            item,
            dimension,
            age: 0,
        }
    }

    /// Whether the projectile has outlived its despawn timer.
    pub fn is_expired(&self) -> bool {
        self.age >= Self::MAX_AGE
    }
}
