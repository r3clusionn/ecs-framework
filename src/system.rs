//! Systems: plain functions whose parameters say what they touch.
//!
//! ```
//! use tessera::{system::{Query, Res, Commands}, World, With};
//! #[derive(Clone, Copy)] struct Pos(f32);
//! #[derive(Clone, Copy)] struct Vel(f32);
//! struct Dt(f32);
//! fn movement(mut q: Query<(&mut Pos, &Vel)>, dt: Res<Dt>) {
//!     for (mut p, v) in q.iter_mut() {
//!         p.0 += v.0 * dt.0;
//!     }
//! }
//! let mut w = World::new();
//! w.insert_resource(Dt(0.5));
//! w.spawn((Pos(0.0), Vel(2.0)));
//! let mut s = tessera::schedule::Schedule::new();
//! s.add_system(movement);
//! s.run(&mut w);
//! assert_eq!(w.query::<&Pos>().iter(&w).next().unwrap().0, 1.0);
//! ```

use std::any::TypeId;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;

use crate::access::{Access, TypeIdKey};
use crate::bundle::Bundle;
use crate::component::Component;
use crate::entity::Entity;
use crate::query::{QueryData, QueryFilter, QueryIter, QueryState, ReadOnly, Ticks};
use crate::world::{Mut, World};

/// The thread pool parallel systems and parallel queries run on (one thread per logical CPU).
pub fn pool() -> &'static workpool::ThreadPool {
    static POOL: OnceLock<workpool::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        workpool::ThreadPool::builder().threads(n).name("tessera").build().expect("thread pool")
    })
}

pub struct SystemMeta {
    pub name: String,
    /// One access per parameter (queries are compared one by one, so disjoint queries share).
    pub access: Vec<Access>,
    pub last_run: u32,
}

/// Something a system can ask for.
///
/// # Safety
/// `init` must record every access `get` uses.
pub unsafe trait SystemParam {
    type State: Send + Sync + 'static;
    type Item<'w, 's>;
    fn init(world: &mut World, meta: &mut SystemMeta) -> Self::State;
    /// # Safety
    /// The caller must make sure no other running system's access conflicts with this one's.
    unsafe fn get<'w, 's>(state: &'s mut Self::State, world: &'w World, ticks: Ticks) -> Self::Item<'w, 's>;
    /// Runs after the system with exclusive access (applies deferred commands).
    fn apply(_state: &mut Self::State, _world: &mut World) {}
}

fn add_access(meta: &mut SystemMeta, a: Access, what: &str) {
    for prev in &meta.access {
        if prev.conflicts(&a) {
            panic!(
                "system `{}`: {what} conflicts with another of its parameters; combine them into one query or make them disjoint with With/Without",
                meta.name
            );
        }
    }
    meta.access.push(a);
}

// ---- Query -----------------------------------------------------------------------------------

/// A query as a system parameter.
pub struct Query<'w, 's, Q: QueryData, F: QueryFilter = ()> {
    world: &'w World,
    state: &'s QueryState<Q, F>,
    ticks: Ticks,
}

struct SendPtr<T>(T);
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

impl<'w, 's, Q: QueryData, F: QueryFilter> Query<'w, 's, Q, F> {
    pub fn iter(&self) -> QueryIter<'_, 's, Q, F>
    where
        Q: ReadOnly,
    {
        unsafe { self.state.iter_unchecked(self.world, self.ticks) }
    }

    pub fn iter_mut(&mut self) -> QueryIter<'_, 's, Q, F> {
        unsafe { self.state.iter_unchecked(self.world, self.ticks) }
    }

    pub fn get(&self, e: Entity) -> Option<Q::Item<'_>>
    where
        Q: ReadOnly,
    {
        unsafe { self.state.get_unchecked(self.world, e, self.ticks) }
    }

