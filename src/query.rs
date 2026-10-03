//! Queries: `QueryState<(Entity, &Pos, &mut Vel), (With<Player>, Changed<Pos>)>` finds the
//! archetypes that match once, remembers them (checking only archetypes created since), and
//! iterates their columns directly.

use std::marker::PhantomData;

use crate::access::Access;
use crate::component::{Component, ComponentId, Components};
use crate::entity::Entity;
use crate::storage::Archetype;
use crate::world::{is_newer, Mut, World};

/// The ticks a query compares change ticks with: changes after `last_run`, up to `this_run`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticks {
    pub last_run: u32,
    pub this_run: u32,
}

/// Something a query fetches per archetype.
///
/// # Safety
/// `access` must describe every component `fetch` gives access to, and `matches` must be true
/// only for archetypes `fetch` can read.
pub unsafe trait WorldQuery {
    type State: Send + Sync + 'static;
    type Fetch<'w>: Clone;
    fn init(components: &mut Components) -> Self::State;
    fn matches(state: &Self::State, arch: &Archetype) -> bool;
    fn access(state: &Self::State, access: &mut Access);
    /// # Safety
    /// `arch` must match, and the caller must hold the access `access` describes.
    unsafe fn fetch<'w>(state: &Self::State, arch: &'w Archetype, ticks: Ticks) -> Self::Fetch<'w>;
}

/// What a query yields per entity.
///
/// # Safety
/// `item` must only touch row `row` of the fetched archetype.
pub unsafe trait QueryData: WorldQuery {
    type Item<'w>;
    /// # Safety
    /// `row` must be in range and not yielded twice for a mutable query.
    unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w>;
}

/// Query data that only reads, so it may be iterated through `&World`.
///
/// # Safety
/// The implementation must never write.
pub unsafe trait ReadOnly: QueryData {}

/// A condition on entities (`With`, `Without`, `Added`, `Changed`, tuples of them).
///
/// # Safety
/// As [`WorldQuery`].
pub unsafe trait QueryFilter: WorldQuery {
    /// True when every row of a matching archetype passes (no per-row test).
    const DENSE: bool;
    /// # Safety
    /// `row` must be in range.
    unsafe fn filter<'w>(f: &mut Self::Fetch<'w>, row: usize) -> bool;
}

// ---- &T --------------------------------------------------------------------------------------

pub struct ReadFetch<'w, T> {
    ptr: *const T,
    _w: PhantomData<&'w T>,
}

impl<T> Clone for ReadFetch<'_, T> {
    fn clone(&self) -> Self {
        ReadFetch { ptr: self.ptr, _w: PhantomData }
    }
}

unsafe impl<T: Component> WorldQuery for &T {
    type State = ComponentId;
    type Fetch<'w> = ReadFetch<'w, T>;
    fn init(c: &mut Components) -> ComponentId {
        c.id::<T>()
    }
    fn matches(s: &ComponentId, a: &Archetype) -> bool {
        a.has(*s)
    }
    fn access(s: &ComponentId, acc: &mut Access) {
        acc.read(*s);
        acc.require(*s);
    }
    unsafe fn fetch<'w>(s: &ComponentId, a: &'w Archetype, _: Ticks) -> ReadFetch<'w, T> {
        ReadFetch { ptr: a.column(*s).unwrap().ptr(0) as *const T, _w: PhantomData }
    }
}

unsafe impl<T: Component> QueryData for &T {
    type Item<'w> = &'w T;
    unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w> {
        unsafe { &*f.ptr.add(row) }
    }
}

unsafe impl<T: Component> ReadOnly for &T {}

// ---- &mut T ----------------------------------------------------------------------------------

pub struct WriteFetch<'w, T> {
    ptr: *mut T,
    changed: *mut u32,
    tick: u32,
    _w: PhantomData<&'w mut T>,
}

impl<T> Clone for WriteFetch<'_, T> {
    fn clone(&self) -> Self {
        WriteFetch { ptr: self.ptr, changed: self.changed, tick: self.tick, _w: PhantomData }
    }
}

unsafe impl<T: Component> WorldQuery for &mut T {
    type State = ComponentId;
    type Fetch<'w> = WriteFetch<'w, T>;
    fn init(c: &mut Components) -> ComponentId {
        c.id::<T>()
    }
    fn matches(s: &ComponentId, a: &Archetype) -> bool {
        a.has(*s)
    }
    fn access(s: &ComponentId, acc: &mut Access) {
        acc.write(*s);
        acc.require(*s);
    }
    unsafe fn fetch<'w>(s: &ComponentId, a: &'w Archetype, t: Ticks) -> WriteFetch<'w, T> {
        let col = a.column(*s).unwrap();
        WriteFetch { ptr: col.ptr(0) as *mut T, changed: col.changed_ticks(), tick: t.this_run, _w: PhantomData }
    }
}

