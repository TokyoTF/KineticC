//! Tests that need more of the server than a single module: they are kept apart from the unit
//! tests inside each module because they cross crate boundaries.
//!
//! The tests only exist in a test build, so nothing here is compiled into the server binary.
#![cfg(test)]

pub mod item_entity_metadata;
pub mod player_data_persistence;
pub mod take_item_entity;