    pub fn get_mut(&mut self, e: Entity) -> Option<Q::Item<'_>> {
        unsafe { self.state.get_unchecked(self.world, e, self.ticks) }
    }

    /// The only match. Panics when there are none or several.
    pub fn single(&self) -> Q::Item<'_>
    where
        Q: ReadOnly,
    {
        let mut it = self.iter();
        let one = it.next().expect("Query::single: no entity matches");
        assert!(it.next().is_none(), "Query::single: more than one entity matches");
        one
    }

    pub fn is_empty(&self) -> bool {
        unsafe { self.state.iter_unchecked(self.world, self.ticks) }.next().is_none()
    }

    /// Calls `f` for every match, in parallel batches of `batch` rows on the shared pool.
    pub fn par_for_each_mut<FN>(&mut self, batch: usize, f: FN)
    where
        FN: for<'a> Fn(Q::Item<'a>) + Send + Sync,
    {
        let (q, fs, matched) = self.state.parts();
        let world = self.world;
        let ticks = self.ticks;
        let batch = batch.max(1);
        let f = &f;
        pool().handle().scope(|s| {
            for &a in matched {
                let arch = &world.archetypes[a as usize];
                let len = arch.len();
                let mut start = 0;
                while start < len {
                    let end = (start + batch).min(len);
                    // Each task gets its own row range, so mutable items never alias.
                    let fetch = SendPtr(unsafe { (Q::fetch(q, arch, ticks), F::fetch(fs, arch, ticks)) });
                    s.spawn(move || {
                        let fetch = fetch;
                        let (mut qf, mut ff) = fetch.0;
                        for r in start..end {
                            if F::DENSE || unsafe { F::filter(&mut ff, r) } {
                                f(unsafe { Q::item(&mut qf, r) });
                            }
                        }
                    });
                    start = end;
                }
            }
        });
    }
}

unsafe impl<Q: QueryData + 'static, F: QueryFilter + 'static> SystemParam for Query<'_, '_, Q, F> {
    type State = QueryState<Q, F>;
    type Item<'w, 's> = Query<'w, 's, Q, F>;
    fn init(world: &mut World, meta: &mut SystemMeta) -> QueryState<Q, F> {
        let s = QueryState::new(world);
        add_access(meta, s.access().clone(), &format!("Query<{}>", std::any::type_name::<Q>()));
        s
    }
    unsafe fn get<'w, 's>(state: &'s mut QueryState<Q, F>, world: &'w World, ticks: Ticks) -> Query<'w, 's, Q, F> {
        state.update(world);
        Query { world, state, ticks }
    }
}

// QueryState must be shareable across the scheduler's threads.
unsafe impl<Q: QueryData, F: QueryFilter> Send for QueryState<Q, F> {}
unsafe impl<Q: QueryData, F: QueryFilter> Sync for QueryState<Q, F> {}

// ---- resources -------------------------------------------------------------------------------

pub struct Res<'w, T> {
    value: &'w T,
    added: u32,
    changed: u32,
    ticks: Ticks,
}

impl<T> Res<'_, T> {
    /// Inserted since this system last ran.
    pub fn is_added(&self) -> bool {
        crate::world::is_newer(self.added, self.ticks.last_run, self.ticks.this_run)
    }

    /// Inserted or written since this system last ran.
    pub fn is_changed(&self) -> bool {
        crate::world::is_newer(self.changed, self.ticks.last_run, self.ticks.this_run)
    }
}

impl<T> Deref for Res<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}

unsafe impl<T: Send + Sync + 'static> SystemParam for Res<'_, T> {
    type State = ();
    type Item<'w, 's> = Res<'w, T>;
    fn init(world: &mut World, meta: &mut SystemMeta) {
        let _ = world;
        let mut a = Access::default();
        a.res_reads.insert(TypeIdKey(TypeId::of::<T>()));
        add_access(meta, a, &format!("Res<{}>", std::any::type_name::<T>()));
    }
    unsafe fn get<'w, 's>(_: &'s mut Self::State, world: &'w World, ticks: Ticks) -> Self::Item<'w, 's> {
        let slot = world
            .resources
            .get(&TypeId::of::<T>())
            .unwrap_or_else(|| panic!("resource {} is missing", std::any::type_name::<T>()));
        unsafe {
            Res {
                value: (*slot.value.get()).downcast_ref::<T>().unwrap(),
                added: slot.added,
                changed: *slot.changed.get(),
                ticks,
            }
        }
    }
}

pub struct ResMut<'w, T> {
    inner: Mut<'w, T>,
    added: u32,
    last_run: u32,
}

impl<T> ResMut<'_, T> {
    pub fn is_added(&self) -> bool {
        crate::world::is_newer(self.added, self.last_run, self.inner.tick)
    }

    pub fn is_changed(&self) -> bool {
        crate::world::is_newer(*self.inner.changed, self.last_run, self.inner.tick)
    }
}

impl<T> Deref for ResMut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> DerefMut for ResMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