unsafe impl<T: Component> QueryData for &mut T {
    type Item<'w> = Mut<'w, T>;
    unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w> {
        unsafe { Mut { value: &mut *f.ptr.add(row), changed: &mut *f.changed.add(row), tick: f.tick } }
    }
}

// ---- Option<Q> -------------------------------------------------------------------------------

unsafe impl<Q: WorldQuery> WorldQuery for Option<Q> {
    type State = Q::State;
    type Fetch<'w> = Option<Q::Fetch<'w>>;
    fn init(c: &mut Components) -> Q::State {
        Q::init(c)
    }
    fn matches(_: &Q::State, _: &Archetype) -> bool {
        true
    }
    fn access(s: &Q::State, acc: &mut Access) {
        // The same reads and writes, but nothing is required.
        let mut inner = Access::default();
        Q::access(s, &mut inner);
        acc.reads.union_with(&inner.reads);
        acc.writes.union_with(&inner.writes);
    }
    unsafe fn fetch<'w>(s: &Q::State, a: &'w Archetype, t: Ticks) -> Option<Q::Fetch<'w>> {
        if Q::matches(s, a) {
            Some(unsafe { Q::fetch(s, a, t) })
        } else {
            None
        }
    }
}

unsafe impl<Q: QueryData> QueryData for Option<Q> {
    type Item<'w> = Option<Q::Item<'w>>;
    unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w> {
        f.as_mut().map(|f| unsafe { Q::item(f, row) })
    }
}

unsafe impl<Q: ReadOnly> ReadOnly for Option<Q> {}

// ---- Entity ----------------------------------------------------------------------------------

#[derive(Clone)]
pub struct EntityFetch<'w>(*const Entity, PhantomData<&'w Entity>);

unsafe impl WorldQuery for Entity {
    type State = ();
    type Fetch<'w> = EntityFetch<'w>;
    fn init(_: &mut Components) {}
    fn matches(_: &(), _: &Archetype) -> bool {
        true
    }
    fn access(_: &(), _: &mut Access) {}
    unsafe fn fetch<'w>(_: &(), a: &'w Archetype, _: Ticks) -> EntityFetch<'w> {
        EntityFetch(a.entities.as_ptr(), PhantomData)
    }
}

unsafe impl QueryData for Entity {
    type Item<'w> = Entity;
    unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w> {
        unsafe { *f.0.add(row) }
    }
}

unsafe impl ReadOnly for Entity {}

// ---- filters ---------------------------------------------------------------------------------

/// Entities that have `T` (not fetched).
pub struct With<T>(PhantomData<T>);
/// Entities that do not have `T`.
pub struct Without<T>(PhantomData<T>);
/// Entities whose `T` was added since the query (or system) last ran.
pub struct Added<T>(PhantomData<T>);
/// Entities whose `T` was added or written since the query (or system) last ran.
pub struct Changed<T>(PhantomData<T>);

unsafe impl<T: Component> WorldQuery for With<T> {
    type State = ComponentId;
    type Fetch<'w> = ();
    fn init(c: &mut Components) -> ComponentId {
        c.id::<T>()
    }
    fn matches(s: &ComponentId, a: &Archetype) -> bool {
        a.has(*s)
    }
    fn access(s: &ComponentId, acc: &mut Access) {
        acc.require(*s);
    }
    unsafe fn fetch<'w>(_: &ComponentId, _: &'w Archetype, _: Ticks) -> Self::Fetch<'w> {}
}

unsafe impl<T: Component> QueryFilter for With<T> {
    const DENSE: bool = true;
    unsafe fn filter<'w>(_: &mut Self::Fetch<'w>, _: usize) -> bool {
        true
    }
}

unsafe impl<T: Component> WorldQuery for Without<T> {
    type State = ComponentId;
    type Fetch<'w> = ();
    fn init(c: &mut Components) -> ComponentId {
        c.id::<T>()
    }
    fn matches(s: &ComponentId, a: &Archetype) -> bool {
        !a.has(*s)
    }
    fn access(s: &ComponentId, acc: &mut Access) {
        acc.exclude(*s);
    }
    unsafe fn fetch<'w>(_: &ComponentId, _: &'w Archetype, _: Ticks) -> Self::Fetch<'w> {}
}

