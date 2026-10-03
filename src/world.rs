//! The world: entities, their components in archetype tables, and resources (singletons).

use std::any::{Any, TypeId};
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::bundle::Bundle;
use crate::component::{Component, ComponentId, Components};
use crate::entity::{Entities, Entity, Location};
use crate::hash::FxMap;
use crate::query::{QueryData, QueryFilter, QueryState};
use crate::storage::{Archetype, Column};

/// Mutable access that records a change: writing through it sets the value's changed tick, so
/// `Changed<T>` filters see it. Reading does not.
pub struct Mut<'a, T: ?Sized> {
    pub(crate) value: &'a mut T,
    pub(crate) changed: &'a mut u32,
    pub(crate) tick: u32,
}

impl<T: ?Sized> Deref for Mut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: ?Sized> DerefMut for Mut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        *self.changed = self.tick;
        self.value
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Mut<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(f)
    }
}

pub(crate) struct ResourceSlot {
    pub value: UnsafeCell<Box<dyn Any + Send + Sync>>,
    pub added: u32,
    pub changed: UnsafeCell<u32>,
}

pub struct World {
    pub(crate) entities: Entities,
    pub(crate) components: Components,
    pub(crate) archetypes: Vec<Archetype>,
    archetype_index: FxMap<Vec<ComponentId>, u32>,
    /// Bundle type spawned: its archetype and its column map (an index into `column_maps`).
    spawn_cache: FxMap<TypeId, (u32, u32)>,
    /// For a bundle and an archetype: the column of each bundle element.
    column_maps: Vec<Box<[u32]>>,
    pub(crate) resources: FxMap<TypeId, ResourceSlot>,
    tick: AtomicU32,
}

// Resources and columns are shared across system threads only as the scheduler allows.
unsafe impl Sync for World {}

impl Default for World {
    fn default() -> Self {
        World::new()
    }
}

/// `a` is newer than `since` and not newer than `now`, with ticks that may wrap around.
pub(crate) fn is_newer(a: u32, since: u32, now: u32) -> bool {
    now.wrapping_sub(a) < now.wrapping_sub(since)
}

impl World {
    pub fn new() -> World {
        let mut w = World {
            entities: Entities::default(),
            components: Components::default(),
            archetypes: Vec::new(),
            archetype_index: FxMap::default(),
            spawn_cache: FxMap::default(),
            column_maps: Vec::new(),
            resources: FxMap::default(),
            tick: AtomicU32::new(1),
        };
        // Archetype 0 holds entities without components.
        w.archetype_for(Vec::new());
        w
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    pub fn archetype_count(&self) -> usize {
        self.archetypes.len()
    }

    pub fn contains(&self, e: Entity) -> bool {
        self.entities.contains(e)
    }

    /// The current change tick.
    pub fn change_tick(&self) -> u32 {
        self.tick.load(Ordering::Acquire)
    }

    /// Claims the current change tick for a query or system run and advances the counter, so
    /// whatever happens afterwards is strictly newer.
    pub fn increment_change_tick(&self) -> u32 {
        self.tick.fetch_add(1, Ordering::AcqRel)
    }

    pub fn component_id<T: Component>(&mut self) -> ComponentId {
        self.components.id::<T>()
    }

    pub(crate) fn archetype_for(&mut self, ids: Vec<ComponentId>) -> u32 {
        if let Some(a) = self.archetype_index.get(&ids) {
            return *a;
        }
        let id = self.archetypes.len() as u32;
        let infos: Vec<_> = ids.iter().map(|c| self.components.info(*c)).collect();
        self.archetypes.push(Archetype::new(id, ids.clone(), &infos));
        self.archetype_index.insert(ids, id);
        id
    }

    /// The bundle's component ids in bundle order, and sorted. Panics on a repeated type.
    fn bundle_ids<B: Bundle>(&mut self) -> (Vec<ComponentId>, Vec<ComponentId>) {
        let ids = B::ids(&mut self.components);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "a bundle contains the same component type twice");
        (ids, sorted)
    }