unsafe impl<T: Send + Sync + 'static> SystemParam for ResMut<'_, T> {
    type State = ();
    type Item<'w, 's> = ResMut<'w, T>;
    fn init(world: &mut World, meta: &mut SystemMeta) {
        let _ = world;
        let mut a = Access::default();
        a.res_writes.insert(TypeIdKey(TypeId::of::<T>()));
        add_access(meta, a, &format!("ResMut<{}>", std::any::type_name::<T>()));
    }
    unsafe fn get<'w, 's>(_: &'s mut Self::State, world: &'w World, ticks: Ticks) -> Self::Item<'w, 's> {
        let slot = world
            .resources
            .get(&TypeId::of::<T>())
            .unwrap_or_else(|| panic!("resource {} is missing", std::any::type_name::<T>()));
        unsafe {
            let value = (*slot.value.get()).downcast_mut::<T>().unwrap();
            ResMut {
                inner: Mut { value, changed: &mut *slot.changed.get(), tick: ticks.this_run },
                added: slot.added,
                last_run: ticks.last_run,
            }
        }
    }
}

// ---- commands and locals ---------------------------------------------------------------------

type Command = Box<dyn FnOnce(&mut World) + Send + Sync>;

#[derive(Default)]
pub struct CommandQueue(Vec<Command>);

/// Structural changes from inside a system (spawn, despawn, insert, remove), applied when the
/// schedule has run every system.
pub struct Commands<'s> {
    queue: &'s mut CommandQueue,
}

impl Commands<'_> {
    pub fn spawn<B: Bundle + Sync>(&mut self, b: B) {
        self.queue.0.push(Box::new(move |w| {
            w.spawn(b);
        }));
    }

    pub fn despawn(&mut self, e: Entity) {
        self.queue.0.push(Box::new(move |w| {
            w.despawn(e);
        }));
    }

    pub fn insert<B: Bundle + Sync>(&mut self, e: Entity, b: B) {
        self.queue.0.push(Box::new(move |w| {
            w.insert(e, b);
        }));
    }

    pub fn remove<T: Component>(&mut self, e: Entity) {
        self.queue.0.push(Box::new(move |w| {
            w.remove::<T>(e);
        }));
    }

    /// Any change to the world.
    pub fn add(&mut self, f: impl FnOnce(&mut World) + Send + Sync + 'static) {
        self.queue.0.push(Box::new(f));
    }
}

unsafe impl SystemParam for Commands<'_> {
    type State = CommandQueue;
    type Item<'w, 's> = Commands<'s>;
    fn init(_: &mut World, _: &mut SystemMeta) -> CommandQueue {
        CommandQueue::default()
    }
    unsafe fn get<'w, 's>(state: &'s mut Self::State, _: &'w World, _: Ticks) -> Self::Item<'w, 's> {
        Commands { queue: state }
    }
    fn apply(state: &mut CommandQueue, world: &mut World) {
        for c in state.0.drain(..) {
            c(world);
        }
    }
}

/// Per-system state that persists between runs.
pub struct Local<'s, T>(&'s mut T);

impl<T> Deref for Local<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0
    }
}

impl<T> DerefMut for Local<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.0
    }
}

unsafe impl<T: Default + Send + Sync + 'static> SystemParam for Local<'_, T> {
    type State = T;
    type Item<'w, 's> = Local<'s, T>;
    fn init(_: &mut World, _: &mut SystemMeta) -> T {
        T::default()
    }
    unsafe fn get<'w, 's>(state: &'s mut Self::State, _: &'w World, _: Ticks) -> Self::Item<'w, 's> {
        Local(state)
    }
}

// ---- systems ---------------------------------------------------------------------------------

pub trait System: Send + Sync {
    fn name(&self) -> &str;
    fn init(&mut self, world: &mut World);
    fn access(&self) -> &[Access];
    fn is_exclusive(&self) -> bool {
        false
    }
    /// # Safety
    /// No running system may conflict with this one; exclusive systems must not use this.
    unsafe fn run_unsafe(&mut self, world: &World);
    fn run_exclusive(&mut self, _world: &mut World) {}
    fn apply(&mut self, world: &mut World);
}

pub trait IntoSystem<Marker> {
    fn into_system(self) -> Box<dyn System>;
}

pub struct FunctionSystem<F, P: SystemParam> {
    f: F,
    state: Option<P::State>,
    meta: SystemMeta,
    _p: PhantomData<fn() -> P>,
}