unsafe impl<T: Component> QueryFilter for Without<T> {
    const DENSE: bool = true;
    unsafe fn filter<'w>(_: &mut Self::Fetch<'w>, _: usize) -> bool {
        true
    }
}

#[derive(Clone)]
pub struct TickFetch<'w> {
    ticks: *const u32,
    t: Ticks,
    _w: PhantomData<&'w u32>,
}

macro_rules! tick_filter {
    ($name:ident, $col:ident) => {
        unsafe impl<T: Component> WorldQuery for $name<T> {
            type State = ComponentId;
            type Fetch<'w> = TickFetch<'w>;
            fn init(c: &mut Components) -> ComponentId {
                c.id::<T>()
            }
            fn matches(s: &ComponentId, a: &Archetype) -> bool {
                a.has(*s)
            }
            fn access(s: &ComponentId, acc: &mut Access) {
                acc.read(*s);
                acc.require(*s);
            }
            unsafe fn fetch<'w>(s: &ComponentId, a: &'w Archetype, t: Ticks) -> TickFetch<'w> {
                TickFetch { ticks: a.column(*s).unwrap().$col(), t, _w: PhantomData }
            }
        }

        unsafe impl<T: Component> QueryFilter for $name<T> {
            const DENSE: bool = false;
            unsafe fn filter<'w>(f: &mut Self::Fetch<'w>, row: usize) -> bool {
                is_newer(unsafe { *f.ticks.add(row) }, f.t.last_run, f.t.this_run)
            }
        }
    };
}

tick_filter!(Added, added_ticks);
tick_filter!(Changed, changed_ticks);

unsafe impl WorldQuery for () {
    type State = ();
    type Fetch<'w> = ();
    fn init(_: &mut Components) {}
    fn matches(_: &(), _: &Archetype) -> bool {
        true
    }
    fn access(_: &(), _: &mut Access) {}
    unsafe fn fetch<'w>(_: &(), _: &'w Archetype, _: Ticks) -> Self::Fetch<'w> {}
}

unsafe impl QueryData for () {
    type Item<'w> = ();
    unsafe fn item<'w>(_: &mut Self::Fetch<'w>, _: usize) -> Self::Item<'w> {}
}

unsafe impl ReadOnly for () {}

unsafe impl QueryFilter for () {
    const DENSE: bool = true;
    unsafe fn filter<'w>(_: &mut Self::Fetch<'w>, _: usize) -> bool {
        true
    }
}

macro_rules! tuple {
    ($($q:ident $i:tt),+) => {
        unsafe impl<$($q: WorldQuery),+> WorldQuery for ($($q,)+) {
            type State = ($($q::State,)+);
            type Fetch<'w> = ($($q::Fetch<'w>,)+);
            fn init(c: &mut Components) -> Self::State {
                ($($q::init(c),)+)
            }
            fn matches(s: &Self::State, a: &Archetype) -> bool {
                true $(&& $q::matches(&s.$i, a))+
            }
            fn access(s: &Self::State, acc: &mut Access) {
                $(
                    let mut one = Access::default();
                    $q::access(&s.$i, &mut one);
                    acc.merge_checked(&one, "a query");
                )+
            }
            unsafe fn fetch<'w>(s: &Self::State, a: &'w Archetype, t: Ticks) -> Self::Fetch<'w> {
                unsafe { ($($q::fetch(&s.$i, a, t),)+) }
            }
        }

        unsafe impl<$($q: QueryData),+> QueryData for ($($q,)+) {
            type Item<'w> = ($($q::Item<'w>,)+);
            unsafe fn item<'w>(f: &mut Self::Fetch<'w>, row: usize) -> Self::Item<'w> {
                unsafe { ($($q::item(&mut f.$i, row),)+) }
            }
        }

        unsafe impl<$($q: ReadOnly),+> ReadOnly for ($($q,)+) {}

        unsafe impl<$($q: QueryFilter),+> QueryFilter for ($($q,)+) {
            const DENSE: bool = true $(&& $q::DENSE)+;
            unsafe fn filter<'w>(f: &mut Self::Fetch<'w>, row: usize) -> bool {
                true $(&& unsafe { $q::filter(&mut f.$i, row) })+
            }
        }
    };
}

tuple!(A 0);
tuple!(A 0, B 1);
tuple!(A 0, B 1, C 2);
tuple!(A 0, B 1, C 2, D 3);
tuple!(A 0, B 1, C 2, D 3, E 4);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);

// ---- query state and iteration ---------------------------------------------------------------