    fn column_map(&mut self, a: u32, ids: &[ComponentId]) -> u32 {
        let arch = &self.archetypes[a as usize];
        let map: Box<[u32]> = ids.iter().map(|c| arch.column_index(*c).unwrap() as u32).collect();
        self.column_maps.push(map);
        self.column_maps.len() as u32 - 1
    }

    fn spawn_target<B: Bundle>(&mut self) -> (u32, u32) {
        if let Some(x) = self.spawn_cache.get(&TypeId::of::<B>()) {
            return *x;
        }
        let (ids, sorted) = self.bundle_ids::<B>();
        let a = self.archetype_for(sorted);
        let m = self.column_map(a, &ids);
        self.spawn_cache.insert(TypeId::of::<B>(), (a, m));
        (a, m)
    }

    fn push_bundle<B: Bundle>(entities: &mut Entities, arch: &mut Archetype, cols: &[u32], bundle: B, tick: u32) -> Entity {
        let e = entities.alloc();
        let row = arch.entities.len() as u32;
        let mut i = 0;
        bundle.take(&mut |p| {
            unsafe { arch.columns[cols[i] as usize].push(p, tick) };
            i += 1;
        });
        arch.entities.push(e);
        entities.set_location(e, Location { archetype: arch.id, row });
        e
    }

    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        let (a, m) = self.spawn_target::<B>();
        let tick = self.change_tick();
        Self::push_bundle(&mut self.entities, &mut self.archetypes[a as usize], &self.column_maps[m as usize], bundle, tick)
    }

    /// Spawns many entities of one bundle type; faster than `spawn` in a loop.
    pub fn spawn_batch<B: Bundle, I: IntoIterator<Item = B>>(&mut self, items: I) -> Vec<Entity> {
        let (a, m) = self.spawn_target::<B>();
        let tick = self.change_tick();
        let items = items.into_iter();
        let n = items.size_hint().0;
        let arch = &mut self.archetypes[a as usize];
        for c in &mut arch.columns {
            c.reserve(n);
        }
        arch.entities.reserve(n);
        let mut out = Vec::with_capacity(n);
        for b in items {
            out.push(Self::push_bundle(&mut self.entities, arch, &self.column_maps[m as usize], b, tick));
        }
        out
    }

    pub fn despawn(&mut self, e: Entity) -> bool {
        let Some(loc) = self.entities.free(e) else { return false };
        let arch = &mut self.archetypes[loc.archetype as usize];
        for c in &mut arch.columns {
            unsafe { c.swap_remove(loc.row as usize, None) };
        }
        arch.entities.swap_remove(loc.row as usize);
        if let Some(moved) = arch.entities.get(loc.row as usize).copied() {
            self.entities.set_location(moved, loc);
        }
        true
    }

    fn two(&mut self, a: u32, b: u32) -> (&mut Archetype, &mut Archetype) {
        let (a, b) = (a as usize, b as usize);
        assert_ne!(a, b);
        if a < b {
            let (x, y) = self.archetypes.split_at_mut(b);
            (&mut x[a], &mut y[0])
        } else {
            let (x, y) = self.archetypes.split_at_mut(a);
            (&mut y[0], &mut x[b])
        }
    }

    /// Moves an entity to another archetype. Columns the target lacks are handed to `leftover`
    /// (which must remove the row: drop or move the value out). Returns the new row.
    fn move_entity(
        &mut self,
        e: Entity,
        loc: Location,
        target: u32,
        leftover: &mut dyn FnMut(ComponentId, &mut Column, usize),
    ) -> u32 {
        let row = loc.row as usize;
        let (src, dst) = self.two(loc.archetype, target);
        let new_row = dst.entities.len() as u32;
        for (i, c) in src.components.iter().enumerate() {
            match dst.column_index(*c) {
                Some(j) => unsafe { src.columns[i].move_row(row, &mut dst.columns[j]) },
                None => leftover(*c, &mut src.columns[i], row),
            }
        }
        src.entities.swap_remove(row);
        let moved = src.entities.get(row).copied();
        dst.entities.push(e);
        if let Some(m) = moved {
            self.entities.set_location(m, loc);
        }
        self.entities.set_location(e, Location { archetype: target, row: new_row });
        new_row
    }

    /// Adds components to an entity (replacing ones it already has). False if it does not exist.
    pub fn insert<B: Bundle>(&mut self, e: Entity, bundle: B) -> bool {
        let Some(loc) = self.entities.location(e) else { return false };
        let tick = self.change_tick();
        let key = TypeId::of::<B>();
        let cached = self.archetypes[loc.archetype as usize].add_edges.get(&key).copied();
        let (target, m) = match cached {
            Some(x) => x,
            None => {
                let (ids, sorted) = self.bundle_ids::<B>();
                let t = self.target_with(loc.archetype, &sorted);
                let m = self.column_map(t, &ids);
                self.archetypes[loc.archetype as usize].add_edges.insert(key, (t, m));
                (t, m)
            }
        };
        let row = if target == loc.archetype {
            loc.row
        } else {
            self.move_entity(e, loc, target, &mut |_, _, _| unreachable!("inserting keeps every component"))
        } as usize;
        let arch = &mut self.archetypes[target as usize];
        let cols = &self.column_maps[m as usize];
        let mut i = 0;
        bundle.take(&mut |p| {
            let col = &mut arch.columns[cols[i] as usize];
            if col.len() > row {
                unsafe { col.replace(row, p, tick) };
            } else {
                unsafe { col.push(p, tick) };
            }
            i += 1;
        });
        true
    }

    fn target_with(&mut self, from: u32, ids: &[ComponentId]) -> u32 {
        let mut all = self.archetypes[from as usize].components.clone();
        all.extend_from_slice(ids);
        all.sort_unstable();
        all.dedup();
        self.archetype_for(all)
    }

    /// Removes a component and returns it.
    pub fn remove<T: Component>(&mut self, e: Entity) -> Option<T> {
        let loc = self.entities.location(e)?;
        let id = self.components.get_id::<T>()?;
        let src = &self.archetypes[loc.archetype as usize];
        if !src.has(id) {
            return None;
        }
        let edge = src.remove_edges.get(&id).copied();
        let target = match edge {
            Some(t) => t,
            None => {
                let ids: Vec<ComponentId> =
                    self.archetypes[loc.archetype as usize].components.iter().copied().filter(|c| *c != id).collect();
                let t = self.archetype_for(ids);
                self.archetypes[loc.archetype as usize].remove_edges.insert(id, t);
                t
            }
        };
        let mut out = MaybeUninit::<T>::uninit();
        self.move_entity(e, loc, target, &mut |c, col, row| {
            debug_assert_eq!(c, id);
            unsafe { col.swap_remove(row, Some(out.as_mut_ptr() as *mut u8)) };
        });
        Some(unsafe { out.assume_init() })
    }

    pub fn get<T: Component>(&self, e: Entity) -> Option<&T> {
        let loc = self.entities.location(e)?;
        let id = self.components.get_id::<T>()?;
        let col = self.archetypes[loc.archetype as usize].column(id)?;
        Some(unsafe { &*(col.ptr(loc.row as usize) as *const T) })
    }

    pub fn get_mut<T: Component>(&mut self, e: Entity) -> Option<Mut<'_, T>> {
        let loc = self.entities.location(e)?;
        let id = self.components.get_id::<T>()?;
        let tick = self.change_tick();
        let col = self.archetypes[loc.archetype as usize].column(id)?;
        let row = loc.row as usize;
        Some(Mut {
            value: unsafe { &mut *(col.ptr(row) as *mut T) },
            changed: unsafe { &mut *col.changed_ticks().add(row) },
            tick,
        })
    }

    pub fn has<T: Component>(&self, e: Entity) -> bool {
        self.get::<T>(e).is_some()
    }

    pub fn query<Q: QueryData>(&mut self) -> QueryState<Q, ()> {
        QueryState::new(self)
    }

    pub fn query_filtered<Q: QueryData, F: QueryFilter>(&mut self) -> QueryState<Q, F> {
        QueryState::new(self)
    }

    pub fn insert_resource<R: Send + Sync + 'static>(&mut self, r: R) {
        let tick = self.change_tick();
        self.resources.insert(
            TypeId::of::<R>(),
            ResourceSlot { value: UnsafeCell::new(Box::new(r)), added: tick, changed: UnsafeCell::new(tick) },
        );
    }

    pub fn resource<R: Send + Sync + 'static>(&self) -> Option<&R> {
        let s = self.resources.get(&TypeId::of::<R>())?;
        unsafe { (*s.value.get()).downcast_ref::<R>() }
    }

    pub fn resource_mut<R: Send + Sync + 'static>(&mut self) -> Option<Mut<'_, R>> {
        let tick = self.change_tick();
        let s = self.resources.get_mut(&TypeId::of::<R>())?;
        let value = s.value.get_mut().downcast_mut::<R>()?;
        Some(Mut { value, changed: s.changed.get_mut(), tick })
    }

    pub fn remove_resource<R: Send + Sync + 'static>(&mut self) -> Option<R> {
        let s = self.resources.remove(&TypeId::of::<R>())?;
        s.value.into_inner().downcast::<R>().ok().map(|b| *b)
    }

    pub fn has_resource<R: Send + Sync + 'static>(&self) -> bool {
        self.resources.contains_key(&TypeId::of::<R>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Pos(f32, f32);
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct Vel(f32, f32);
    #[derive(Debug, PartialEq)]
    struct Name(String);

    #[test]
    fn spawn_get_insert_remove_despawn() {
        let mut w = World::new();
        let a = w.spawn((Pos(1.0, 2.0), Vel(3.0, 4.0)));
        let b = w.spawn((Pos(5.0, 6.0),));
        assert_eq!(w.get::<Pos>(a), Some(&Pos(1.0, 2.0)));
        assert_eq!(w.get::<Vel>(b), None);
        assert!(w.insert(b, (Vel(7.0, 8.0), Name("b".into()))));
        assert_eq!(w.get::<Vel>(b), Some(&Vel(7.0, 8.0)));
        assert_eq!(w.get::<Pos>(b), Some(&Pos(5.0, 6.0)), "existing components move along");
        // Inserting an existing component replaces it in place.
        let arch = w.archetype_count();
        w.insert(b, (Pos(0.0, 0.0),));
        assert_eq!(w.archetype_count(), arch);
        assert_eq!(w.get::<Pos>(b), Some(&Pos(0.0, 0.0)));
        assert_eq!(w.remove::<Name>(b), Some(Name("b".into())));
        assert_eq!(w.remove::<Name>(b), None);
        assert_eq!(w.get::<Vel>(b), Some(&Vel(7.0, 8.0)));
        assert!(w.despawn(a));
        assert!(!w.despawn(a));
        assert!(!w.contains(a));
        assert_eq!(w.get::<Pos>(b), Some(&Pos(0.0, 0.0)), "the entity moved into the freed row is still found");
        assert_eq!(w.len(), 1);
    }

    #[test]
    #[should_panic(expected = "same component type twice")]
    fn duplicate_types_in_a_bundle_are_refused() {
        World::new().spawn((Pos(0.0, 0.0), Pos(1.0, 1.0)));
    }

    #[test]
    fn resources() {
        let mut w = World::new();
        w.insert_resource(10u64);
        assert_eq!(w.resource::<u64>(), Some(&10));
        *w.resource_mut::<u64>().unwrap() += 5;
        assert_eq!(w.resource::<u64>(), Some(&15));
        assert_eq!(w.remove_resource::<u64>(), Some(15));
        assert!(!w.has_resource::<u64>());
        assert!(w.resource::<u32>().is_none());
    }

    #[test]
    fn ticks_compare_across_wraparound() {
        assert!(is_newer(5, 3, 10));
        assert!(!is_newer(3, 5, 10));
        assert!(!is_newer(3, 3, 10));
        // Wrapped: since = u32::MAX - 1, value = 2, now = 5.
        assert!(is_newer(2, u32::MAX - 1, 5));
        assert!(!is_newer(u32::MAX - 2, u32::MAX - 1, 5));
    }
}