macro_rules! params {
    ($($p:ident $i:tt),*) => {
        #[allow(unused_variables, unused_unsafe, clippy::unused_unit)]
        unsafe impl<$($p: SystemParam),*> SystemParam for ($($p,)*) {
            type State = ($($p::State,)*);
            type Item<'w, 's> = ($($p::Item<'w, 's>,)*);
            fn init(world: &mut World, meta: &mut SystemMeta) -> Self::State {
                ($($p::init(world, meta),)*)
            }
            unsafe fn get<'w, 's>(state: &'s mut Self::State, world: &'w World, ticks: Ticks) -> Self::Item<'w, 's> {
                unsafe { ($($p::get(&mut state.$i, world, ticks),)*) }
            }
            fn apply(state: &mut Self::State, world: &mut World) {
                $($p::apply(&mut state.$i, world);)*
            }
        }

        #[allow(non_snake_case)]
        impl<Func, $($p: SystemParam + 'static),*> IntoSystem<fn($($p,)*)> for Func
        where
            Func: Send + Sync + 'static,
            for<'a> &'a mut Func: FnMut($($p,)*) + FnMut($($p::Item<'_, '_>,)*),
        {
            fn into_system(self) -> Box<dyn System> {
                Box::new(FunctionSystem::<Func, ($($p,)*)> {
                    f: self,
                    state: None,
                    meta: SystemMeta { name: std::any::type_name::<Func>().to_string(), access: Vec::new(), last_run: 0 },
                    _p: PhantomData,
                })
            }
        }

        #[allow(non_snake_case, unused_variables, unused_mut)]
        impl<Func, $($p: SystemParam + 'static),*> System for FunctionSystem<Func, ($($p,)*)>
        where
            Func: Send + Sync + 'static,
            for<'a> &'a mut Func: FnMut($($p,)*) + FnMut($($p::Item<'_, '_>,)*),
        {
            fn name(&self) -> &str {
                &self.meta.name
            }
            fn init(&mut self, world: &mut World) {
                if self.state.is_none() {
                    self.state = Some(<($($p,)*) as SystemParam>::init(world, &mut self.meta));
                }
            }
            fn access(&self) -> &[Access] {
                &self.meta.access
            }
            unsafe fn run_unsafe(&mut self, world: &World) {
                // Calls through a helper so the compiler sees one concrete FnMut signature.
                #[allow(clippy::too_many_arguments)]
                fn call<$($p),*>(mut f: impl FnMut($($p),*), $($p: $p),*) {
                    f($($p),*)
                }
                let ticks = Ticks { last_run: self.meta.last_run, this_run: world.increment_change_tick() };
                let state = self.state.as_mut().expect("system not initialized");
                let ($($p,)*) = unsafe { <($($p,)*) as SystemParam>::get(state, world, ticks) };
                call(&mut self.f, $($p),*);
                self.meta.last_run = ticks.this_run;
            }
            fn apply(&mut self, world: &mut World) {
                if let Some(s) = self.state.as_mut() {
                    <($($p,)*) as SystemParam>::apply(s, world);
                }
            }
        }
    };
}

params!();
params!(P0 0);
params!(P0 0, P1 1);
params!(P0 0, P1 1, P2 2);
params!(P0 0, P1 1, P2 2, P3 3);
params!(P0 0, P1 1, P2 2, P3 3, P4 4);
params!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5);
params!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6);
params!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7);

/// A system that takes the whole world (`fn(&mut World)`); it runs alone.
pub struct ExclusiveSystem<F> {
    f: F,
    name: String,
    access: Vec<Access>,
}

pub struct ExclusiveMarker;

impl<F: FnMut(&mut World) + Send + Sync + 'static> IntoSystem<ExclusiveMarker> for F {
    fn into_system(self) -> Box<dyn System> {
        Box::new(ExclusiveSystem {
            f: self,
            name: std::any::type_name::<F>().to_string(),
            access: vec![Access { exclusive: true, ..Default::default() }],
        })
    }
}

impl<F: FnMut(&mut World) + Send + Sync + 'static> System for ExclusiveSystem<F> {
    fn name(&self) -> &str {
        &self.name
    }
    fn init(&mut self, _: &mut World) {}
    fn access(&self) -> &[Access] {
        &self.access
    }
    fn is_exclusive(&self) -> bool {
        true
    }
    unsafe fn run_unsafe(&mut self, _: &World) {
        panic!("an exclusive system needs &mut World");
    }
    fn run_exclusive(&mut self, world: &mut World) {
        (self.f)(world)
    }
    fn apply(&mut self, _: &mut World) {}
}

/// Whether two systems may run at the same time.
pub fn systems_conflict(a: &dyn System, b: &dyn System) -> bool {
    if a.is_exclusive() || b.is_exclusive() {
        return true;
    }
    a.access().iter().any(|x| b.access().iter().any(|y| x.conflicts(y)))
}