pub struct QueryState<Q: QueryData, F: QueryFilter = ()> {
    q: Q::State,
    f: F::State,
    matched: Vec<u32>,
    seen: usize,
    pub(crate) access: Access,
    last_run: u32,
    _m: PhantomData<fn() -> (Q, F)>,
}

impl<Q: QueryData, F: QueryFilter> QueryState<Q, F> {
    pub fn new(world: &mut World) -> Self {
        let q = Q::init(&mut world.components);
        let f = F::init(&mut world.components);
        let mut access = Access::default();
        Q::access(&q, &mut access);
        let mut fa = Access::default();
        F::access(&f, &mut fa);
        // Filters only read ticks; reading a component the query writes is fine there.
        access.required.union_with(&fa.required);
        access.excluded.union_with(&fa.excluded);
        access.reads.union_with(&fa.reads);
        let mut s = QueryState { q, f, matched: Vec::new(), seen: 0, access, last_run: 0, _m: PhantomData };
        s.update(world);
        s
    }

    /// Picks up archetypes created since the last call.
    pub fn update(&mut self, world: &World) {
        for a in &world.archetypes[self.seen..] {
            if Q::matches(&self.q, a) && F::matches(&self.f, a) {
                self.matched.push(a.id);
            }
        }
        self.seen = world.archetypes.len();
    }

    pub fn access(&self) -> &Access {
        &self.access
    }

    pub fn matched_archetypes(&self) -> usize {
        self.matched.len()
    }

    /// Read-only iteration. `Added`/`Changed` filters see changes since this state last iterated.
    pub fn iter<'w, 's>(&'s mut self, world: &'w World) -> QueryIter<'w, 's, Q, F>
    where
        Q: ReadOnly,
    {
        self.update(world);
        let ticks = Ticks { last_run: self.last_run, this_run: world.increment_change_tick() };
        self.last_run = ticks.this_run;
        unsafe { QueryIter::new(world, &self.q, &self.f, &self.matched, ticks) }
    }

    pub fn iter_mut<'w, 's>(&'s mut self, world: &'w mut World) -> QueryIter<'w, 's, Q, F> {
        self.update(world);
        let ticks = Ticks { last_run: self.last_run, this_run: world.increment_change_tick() };
        self.last_run = ticks.this_run;
        unsafe { QueryIter::new(world, &self.q, &self.f, &self.matched, ticks) }
    }

    /// Iteration with explicit ticks, for systems. The caller guarantees the access.
    ///
    /// # Safety
    /// No conflicting access to the world may exist while the iterator lives.
    pub unsafe fn iter_unchecked<'w, 's>(&'s self, world: &'w World, ticks: Ticks) -> QueryIter<'w, 's, Q, F> {
        unsafe { QueryIter::new(world, &self.q, &self.f, &self.matched, ticks) }
    }

    /// The item of one entity, if it matches.
    ///
    /// # Safety
    /// As [`iter_unchecked`](Self::iter_unchecked).
    pub unsafe fn get_unchecked<'w>(&self, world: &'w World, e: Entity, ticks: Ticks) -> Option<Q::Item<'w>> {
        let loc = world.entities.location(e)?;
        let a = &world.archetypes[loc.archetype as usize];
        if !Q::matches(&self.q, a) || !F::matches(&self.f, a) {
            return None;
        }
        unsafe {
            let mut ff = F::fetch(&self.f, a, ticks);
            if !F::filter(&mut ff, loc.row as usize) {
                return None;
            }
            let mut qf = Q::fetch(&self.q, a, ticks);
            Some(Q::item(&mut qf, loc.row as usize))
        }
    }

    pub fn get<'w>(&self, world: &'w World, e: Entity) -> Option<Q::Item<'w>>
    where
        Q: ReadOnly,
    {
        let t = world.change_tick();
        unsafe { self.get_unchecked(world, e, Ticks { last_run: self.last_run, this_run: t }) }
    }

    pub fn get_mut<'w>(&mut self, world: &'w mut World, e: Entity) -> Option<Q::Item<'w>> {
        let t = world.change_tick();
        unsafe { self.get_unchecked(world, e, Ticks { last_run: self.last_run, this_run: t }) }
    }

    pub(crate) fn parts(&self) -> (&Q::State, &F::State, &[u32]) {
        (&self.q, &self.f, &self.matched)
    }
}

pub struct QueryIter<'w, 's, Q: QueryData, F: QueryFilter> {
    world: &'w World,
    q: &'s Q::State,
    f: &'s F::State,
    archetypes: std::slice::Iter<'s, u32>,
    fetch: Option<(Q::Fetch<'w>, F::Fetch<'w>)>,
    row: usize,
    len: usize,
    ticks: Ticks,
}

