//! `tessera`: an entity component system with archetype storage, cached queries with change
//! detection, and systems scheduled in parallel from the access they declare.

pub mod access;
pub mod bundle;
pub mod component;
pub mod entity;
pub mod hash;
pub mod query;
pub mod schedule;
pub mod storage;
pub mod system;
pub mod world;

pub use bundle::Bundle;
pub use component::Component;
pub use entity::Entity;
pub use query::{Added, Changed, QueryState, With, Without};
pub use schedule::Schedule;
pub use system::{Commands, Local, Query, Res, ResMut};
pub use world::{Mut, World};