impl<'w, 's, Q: QueryData, F: QueryFilter> QueryIter<'w, 's, Q, F> {
    /// # Safety
    /// The caller must hold the access the query describes.
    unsafe fn new(world: &'w World, q: &'s Q::State, f: &'s F::State, matched: &'s [u32], ticks: Ticks) -> Self {
        QueryIter { world, q, f, archetypes: matched.iter(), fetch: None, row: 0, len: 0, ticks }
    }
}

impl<'w, Q: QueryData, F: QueryFilter> Iterator for QueryIter<'w, '_, Q, F> {
    type Item = Q::Item<'w>;

    #[inline]
    fn next(&mut self) -> Option<Q::Item<'w>> {
        loop {
            if let Some((qf, ff)) = self.fetch.as_mut() {
                while self.row < self.len {
                    let r = self.row;
                    self.row += 1;
                    if F::DENSE || unsafe { F::filter(ff, r) } {
                        return Some(unsafe { Q::item(qf, r) });
                    }
                }
            }
            let a = &self.world.archetypes[*self.archetypes.next()? as usize];
            self.row = 0;
            self.len = a.len();
            self.fetch = if self.len == 0 {
                None
            } else {
                unsafe { Some((Q::fetch(self.q, a, self.ticks), F::fetch(self.f, a, self.ticks))) }
            };
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if !F::DENSE {
            return (0, None);
        }
        let rest: usize = self.archetypes.clone().map(|a| self.world.archetypes[*a as usize].len()).sum();
        let n = rest + self.len - self.row;
        (n, Some(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Clone, Copy)]
    struct A(u32);
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct B(u32);
    #[derive(Debug, PartialEq, Clone, Copy)]
    struct C;

    fn world() -> (World, Vec<Entity>) {
        let mut w = World::new();
        let es = vec![w.spawn((A(1),)), w.spawn((A(2), B(20))), w.spawn((A(3), B(30), C)), w.spawn((B(40),))];
        (w, es)
    }

    #[test]
    fn tuples_options_entities_and_filters() {
        let (mut w, es) = world();
        let mut q = w.query::<(Entity, &A, Option<&B>)>();
        let mut got: Vec<(Entity, u32, Option<u32>)> = q.iter(&w).map(|(e, a, b)| (e, a.0, b.map(|b| b.0))).collect();
        got.sort_by_key(|x| x.1);
        assert_eq!(got, vec![(es[0], 1, None), (es[1], 2, Some(20)), (es[2], 3, Some(30))]);
        let mut with = w.query_filtered::<&A, (With<B>, Without<C>)>();
        assert_eq!(with.iter(&w).map(|a| a.0).collect::<Vec<_>>(), vec![2]);
        assert_eq!(with.iter(&w).size_hint(), (1, Some(1)));
    }

    #[test]
    fn mutation_through_queries_and_change_detection() {
        let (mut w, es) = world();
        let mut changed = w.query_filtered::<&A, Changed<A>>();
        assert_eq!(changed.iter(&w).count(), 3, "everything counts as changed the first time");
        assert_eq!(changed.iter(&w).count(), 0);
        let mut q = w.query::<(&mut A, &B)>();
        for (mut a, b) in q.iter_mut(&mut w) {
            if b.0 == 20 {
                a.0 += 100;
            }
            // Reading through Mut does not count as a change.
            let _ = a.0;
        }
        assert_eq!(changed.iter(&w).map(|a| a.0).collect::<Vec<_>>(), vec![102]);
        let mut added = w.query_filtered::<Entity, Added<B>>();
        assert_eq!(added.iter(&w).count(), 3);
        w.insert(es[0], (B(10),));
        assert_eq!(added.iter(&w).collect::<Vec<_>>(), vec![es[0]]);
        // New archetypes created after the state was made are picked up.
        w.spawn((A(9), C));
        assert_eq!(w.query::<&A>().iter(&w).count(), 4);
    }

    #[test]
    fn get_one_entity() {
        let (mut w, es) = world();
        let q = w.query_filtered::<&A, With<B>>();
        assert_eq!(q.get(&w, es[1]), Some(&A(2)));
        assert_eq!(q.get(&w, es[0]), None);
        let mut m = w.query::<&mut B>();
        m.get_mut(&mut w, es[3]).unwrap().0 = 41;
        assert_eq!(w.get::<B>(es[3]), Some(&B(41)));
    }

    #[test]
    #[should_panic(expected = "mutably")]
    fn a_query_that_aliases_itself_panics() {
        World::new().query::<(&mut A, &A)>();
    }
}
